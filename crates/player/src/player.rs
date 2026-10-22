//! The built-in player's [`Playback`] (H15-47): the protocol side's commands go to the
//! [`Decoder`] (H15-56) and the [`Output`]; their events come back as one `STAT` status.

use std::fmt;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use slimproto::{Playback, PlaybackStatus, Track};
use tokio::sync::mpsc;

use crate::decode::{Decoder, DecoderConfig};
use crate::output::{Device, Output, Start};

/// The rate the output threshold is counted in before the track's own rate is known.
const THRESHOLD_RATE: f64 = 44_100.0;

/// What the server's volume (`audg`) does: on the board it sets the output's PipeWire volume,
/// not the samples, so the DSP and the headset get the full resolution.
pub type VolumeHandler = Box<dyn Fn(f32, f32, u32) + Send + Sync>;

/// Decoder plus output, as the protocol side sees them.
pub struct Player {
    decoder: Decoder,
    output: Output,
    on_volume: VolumeHandler,
}

impl fmt::Debug for Player {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Player")
            .field("decoder", &self.decoder)
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

impl Player {
    /// Starts the decoder and output threads; `buffer` is the output buffer (seconds of audio).
    pub fn new(device: impl Device, buffer: Duration, on_volume: VolumeHandler) -> io::Result<Arc<Self>> {
        let (output, sink) = Output::spawn(device, buffer)?;
        let decoder = Decoder::spawn(sink, DecoderConfig::default())?;
        Ok(Arc::new(Self {
            decoder,
            output,
            on_volume,
        }))
    }
}

impl Drop for Player {
    /// Shuts the output down first: a decoder blocked on a full buffer (nothing playing) then
    /// gets `Closed` and its thread can be joined when the decoder drops (H15-60).
    fn drop(&mut self) {
        self.output.shutdown();
    }
}

impl Playback for Player {
    fn start(&self, track: Track, source: mpsc::Receiver<Bytes>) {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "frames of audio"
        )]
        let threshold_frames = (track.output_threshold.as_secs_f64() * THRESHOLD_RATE) as usize;
        self.output.prepare(Start {
            threshold_frames,
            autostart: track.autostart,
        });
        let _id = self.decoder.start(track, source);
    }

    fn pause(&self) {
        self.output.pause();
    }

    fn unpause(&self, at: Option<Instant>) {
        self.output.unpause(at);
    }

    fn stop(&self) {
        self.decoder.stop();
        self.output.stop();
    }

    fn flush_next(&self) {
        self.decoder.flush_next(self.output.playing());
        self.output.flush_next();
    }

    fn skip(&self, ahead: Duration) {
        self.output.skip(ahead);
    }

    fn volume(&self, left: f32, right: f32, percent: u32) {
        (self.on_volume)(left, right, percent);
    }

    fn enable(&self, on: bool) {
        self.output.enable(on);
    }

    fn status(&self) -> PlaybackStatus {
        let output = self.output.status();
        let (decode_fill, decode_size) = self.decoder.input_fill();
        let mut events = self.decoder.take_events();
        events.extend(output.events);
        PlaybackStatus {
            elapsed: output.elapsed,
            output_fill: output.fill,
            output_size: output.size,
            decode_fill,
            decode_size,
            paused: output.paused,
            events,
            sample_rate: output.sample_rate,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use slimproto::{Format, PlaybackEvent};

    use super::*;

    /// Counts the bytes it gets.
    #[derive(Debug, Default, Clone)]
    struct Fake(Arc<Mutex<(bool, usize)>>);

    impl Device for Fake {
        fn open(&mut self, _rate: u32, _channels: u16) -> io::Result<()> {
            self.0.lock().unwrap().0 = true;
            Ok(())
        }
        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            std::thread::sleep(Duration::from_millis(1));
            if bytes.iter().any(|b| *b != 0) {
                self.0.lock().unwrap().1 += bytes.len();
            }
            Ok(())
        }
        fn delay_frames(&self) -> u32 {
            0
        }
        fn close(&mut self) {
            self.0.lock().unwrap().0 = false;
        }
        fn is_open(&self) -> bool {
            self.0.lock().unwrap().0
        }
    }

    #[test]
    fn plays_a_pcm_track_from_start_to_done() {
        let fake = Fake::default();
        let volume = Arc::new(Mutex::new(None));
        let seen_volume = Arc::clone(&volume);
        let player = Player::new(
            fake.clone(),
            Duration::from_secs(1),
            Box::new(move |l, r, p| *seen_volume.lock().unwrap() = Some((l, r, p))),
        )
        .unwrap();
        let (tx, rx) = mpsc::channel(4);
        player.start(
            Track {
                format: Format::Pcm {
                    sample_rate: Some(44_100),
                    sample_size: Some(16),
                    channels: Some(2),
                    big_endian: Some(false),
                },
                replay_gain: 1.0,
                autostart: true,
                output_threshold: Duration::from_millis(10),
                transition: slimproto::Transition::default(),
            },
            rx,
        );
        // 2000 frames of 16-bit stereo, not silent.
        tx.blocking_send(Bytes::from(vec![0x10u8; 2000 * 4])).unwrap();
        drop(tx);
        player.volume(1.0, 1.0, 42);
        let mut events = Vec::new();
        for _ in 0..3000 {
            events.extend(player.status().events);
            if events.contains(&PlaybackEvent::PlaybackDone) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        for expected in [
            PlaybackEvent::TrackStarted,
            PlaybackEvent::DecodeDone,
            PlaybackEvent::PlaybackDone,
        ] {
            assert!(events.contains(&expected), "no {expected:?} in {events:?}");
        }
        assert_eq!(fake.0.lock().unwrap().1, 2000 * 2 * 4, "every frame as f32");
        assert_eq!(*volume.lock().unwrap(), Some((1.0, 1.0, 42)));
    }

    #[test]
    fn drops_while_the_decoder_waits_on_a_full_buffer() {
        let player = Player::new(Fake::default(), Duration::from_millis(10), Box::new(|_, _, _| {})).unwrap();
        let (tx, rx) = mpsc::channel(4);
        player.start(
            Track {
                format: Format::Pcm {
                    sample_rate: Some(44_100),
                    sample_size: Some(16),
                    channels: Some(2),
                    big_endian: Some(false),
                },
                replay_gain: 1.0,
                autostart: false,
                output_threshold: Duration::from_secs(10),
                transition: slimproto::Transition::default(),
            },
            rx,
        );
        tx.blocking_send(Bytes::from(vec![0x10u8; 44_100 * 4])).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(player);
            done_tx.send(()).unwrap();
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "dropping the player hung"
        );
    }
}

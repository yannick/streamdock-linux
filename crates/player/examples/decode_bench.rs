//! Decode CPU benchmark (H15-56): decodes a file through [`Decoder`] into a sink that drops the
//! frames, and reports the process CPU time against the audio's duration.
//!
//! ```sh
//! crates/player/examples/bench-input.sh /tmp/bench.flac        # 3 min, 44.1 kHz stereo
//! cargo run --release -p player --example decode_bench -- /tmp/bench.flac
//! ```
//!
//! Formats by extension: `.flac`, `.mp3`, `.wav` / `.aif` (header), anything else raw 16-bit LE
//! stereo 44.1 kHz.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use player::{Closed, Decoder, DecoderConfig, FrameSink, TrackId};
use slimproto::{Format, PlaybackEvent, Track};
use tokio::sync::mpsc;

#[derive(Debug, Default, Clone, Copy)]
struct Count {
    rate: u32,
    channels: u16,
    samples: u64,
}

struct Null(Arc<Mutex<Count>>);

impl FrameSink for Null {
    fn begin_track(&mut self, _: TrackId, rate: u32, channels: u16) {
        let mut c = self.0.lock().unwrap();
        c.rate = rate;
        c.channels = channels;
    }

    fn write(&mut self, frames: &[f32]) -> Result<(), Closed> {
        self.0.lock().unwrap().samples += frames.len() as u64;
        Ok(())
    }
}

/// CPU time (user + system) of this process, from `/proc/self/stat` (clock ticks of 10 ms).
fn cpu_time() -> Duration {
    let stat = std::fs::read_to_string("/proc/self/stat").expect("/proc/self/stat");
    // Fields after the parenthesised command name; utime and stime are fields 14 and 15.
    let rest = &stat[stat.rfind(')').expect("stat format") + 2..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    Duration::from_millis(ticks * 10)
}

fn format_for(path: &str) -> Format {
    let header = Format::Pcm {
        sample_rate: None,
        sample_size: None,
        channels: None,
        big_endian: None,
    };
    match path.rsplit('.').next().unwrap_or_default() {
        "flac" => Format::Flac,
        "mp3" => Format::Mp3,
        "wav" | "aif" | "aiff" => header,
        _ => Format::Pcm {
            sample_rate: Some(44_100),
            sample_size: Some(16),
            channels: Some(2),
            big_endian: Some(false),
        },
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: decode_bench <file>");
    let data = Bytes::from(std::fs::read(&path).expect("read input"));
    let count = Arc::new(Mutex::new(Count::default()));
    let decoder = Decoder::spawn(Null(Arc::clone(&count)), DecoderConfig::default()).unwrap();

    // The body arrives in 16 KiB chunks, as from the HTTP fetch.
    let (tx, rx) = mpsc::channel(64);
    let feeder = std::thread::spawn(move || {
        let mut at = 0;
        while at < data.len() {
            let end = (at + 16 * 1024).min(data.len());
            if tx.blocking_send(data.slice(at..end)).is_err() {
                break;
            }
            at = end;
        }
    });

    let cpu0 = cpu_time();
    let wall0 = Instant::now();
    let track = Track {
        format: format_for(&path),
        replay_gain: 1.0,
        autostart: true,
        output_threshold: Duration::ZERO,
        transition: slimproto::Transition::default(),
    };
    let _ = decoder.start(track, rx);
    let event = loop {
        if let Some(e) = decoder.take_events().into_iter().next() {
            break e;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let wall = wall0.elapsed();
    let cpu = cpu_time().saturating_sub(cpu0);
    feeder.join().unwrap();

    let c = *count.lock().unwrap();
    assert_eq!(event, PlaybackEvent::DecodeDone, "decode failed");
    #[allow(clippy::cast_precision_loss)] // H15-56: durations for a report
    let audio = c.samples as f64 / f64::from(c.channels.max(1)) / f64::from(c.rate.max(1));
    println!(
        "{path}: {audio:.1} s audio ({} Hz × {}), decode wall {:.3} s, CPU {:.2} s = {:.2} % of one core in real time ({:.0}× real time)",
        c.rate,
        c.channels,
        wall.as_secs_f64(),
        cpu.as_secs_f64(),
        cpu.as_secs_f64() / audio * 100.0,
        audio / wall.as_secs_f64(),
    );
}

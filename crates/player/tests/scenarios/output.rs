use std::time::{Duration, Instant};

use player::output::{Output, OutputSink, Start};
use player::{Closed, FrameSink as _, TrackId};
use slimproto::PlaybackEvent;

use crate::support::device::FakeDevice;
use crate::support::lms::TIMEOUT;
use crate::support::signals::{CHANNELS, RATE};

struct Shutdown(Output);
impl Drop for Shutdown {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

fn fixture(buffer: Duration) -> (Output, OutputSink, FakeDevice, Shutdown) {
    let device = FakeDevice::default();
    let (output, sink) = Output::spawn(device.clone(), buffer).unwrap();
    let shutdown = Shutdown(output.clone());
    (output, sink, device, shutdown)
}

fn track(sink: &mut OutputSink, id: u64, frames: usize, value: f32) {
    sink.begin_track(TrackId::from_raw(id), RATE, CHANNELS);
    sink.write(&vec![value; frames * usize::from(CHANNELS)]).unwrap();
    sink.end_track();
}

fn wait_event(output: &Output, wanted: &PlaybackEvent) -> Vec<PlaybackEvent> {
    let deadline = Instant::now() + TIMEOUT;
    let mut events = Vec::new();
    loop {
        events.extend(output.status().events);
        if events.contains(wanted) {
            return events;
        }
        assert!(Instant::now() < deadline, "missing {wanted:?}: {events:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn fake_device_gets_both_tracks_without_reopen_gaps_or_sample_loss() {
    let (output, mut sink, device, _shutdown) = fixture(Duration::from_secs(1));
    track(&mut sink, 1, 4410, 0.25);
    track(&mut sink, 2, 4410, -0.25);
    output.prepare(Start {
        threshold_frames: 100,
        autostart: false,
    });
    wait_event(&output, &PlaybackEvent::Buffered);
    assert!(output.status().paused);
    assert!(device.inspect(|log| log.formats.is_empty()));
    output.unpause(None);
    let events = wait_event(&output, &PlaybackEvent::PlaybackDone);
    assert_eq!(
        events
            .iter()
            .filter(|e| **e == PlaybackEvent::TrackStarted)
            .count(),
        2
    );
    assert!(!events.contains(&PlaybackEvent::OutputUnderrun));
    device.wait(|log| log.samples.len() == 17_640);
    device.inspect(|log| {
        assert_eq!(log.formats, vec![(RATE, CHANNELS)]);
        assert!(log.silence_at.iter().all(|at| *at == 0 || *at == 17_640));
        assert!(
            log.samples[..8820]
                .iter()
                .all(|v| (*v - 0.25).abs() < f32::EPSILON)
        );
        assert!(
            log.samples[8820..]
                .iter()
                .all(|v| (*v + 0.25).abs() < f32::EPSILON)
        );
    });
    assert_eq!(output.status().elapsed, Duration::from_millis(100));
}

#[test]
fn pause_holds_audio_and_elapsed_while_device_receives_silence() {
    let (output, mut sink, device, _shutdown) = fixture(Duration::from_secs(2));
    track(&mut sink, 1, 44_100, 0.25);
    output.prepare(Start {
        threshold_frames: 100,
        autostart: true,
    });
    device.wait(|log| log.samples.len() >= 882);
    output.pause();
    device.wait(|log| log.silence_writes >= 3);
    let count = device.inspect(|log| log.samples.len());
    let elapsed = output.status().elapsed;
    device.wait(|log| log.silence_writes >= 6);
    assert_eq!(device.inspect(|log| log.samples.len()), count);
    assert_eq!(output.status().elapsed, elapsed);
    assert!(output.status().paused);
    output.unpause(None);
    wait_event(&output, &PlaybackEvent::PlaybackDone);
    device.wait(|log| log.samples.len() == 88_200);
}

#[test]
fn flush_next_preserves_current_track_and_cancels_its_old_writer() {
    let (output, mut sink, device, _shutdown) = fixture(Duration::from_secs(1));
    track(&mut sink, 1, 4410, 0.25);
    track(&mut sink, 2, 4410, -0.25);
    output.flush_next();
    assert_eq!(sink.write(&[0.75; 2]), Err(Closed));
    output.prepare(Start {
        threshold_frames: 100,
        autostart: true,
    });
    let events = wait_event(&output, &PlaybackEvent::PlaybackDone);
    assert_eq!(
        events
            .iter()
            .filter(|e| **e == PlaybackEvent::TrackStarted)
            .count(),
        1
    );
    device.wait(|log| log.samples.len() == 8820);
    assert!(device.inspect(|log| log.samples.iter().all(|v| (*v - 0.25).abs() < f32::EPSILON)));
}

#[test]
fn stop_releases_a_blocked_decoder_and_resets_elapsed() {
    let (output, mut sink, _device, _shutdown) = fixture(Duration::from_millis(10));
    sink.begin_track(TrackId::from_raw(1), RATE, CHANNELS);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        done_tx.send(sink.write(&vec![0.25; 88_200])).unwrap();
    });
    let deadline = Instant::now() + TIMEOUT;
    while output.status().fill < output.status().size {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    output.stop();
    assert_eq!(done_rx.recv_timeout(TIMEOUT).unwrap(), Err(Closed));
    writer.join().unwrap();
    assert_eq!(output.status().elapsed, Duration::ZERO);
}

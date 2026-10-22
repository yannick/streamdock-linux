//! Plays a quiet sine through the output into a PipeWire node, for measuring the output on the
//! board: `tone <node id> [seconds] [rate] [level, default 0: silent]`. Prints the status every second.

use std::sync::Arc;
use std::time::Duration;

use player::FrameSink;
use player::output::{Output, PwCat, Start};

#[allow(clippy::cast_possible_truncation, reason = "a test tone")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let node: Option<u32> = args.get(1).and_then(|a| a.parse().ok());
    let seconds: u32 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(10);
    let rate: u32 = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(44_100);
    let level: f32 = args.get(4).and_then(|a| a.parse().ok()).unwrap_or(0.0);
    let device = PwCat::new(
        "pw-cat",
        Vec::new(),
        Arc::new(move || node),
        Duration::from_millis(100),
    );
    let (output, mut sink) = Output::spawn(device, Duration::from_secs(4)).expect("output");
    output.prepare(Start {
        threshold_frames: rate as usize / 2,
        autostart: true,
    });
    let writer = std::thread::spawn(move || {
        sink.begin_track(player::TrackId::from_raw(0), rate, 2);
        let mut phase = 0.0f32;
        let step = 440.0 * std::f32::consts::TAU / f64::from(rate) as f32;
        let mut buf = vec![0.0f32; 2048];
        for _ in 0..(seconds * rate / 1024) {
            for frame in buf.as_chunks_mut::<2>().0 {
                let s = phase.sin() * level;
                frame[0] = s;
                frame[1] = s;
                phase = (phase + step) % std::f32::consts::TAU;
            }
            if sink.write(&buf).is_err() {
                return;
            }
        }
        sink.end_track();
    });
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let status = output.status();
        println!("{status:?}");
        if status.events.contains(&slimproto::PlaybackEvent::PlaybackDone) {
            break;
        }
    }
    writer.join().unwrap();
}

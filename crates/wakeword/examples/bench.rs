//! Runs a model over a 16 kHz mono 16-bit WAV (44-byte header) and reports detections and the CPU
//! time per second of audio (H15-68 measurements on the Luckfox):
//! `bench <model.tflite> <cutoff> <file.wav> [repeat]`.

use std::time::Instant;

use wakeword::{Detector, Model};

#[allow(clippy::cast_precision_loss, reason = "timing arithmetic")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, model, cutoff, wav, rest @ ..] = args.as_slice() else {
        eprintln!("usage: bench <model.tflite> <cutoff> <file.wav> [repeat]");
        std::process::exit(2);
    };
    let repeat: usize = rest.first().map_or(1, |r| r.parse().expect("repeat"));
    let model = Model::new(&std::fs::read(model).expect("model")).expect("valid model");
    let bytes = std::fs::read(wav).expect("wav");
    let samples: Vec<i16> = bytes[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect();
    let mut detector = Detector::new(model, cutoff.parse().expect("cutoff"), 5).expect("detector");
    let started = Instant::now();
    let mut detections = Vec::new();
    for round in 0..repeat {
        detector.reset();
        let mut at = 0;
        for chunk in samples.chunks(320) {
            for score in detector.push(chunk).expect("push") {
                if score.detected && round == 0 {
                    detections.push(at as f64 / 16000.0);
                }
            }
            at += chunk.len();
        }
    }
    let wall = started.elapsed().as_secs_f64();
    // This process's own CPU time (user + system), not wall time: the board runs other work.
    let cpu = cpu_seconds().unwrap_or(wall);
    let audio = samples.len() as f64 / 16000.0 * repeat as f64;
    println!("detections at {detections:?} s");
    println!(
        "{audio:.1} s of audio: {cpu:.2} s CPU ({wall:.2} s wall), {:.2} % of one core",
        cpu / audio * 100.0
    );
}

/// utime + stime from `/proc/self/stat`, at 100 ticks per second.
fn cpu_seconds() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let ticks: f64 = fields.get(11)?.parse::<f64>().ok()? + fields.get(12)?.parse::<f64>().ok()?;
    Some(ticks / 100.0)
}

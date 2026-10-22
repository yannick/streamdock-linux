//! Prints the detection times of a model on WAV files (16 kHz mono 16-bit, 44-byte header), one
//! line per file: `<file>\t<seconds,…>` (H15-68 evaluation of false accepts/rejects and latency).
//! `eval <model.tflite> <cutoff> <window> <file.wav>…`

use wakeword::{Detector, Model};

#[allow(clippy::cast_precision_loss, reason = "seconds")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, model, cutoff, window, files @ ..] = args.as_slice() else {
        eprintln!("usage: eval <model.tflite> <cutoff> <window> <file.wav>…");
        std::process::exit(2);
    };
    let model = Model::new(&std::fs::read(model).expect("model")).expect("valid model");
    let mut detector = Detector::new(
        model,
        cutoff.parse().expect("cutoff"),
        window.parse().expect("window"),
    )
    .expect("detector");
    for file in files {
        let bytes = std::fs::read(file).expect("wav");
        let samples: Vec<i16> = bytes[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        detector.reset();
        let mut times = Vec::new();
        let mut fed = 0;
        for chunk in samples.chunks(320) {
            fed += chunk.len();
            for s in detector.push(chunk).expect("push") {
                if s.detected {
                    times.push(format!("{:.3}", fed as f64 / 16000.0));
                }
            }
        }
        println!("{file}\t{}", times.join(","));
    }
}

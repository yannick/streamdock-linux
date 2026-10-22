//! Times the frontend and the model separately (H15-68 profiling).
use std::time::Instant;
use wakeword::{Model, frontend::Frontend};

#[allow(clippy::cast_precision_loss, reason = "timing")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&args[2]).unwrap();
    let samples: Vec<i16> = bytes[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect();
    let repeat = 10;
    let mut frontend = Frontend::new();
    let mut features = Vec::new();
    let t = Instant::now();
    for _ in 0..repeat {
        features.clear();
        for c in samples.as_chunks::<160>().0 {
            if let (Some(f), _) = frontend.process(c) {
                features.push(f);
            }
        }
    }
    let fe = t.elapsed().as_secs_f64();
    let mut model = Model::new(&std::fs::read(&args[1]).unwrap()).unwrap();
    let input: Vec<Vec<i8>> = features
        .as_chunks::<3>()
        .0
        .iter()
        .map(|s| s.iter().flatten().map(|&v| model.quantize(v)).collect())
        .collect();
    let t = Instant::now();
    for _ in 0..repeat {
        model.reset();
        for i in &input {
            model.invoke(i).unwrap();
        }
    }
    let m = t.elapsed().as_secs_f64();
    let audio = samples.len() as f64 / 16000.0 * f64::from(repeat);
    println!(
        "frontend {:.2} %  model {:.2} % of one core (wall)",
        fe / audio * 100.0,
        m / audio * 100.0
    );
}

//! Detection on the TTS fixtures (H15-68): each wake word once in its phrase, never elsewhere,
//! however the audio is chunked.

mod common;

use wakeword::{Detector, Model};

fn detector(word: &str, cutoff: f32) -> Detector {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("models/{word}.tflite"));
    Detector::new(Model::new(&std::fs::read(path).unwrap()).unwrap(), cutoff, 5).unwrap()
}

/// Seconds at which `word` was detected in `wav`, fed in chunks of `chunk` samples.
fn detections(word: &str, cutoff: f32, wav: &str, chunk: usize) -> Vec<f32> {
    let mut d = detector(word, cutoff);
    let samples = common::wav(wav);
    let mut out = Vec::new();
    let mut fed = 0;
    for c in samples.chunks(chunk) {
        fed += c.len();
        for s in d.push(c).unwrap() {
            if s.detected {
                #[allow(clippy::cast_precision_loss, reason = "test timing")]
                out.push(fed as f32 / 16000.0);
            }
        }
    }
    out
}

#[test]
fn each_word_once_in_its_phrase() {
    let cases = [
        ("okay_nabu", 0.85, "okay_nabu_lights"),
        ("hey_jarvis", 0.97, "hey_jarvis_time"),
    ];
    for (word, cutoff, wav) in cases {
        let at = detections(word, cutoff, wav, 320);
        assert_eq!(at.len(), 1, "{word} in {wav}: {at:?}");
        // The phrase starts after 0.5 s of silence; the word is said within the next second.
        assert!((0.9..2.0).contains(&at[0]), "{word} at {}", at[0]);
    }
}

#[test]
fn no_false_accepts() {
    let cases = [
        ("okay_nabu", 0.85, "hey_jarvis_time"),
        ("okay_nabu", 0.85, "neg_similar"),
        ("hey_jarvis", 0.97, "okay_nabu_lights"),
        ("hey_jarvis", 0.97, "neg_similar"),
    ];
    for (word, cutoff, wav) in cases {
        assert_eq!(
            detections(word, cutoff, wav, 320),
            Vec::<f32>::new(),
            "{word} in {wav}"
        );
    }
}

#[test]
fn chunking_does_not_matter() {
    let reference = detections("okay_nabu", 0.85, "okay_nabu_lights", 160);
    for chunk in [1, 77, 1000, 4096] {
        let at = detections("okay_nabu", 0.85, "okay_nabu_lights", chunk);
        assert_eq!(at.len(), reference.len(), "chunk {chunk}");
        // Reported at the end of the chunk that completed the step.
        #[allow(clippy::cast_precision_loss, reason = "test timing")]
        let slack = chunk as f32 / 16000.0 + 0.011;
        assert!(
            at.iter().zip(&reference).all(|(a, b)| (a - b).abs() <= slack),
            "chunk {chunk}: {at:?} vs {reference:?}"
        );
    }
}

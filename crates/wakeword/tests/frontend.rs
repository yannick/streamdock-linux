//! The frontend's features equal the reference's bit for bit (H15-68).

mod common;

use wakeword::frontend::{Frontend, STEP};

fn features(name: &str) -> Vec<Vec<i64>> {
    let mut frontend = Frontend::new();
    common::wav(name)
        .as_chunks::<STEP>()
        .0
        .iter()
        .filter_map(|chunk| frontend.process(chunk).0)
        .map(|f| f.iter().map(|&v| i64::from(v)).collect())
        .collect()
}

#[test]
fn features_match_the_reference() {
    for name in ["okay_nabu_lights", "hey_jarvis_time", "neg_similar"] {
        let ours = features(name);
        let reference = common::rows(&format!("{name}.features"));
        assert_eq!(ours.len(), reference.len(), "{name}: number of steps");
        for (step, (a, b)) in ours.iter().zip(&reference).enumerate() {
            assert_eq!(a, b, "{name}: step {step}");
        }
    }
}

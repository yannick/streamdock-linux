//! The interpreter's outputs equal TensorFlow Lite Micro's (H15-68): the fixtures hold TFLM's raw
//! uint8 output per invocation on the reference frontend features.

mod common;

use wakeword::Model;

fn model(name: &str) -> Model {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("models/{name}.tflite"));
    Model::new(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn outputs_match_tflite_micro() {
    for word in ["okay_nabu", "hey_jarvis"] {
        for wav in ["okay_nabu_lights", "hey_jarvis_time", "neg_similar"] {
            let mut m = model(word);
            assert_eq!(m.stride(), 3);
            let features = common::rows(&format!("{wav}.features"));
            let expected: Vec<i64> = common::rows(&format!("{wav}.{word}"))
                .into_iter()
                .map(|r| r[0])
                .collect();
            let ours: Vec<i64> = features
                .as_chunks::<3>()
                .0
                .iter()
                .map(|steps| {
                    let input: Vec<i8> = steps
                        .iter()
                        .flatten()
                        .map(|&v| m.quantize(u16::try_from(v).unwrap()))
                        .collect();
                    i64::from(m.invoke(&input).unwrap())
                })
                .collect();
            assert_eq!(ours, expected, "{word} on {wav}");
        }
    }
}

#[test]
fn all_built_in_models_load() {
    for word in ["okay_nabu", "hey_jarvis", "alexa", "hey_mycroft"] {
        let m = model(word);
        assert_eq!(m.stride(), 3, "{word}");
    }
}

#[test]
fn garbage_is_an_error_not_a_panic() {
    let bytes =
        std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models/okay_nabu.tflite"))
            .unwrap();
    assert!(Model::new(&[]).is_err());
    assert!(Model::new(&[1, 2, 3, 4, 5, 6, 7, 8]).is_err());
    for cut in [100, 1000, bytes.len() / 2, bytes.len() - 1] {
        assert!(Model::new(&bytes[..cut]).is_err(), "cut at {cut}");
    }
    let mut flipped = bytes.clone();
    for i in (0..flipped.len()).step_by(97) {
        flipped[i] ^= 0x5a;
    }
    let _ = Model::new(&flipped);
}

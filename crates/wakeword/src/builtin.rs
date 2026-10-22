//! The built-in wake words: microWakeWord v2 models (Apache-2.0, see `models/README.md`), compiled
//! into the binary with their manifest values.

/// A built-in model and the detection settings its manifest gives.
#[derive(Debug, Clone, Copy)]
pub struct Builtin {
    /// Model id, as configured (`wake_word = "okay_nabu"`).
    pub id: &'static str,
    /// What to say.
    pub wake_word: &'static str,
    /// Languages it was trained for.
    pub languages: &'static [&'static str],
    /// `probability_cutoff`.
    pub cutoff: f32,
    /// `sliding_window_size`.
    pub window: usize,
    pub tflite: &'static [u8],
}

pub const MODELS: [Builtin; 4] = [
    Builtin {
        id: "okay_nabu",
        wake_word: "Okay Nabu",
        languages: &["en", "nl", "fr", "de", "it", "es", "sv"],
        cutoff: 0.85,
        window: 5,
        tflite: include_bytes!("../models/okay_nabu.tflite"),
    },
    Builtin {
        id: "hey_jarvis",
        wake_word: "Hey Jarvis",
        languages: &["en"],
        cutoff: 0.97,
        window: 5,
        tflite: include_bytes!("../models/hey_jarvis.tflite"),
    },
    Builtin {
        id: "alexa",
        wake_word: "Alexa",
        languages: &["en"],
        cutoff: 0.9,
        window: 5,
        tflite: include_bytes!("../models/alexa.tflite"),
    },
    Builtin {
        id: "hey_mycroft",
        wake_word: "Hey Mycroft",
        languages: &["en"],
        cutoff: 0.95,
        window: 5,
        tflite: include_bytes!("../models/hey_mycroft.tflite"),
    },
];

/// The built-in model `id`.
#[must_use]
pub fn find(id: &str) -> Option<&'static Builtin> {
    MODELS.iter().find(|m| m.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values above are the manifests' (`models/*.json`).
    #[test]
    fn match_the_manifests() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
        for m in MODELS {
            let json = std::fs::read_to_string(dir.join(format!("{}.json", m.id))).unwrap();
            assert!(
                json.contains(&format!("\"wake_word\": \"{}\"", m.wake_word)),
                "{}",
                m.id
            );
            assert!(
                json.contains(&format!("\"probability_cutoff\": {}", m.cutoff)),
                "{}",
                m.id
            );
            assert!(
                json.contains(&format!("\"sliding_window_size\": {}", m.window)),
                "{}",
                m.id
            );
            assert!(crate::Model::new(m.tflite).is_ok(), "{}", m.id);
        }
    }
}

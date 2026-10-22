# Built-in wake word models

microWakeWord models (`.tflite` + manifest `.json`) by Kevin Ahrendt / ESPHome, from
[esphome/micro-wake-word-models](https://github.com/esphome/micro-wake-word-models) as shipped in
[`pymicro-wakeword`](https://github.com/OHF-Voice/pymicro-wakeword) 2.5.0, licensed
**Apache-2.0** (both the code and the model weights). Unmodified.

| Model | Wake word | Trained languages | Cutoff | Window |
|---|---|---|---|---|
| `okay_nabu` | Okay Nabu | en, nl, fr, de, it, es, sv | 0.85 | 5 |
| `hey_jarvis` | Hey Jarvis | en | 0.97 | 5 |
| `alexa` | Alexa | en | 0.90 | 5 |
| `hey_mycroft` | Hey Mycroft | en | 0.95 | 5 |

`crates/wakeword` runs them with its own int8 interpreter (H15-68); `tests/model.rs` checks its
outputs against TensorFlow Lite Micro's.

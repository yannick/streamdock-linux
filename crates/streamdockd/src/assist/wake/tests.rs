//! The wake word listener end to end (H15-68): the TTS phrases of `crates/wakeword`'s fixtures as
//! the "microphone" (`test_input`, played at real-time pace) and a fake Home Assistant.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::assist::pipeline::tests::{Behavior, fake_ha_with};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../wakeword/tests/fixtures")
        .join(format!("{name}.wav"))
}

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("streamdockd-wake-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `[modules.homeassistant]` for `url` with an assist table from `assist` (TOML).
fn tables(url: &str, dir: &std::path::Path, assist: &str) -> BTreeMap<String, toml::Table> {
    let token = dir.join("token");
    std::fs::write(&token, "secret").unwrap();
    let text = format!(
        "url = \"{url}\"\ntoken_file = \"{}\"\n[assist]\nspeak = false\n{assist}\n",
        token.display()
    );
    BTreeMap::from([("homeassistant".to_owned(), toml::from_str(&text).unwrap())])
}

/// Waits until the published state satisfies `done` (or fails after `secs`).
async fn wait_for(hub: &Hub, secs: u64, done: impl Fn(&pb::WakeWordState) -> bool) -> pb::WakeWordState {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(s) = hub.wake_state()
            && done(&s)
        {
            return s;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out; last state {:?}",
            hub.wake_state()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn the_wake_word_starts_a_request_with_the_speech_after_it() {
    let (url, seen) = fake_ha_with(Behavior::Answer).await;
    let dir = tmpdir("heard");
    let hub = Hub::new();
    let wake = WakeWord::new(Arc::clone(&hub), Models::new(dir.join("wakewords")));
    let assist = format!(
        "wake_word = \"okay_nabu\"\ntest_input = \"{}\"",
        fixture("okay_nabu_lights").display()
    );
    wake.apply(&tables(&url, &dir, &assist));
    let state = wait_for(&hub, 15, |s| s.phase == "test input played").await;
    assert_eq!(state.heard_count, 1, "{state:?}");
    assert!(state.last_result.contains("→"), "a finished request: {state:?}");
    assert_eq!(
        (state.model.as_str(), state.wake_word.as_str()),
        ("okay_nabu", "Okay Nabu")
    );
    // Only the speech after the word went to Home Assistant: about 2 s of the 3.4 s file.
    let bytes = seen.lock().unwrap().audio_bytes;
    assert!((16_000..=96_000).contains(&bytes), "{bytes} bytes of audio sent");
}

#[tokio::test]
async fn other_speech_starts_nothing() {
    let (url, seen) = fake_ha_with(Behavior::Answer).await;
    let dir = tmpdir("negative");
    let hub = Hub::new();
    let wake = WakeWord::new(Arc::clone(&hub), Models::new(dir.join("wakewords")));
    let assist = format!(
        "wake_word = \"okay_nabu\"\ntest_input = \"{}\"",
        fixture("neg_similar").display()
    );
    wake.apply(&tables(&url, &dir, &assist));
    let state = wait_for(&hub, 20, |s| s.phase == "test input played").await;
    assert_eq!(state.heard_count, 0, "{state:?}");
    assert_eq!(seen.lock().unwrap().audio_bytes, 0);
}

#[tokio::test]
async fn turning_it_off_stops_the_listener() {
    let (url, _seen) = fake_ha_with(Behavior::Answer).await;
    let dir = tmpdir("off");
    let hub = Hub::new();
    let wake = WakeWord::new(Arc::clone(&hub), Models::new(dir.join("wakewords")));
    let assist = format!(
        "wake_word = \"hey_jarvis\"\ntest_input = \"{}\"",
        fixture("neg_similar").display()
    );
    wake.apply(&tables(&url, &dir, &assist));
    wait_for(&hub, 5, |s| s.phase == "listening").await;
    wake.apply(&tables(&url, &dir, ""));
    let state = wait_for(&hub, 5, |s| s.phase == "off").await;
    assert!(!state.enabled);
    assert!(wake.running.lock().unwrap().is_none());
}

#[tokio::test]
async fn an_unknown_model_is_a_problem_not_a_crash() {
    let (url, _seen) = fake_ha_with(Behavior::Answer).await;
    let dir = tmpdir("unknown");
    let hub = Hub::new();
    let wake = WakeWord::new(Arc::clone(&hub), Models::new(dir.join("wakewords")));
    let assist = format!(
        "wake_word = \"hey_dock\"\ntest_input = \"{}\"",
        fixture("neg_similar").display()
    );
    wake.apply(&tables(&url, &dir, &assist));
    let state = wait_for(&hub, 5, |s| !s.problem.is_empty()).await;
    assert!(state.problem.contains("hey_dock"), "{state:?}");
}

#[test]
fn the_meter_reports_rms_in_dbfs() {
    let mut m = Meter::default();
    m.add(&[0; 1600]);
    m.since -= STATE_EVERY;
    assert_eq!(m.due(), Some(-100.0));
    // A full-scale square wave is 0 dBFS; half scale about -6.
    m.add(&[16384, -16384].repeat(800));
    m.since -= STATE_EVERY;
    let db = m.due().unwrap();
    assert!((db + 6.02).abs() < 0.01, "{db}");
    assert_eq!(m.due(), None, "nothing new yet");
}

#[test]
fn microphones_are_the_audio_sources() {
    let dump = r#"[
        {"type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Source", "node.name": "alsa_input.usb-mic", "node.description": "USB Mic"}}},
        {"type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Sink", "node.name": "alsa_output.x"}}},
        {"type": "PipeWire:Interface:Link", "info": {}}
    ]"#;
    assert_eq!(
        sources(dump),
        [("alsa_input.usb-mic".to_owned(), "USB Mic".to_owned())]
    );
    assert_eq!(sources("not json"), Vec::new());
}

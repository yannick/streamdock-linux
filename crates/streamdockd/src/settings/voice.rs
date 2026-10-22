//! The Voice settings (H15-68/69): `[modules.homeassistant.assist]` read for the UI and written
//! back with `toml_edit` (the rest of the file and its comments are kept). Checked by
//! `assist::Config::from_table`, so the UI and the daemon's start can't disagree.

use std::time::Duration;

use streamdock_proto::streamdock::v1 as pb;

use super::{SettingsError, Store, write_atomically};
use crate::assist;

/// The settings as the UI edits them, with what the file holds today.
#[derive(Debug, Clone)]
pub struct Voice {
    /// `[modules.homeassistant]` exists and is enabled.
    pub available: bool,
    pub config: assist::Config,
}

impl Store {
    /// `[modules.homeassistant.assist]` (its defaults where unset).
    pub fn voice(&self) -> Result<Voice, SettingsError> {
        let ha = self.module_table("homeassistant")?;
        let available = ha
            .as_ref()
            .is_some_and(|t| t.get("enabled").and_then(toml::Value::as_bool) != Some(false));
        let assist = ha
            .as_ref()
            .and_then(|t| t.get("assist"))
            .and_then(toml::Value::as_table);
        let config =
            assist::Config::from_table(assist).map_err(|e| SettingsError::Invalid(format!("{e:#}")))?;
        Ok(Voice { available, config })
    }

    /// Checks `settings` and writes them as `[modules.homeassistant.assist]` (`test_input` stays).
    pub fn save_voice(&self, settings: &pb::VoiceSettings) -> Result<(), SettingsError> {
        let current = self.voice()?;
        if !current.available {
            return Err(SettingsError::Invalid(
                "set up Home Assistant first (Settings → Integrations)".into(),
            ));
        }
        let table = voice_table(settings, current.config.test_input.as_deref());
        assist::Config::from_table(Some(&table)).map_err(|e| SettingsError::Invalid(format!("{e:#}")))?;
        let mut doc = self.read()?;
        let ha = doc
            .get_mut("modules")
            .and_then(|m| m.get_mut("homeassistant"))
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| SettingsError::Invalid("`modules.homeassistant` is not a table".into()))?;
        let assist = ha
            .entry("assist")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| SettingsError::Invalid("`modules.homeassistant.assist` is not a table".into()))?;
        let keys: Vec<String> = assist.iter().map(|(k, _)| k.to_owned()).collect();
        for key in keys {
            if !table.contains_key(&key) {
                assist.remove(&key);
            }
        }
        for (key, value) in &table {
            assist[key.as_str()] = toml_edit::value(super::edit_value(value));
        }
        write_atomically(&self.path, doc.to_string().as_bytes(), None)
    }
}

impl Store {
    /// Home Assistant's base URL and token file, for asking it directly (the pipeline list).
    pub fn homeassistant_target(&self) -> Result<Option<(String, std::path::PathBuf)>, SettingsError> {
        let Some(table) = self.module_table("homeassistant")? else {
            return Ok(None);
        };
        let config = crate::modules::homeassistant::Config::from_table(&table)
            .map_err(|e| SettingsError::Invalid(format!("{e:#}")))?;
        Ok(Some((config.url, config.token_file)))
    }
}

/// The table for `settings`: defaults and empty values are left out.
fn voice_table(settings: &pb::VoiceSettings, test_input: Option<&std::path::Path>) -> toml::Table {
    let defaults = assist::Config::default();
    let mut t = toml::Table::new();
    let mut text = |key: &str, value: &str| {
        if !value.trim().is_empty() {
            t.insert(key.into(), value.trim().into());
        }
    };
    text("pipeline", &settings.pipeline);
    text("microphone", &settings.microphone);
    text("wake_word", &settings.wake_word);
    if let Some(path) = test_input {
        t.insert("test_input".into(), path.display().to_string().into());
    }
    if !settings.speak {
        t.insert("speak".into(), false.into());
    }
    let ms = |d: Duration| u32::try_from(d.as_millis()).unwrap_or(u32::MAX);
    let mut number = |key: &str, value: u32, default: u32| {
        if value != default && value != 0 {
            t.insert(key.into(), i64::from(value).into());
        }
    };
    number("silence_ms", settings.silence_ms, ms(defaults.vad.silence));
    number("no_speech_ms", settings.no_speech_ms, ms(defaults.vad.no_speech));
    number(
        "max_seconds",
        settings.max_seconds,
        u32::try_from(defaults.max_listen.as_secs()).unwrap_or(u32::MAX),
    );
    // Floats as the UI sends them, rounded to two decimals so the file stays readable.
    let round = |v: f32| (f64::from(v) * 100.0).round() / 100.0;
    if settings.vad_threshold > 0.0 && (settings.vad_threshold - defaults.vad.threshold).abs() > 0.001 {
        t.insert("vad_threshold".into(), round(settings.vad_threshold).into());
    }
    if let Some(threshold) = settings.wake_threshold {
        t.insert("wake_threshold".into(), round(threshold).into());
    }
    t
}

/// The UI's view of a config.
#[must_use]
pub fn voice_settings(config: &assist::Config) -> pb::VoiceSettings {
    let ms = |d: Duration| u32::try_from(d.as_millis()).unwrap_or(u32::MAX);
    pb::VoiceSettings {
        pipeline: config.pipeline.clone().unwrap_or_default(),
        microphone: config.microphone.clone().unwrap_or_default(),
        speak: config.speak,
        silence_ms: ms(config.vad.silence),
        vad_threshold: config.vad.threshold,
        no_speech_ms: ms(config.vad.no_speech),
        max_seconds: u32::try_from(config.max_listen.as_secs()).unwrap_or(u32::MAX),
        wake_word: config.wake_word.clone().unwrap_or_default(),
        wake_threshold: config.wake_threshold,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str, config: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("streamdockd-voice-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, config).unwrap();
        (Store::new(path.clone(), Vec::new()), path)
    }

    const CONFIG: &str = "# my dock\n[modules.homeassistant]\nurl = \"http://ha:8123\" # the server\n\n[modules.homeassistant.assist]\ntest_input = \"/tmp/x.wav\"\nsilence_ms = 900\n";

    #[test]
    fn reads_and_writes_the_assist_table_keeping_the_rest() {
        let (s, path) = store("rw", CONFIG);
        let v = s.voice().unwrap();
        assert!(v.available);
        assert_eq!(v.config.vad.silence, Duration::from_millis(900));
        let mut settings = voice_settings(&v.config);
        settings.wake_word = "okay_nabu".into();
        settings.wake_threshold = Some(0.9);
        settings.silence_ms = 700;
        settings.pipeline = "01abc".into();
        s.save_voice(&settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# my dock") && text.contains("# the server"),
            "{text}"
        );
        assert!(
            text.contains("wake_word = \"okay_nabu\"") && text.contains("wake_threshold = 0.9"),
            "{text}"
        );
        assert!(text.contains("test_input = \"/tmp/x.wav\""), "kept: {text}");
        assert!(!text.contains("silence_ms"), "the default is left out: {text}");
        let again = s.voice().unwrap().config;
        assert_eq!(again.wake_word.as_deref(), Some("okay_nabu"));
        assert_eq!(again.pipeline.as_deref(), Some("01abc"));
    }

    #[test]
    fn invalid_values_and_a_missing_home_assistant_are_refused() {
        let (s, _) = store("bad", CONFIG);
        let mut settings = voice_settings(&s.voice().unwrap().config);
        settings.wake_threshold = Some(1.5);
        assert!(s.save_voice(&settings).is_err());
        settings.wake_threshold = None;
        settings.silence_ms = 50;
        assert!(s.save_voice(&settings).is_err());
        let (none, _) = store("none", "# nothing\n");
        assert!(!none.voice().unwrap().available);
        assert!(none.save_voice(&pb::VoiceSettings::default()).is_err());
    }
}

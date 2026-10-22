//! Small persistent daemon state (H15-24): what the user changed at runtime and should survive a
//! restart: each dock's brightness, by serial, and the active profile (H15-25). Stored as TOML in
//! `$XDG_STATE_HOME/streamdockd/state.toml` (default `~/.local/state/streamdockd/state.toml`),
//! written atomically (temp file + rename). A missing or unreadable file means "nothing saved".

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Data {
    /// LCD brightness by dock serial, as last set through the API or the dock.
    #[serde(default)]
    brightness: BTreeMap<String, u8>,
    /// The active profile's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    /// The sound settings of the audio output (H15-50, H15-54); "tone" for older files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tone: Option<crate::dsp::Dsp>,
    /// The built-in player's name as renamed in Lyrion (H15-55), with the configured name it
    /// replaced: a new name in the config wins again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    player_name: Option<PlayerName>,
    /// Bumped by "new player id" (H15-61): part of the player id's seed; 0 = the original id.
    #[serde(default, skip_serializing_if = "is_zero")]
    player_id_generation: u32,
    /// The output priority (H15-79): restored after audio restarts and reconnects.
    #[serde(default, skip_serializing_if = "is_default_priority")]
    output_priority: crate::output_memory::Priority,
    /// The Bluetooth codec chosen per device address (H15-78).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    bt_codecs: BTreeMap<String, crate::bt_codec::Preference>,
    /// Device addresses whose link keeps the rate the headset chooses (H15-78; others: 48 kHz).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    bt_any_rate: BTreeSet<String>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_default_priority(p: &crate::output_memory::Priority) -> bool {
    *p == crate::output_memory::Priority::default()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PlayerName {
    configured: String,
    renamed: String,
}

#[derive(Debug)]
pub struct StateFile {
    /// `None`: keep state in memory only (tests, no writable state directory).
    path: Option<PathBuf>,
    data: Mutex<Data>,
}

impl StateFile {
    /// Loads `path`; a missing or broken file starts empty (with a warning if broken).
    pub fn load(path: PathBuf) -> Self {
        let data = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "ignoring unreadable state file");
                Data::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read state file");
                Data::default()
            }
        };
        Self {
            path: Some(path),
            data: Mutex::new(data),
        }
    }

    /// State that is never written to disk.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            path: None,
            data: Mutex::default(),
        }
    }

    /// `$XDG_STATE_HOME/streamdockd/state.toml`, else `~/.local/state/streamdockd/state.toml`.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
        Some(base.join("streamdockd/state.toml"))
    }

    #[must_use]
    pub fn brightness(&self, serial: &str) -> Option<u8> {
        self.data
            .lock()
            .expect("state lock poisoned")
            .brightness
            .get(serial)
            .copied()
    }

    /// Remembers a dock's brightness and saves the file if it changed.
    pub fn set_brightness(&self, serial: &str, percent: u8) {
        self.change(|data| data.brightness.insert(serial.to_owned(), percent) != Some(percent));
    }

    #[must_use]
    pub fn profile(&self) -> Option<String> {
        self.data.lock().expect("state lock poisoned").profile.clone()
    }

    /// Remembers the active profile and saves the file if it changed.
    pub fn set_profile(&self, name: &str) {
        self.change(|data| data.profile.replace(name.to_owned()).as_deref() != Some(name));
    }

    #[must_use]
    pub fn dsp(&self) -> crate::dsp::Dsp {
        self.data
            .lock()
            .expect("state lock poisoned")
            .tone
            .clone()
            .unwrap_or_default()
    }

    /// Remembers the sound settings and saves the file if they changed.
    pub fn set_dsp(&self, dsp: crate::dsp::Dsp) {
        self.change(|data| {
            if data.tone.as_ref() == Some(&dsp) {
                return false;
            }
            data.tone = Some(dsp);
            true
        });
    }

    /// The built-in player's name: the rename from Lyrion, unless the config's name changed since.
    #[must_use]
    pub fn player_name(&self, configured: &str) -> String {
        let data = self.data.lock().expect("state lock poisoned");
        match &data.player_name {
            Some(n) if n.configured == configured => n.renamed.clone(),
            _ => configured.to_owned(),
        }
    }

    /// Remembers a rename from Lyrion; renaming back to the configured name forgets it.
    pub fn set_player_name(&self, configured: &str, renamed: &str) {
        self.change(|data| {
            let next = (renamed != configured).then(|| PlayerName {
                configured: configured.to_owned(),
                renamed: renamed.to_owned(),
            });
            if data.player_name == next {
                return false;
            }
            data.player_name = next;
            true
        });
    }

    #[must_use]
    pub fn player_id_generation(&self) -> u32 {
        self.data
            .lock()
            .expect("state lock poisoned")
            .player_id_generation
    }

    /// A new player id: the next generation, saved.
    pub fn next_player_id_generation(&self) {
        self.change(|data| {
            data.player_id_generation += 1;
            true
        });
    }

    /// The output priority (H15-79).
    #[must_use]
    pub fn output_priority(&self) -> crate::output_memory::Priority {
        self.data
            .lock()
            .expect("state lock poisoned")
            .output_priority
            .clone()
    }

    /// Replaces the output priority (the settings page).
    pub fn set_output_priority(&self, priority: crate::output_memory::Priority) {
        self.change(|data| {
            if data.output_priority == priority {
                return false;
            }
            data.output_priority = priority;
            true
        });
    }

    /// Puts an output the user chose first in the priority.
    pub fn prefer_output(&self, chosen: crate::output_memory::Chosen) {
        self.change(|data| {
            let before = data.output_priority.clone();
            data.output_priority.prefer(chosen);
            data.output_priority != before
        });
    }

    /// The Bluetooth codec chosen for a device address (H15-78).
    #[must_use]
    pub fn bt_codec(&self, address: &str) -> Option<crate::bt_codec::Preference> {
        let key = address.to_ascii_uppercase();
        self.data
            .lock()
            .expect("state lock poisoned")
            .bt_codecs
            .get(&key)
            .cloned()
    }

    /// Remembers (or with `None` forgets) the codec of a device address.
    pub fn set_bt_codec(&self, address: &str, preference: Option<crate::bt_codec::Preference>) {
        let key = address.to_ascii_uppercase();
        self.change(|data| match preference {
            Some(p) => data.bt_codecs.insert(key, p.clone()).as_ref() != Some(&p),
            None => data.bt_codecs.remove(&key).is_some(),
        });
    }

    /// Whether a device address keeps the link rate its headset chooses (H15-78).
    #[must_use]
    pub fn bt_any_rate(&self, address: &str) -> bool {
        let key = address.to_ascii_uppercase();
        self.data
            .lock()
            .expect("state lock poisoned")
            .bt_any_rate
            .contains(&key)
    }

    /// Remembers whether a device address keeps its headset's link rate.
    pub fn set_bt_any_rate(&self, address: &str, any: bool) {
        let key = address.to_ascii_uppercase();
        self.change(|data| {
            if any {
                data.bt_any_rate.insert(key)
            } else {
                data.bt_any_rate.remove(&key)
            }
        });
    }

    /// Applies `edit` (which says whether it changed anything) and saves the file if so.
    fn change(&self, edit: impl FnOnce(&mut Data) -> bool) {
        let snapshot = {
            let mut data = self.data.lock().expect("state lock poisoned");
            if !edit(&mut data) {
                return;
            }
            data.clone()
        };
        if let Some(path) = &self.path
            && let Err(e) = save(path, &snapshot)
        {
            tracing::warn!(path = %path.display(), error = %e, "cannot save state file");
        }
    }
}

fn save(path: &Path, data: &Data) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string(data)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brightness_survives_a_reload() {
        let dir = std::env::temp_dir().join(format!("streamdockd-state-test-{}", std::process::id()));
        let path = dir.join("state.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let state = StateFile::load(path.clone());
        assert_eq!(state.brightness("0300D0000001"), None);
        state.set_brightness("0300D0000001", 42);
        state.set_profile("Evening");
        let reloaded = StateFile::load(path.clone());
        assert_eq!(reloaded.brightness("0300D0000001"), Some(42));
        assert_eq!(reloaded.profile().as_deref(), Some("Evening"));

        std::fs::write(&path, "not = [valid").unwrap();
        assert_eq!(
            StateFile::load(path).brightness("0300D0000001"),
            None,
            "broken file starts empty"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn player_rename_until_the_config_changes() {
        let state = StateFile::in_memory();
        assert_eq!(state.player_name("StreamDock"), "StreamDock");
        state.set_player_name("StreamDock", "Kitchen");
        assert_eq!(state.player_name("StreamDock"), "Kitchen");
        assert_eq!(
            state.player_name("Office"),
            "Office",
            "a new configured name wins"
        );
        state.set_player_name("StreamDock", "StreamDock");
        assert_eq!(state.player_name("StreamDock"), "StreamDock");
    }

    #[test]
    fn in_memory_state_writes_nothing() {
        let state = StateFile::in_memory();
        state.set_brightness("x", 10);
        assert_eq!(state.brightness("x"), Some(10));
    }
}

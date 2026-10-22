//! `[player]` in the config file (H15-52): the built-in Lyrion player's settings, read and written
//! with `toml_edit`, so the rest of the file and its comments are kept.

use super::{SettingsError, Store, write_atomically};
use crate::config::{PlayerSettings, Settings};

impl Store {
    /// The `[player]` settings in the file (the defaults without one).
    pub fn player(&self) -> Result<PlayerSettings, SettingsError> {
        let text = self.read()?.to_string();
        Settings::parse(&text)
            .map(|s| s.player)
            .map_err(|e| SettingsError::Invalid(format!("{e:#}")))
    }

    /// Writes `[player]`; an empty server removes the key (discovery). The caller checks the values.
    pub fn set_player(&self, player: &PlayerSettings) -> Result<(), SettingsError> {
        let mut doc = self.read()?;
        let table = doc
            .entry("player")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| SettingsError::Invalid("`player` in the config is not a table".into()))?;
        table["enabled"] = toml_edit::value(player.enabled);
        table["name"] = toml_edit::value(player.name.as_str());
        match player.server {
            Some(server) => table["server"] = toml_edit::value(server.to_string()),
            None => {
                table.remove("server");
            }
        }
        table["max_sample_rate"] = toml_edit::value(i64::from(player.max_sample_rate));
        let mut formats = toml_edit::Array::new();
        for codec in &player.formats {
            formats.push(codec.name());
        }
        table["formats"] = toml_edit::value(formats);
        table["output_buffer_ms"] =
            toml_edit::value(i64::try_from(player.output_buffer.as_millis()).unwrap_or(i64::MAX));
        match player.replay_gain {
            Some(gain) => table["replay_gain"] = toml_edit::value(gain.name()),
            None => {
                table.remove("replay_gain");
            }
        }
        table["output"] = toml_edit::value(player.output.as_str());
        if let Some(t) = player.transition {
            table["transition"] = toml_edit::value(t.kind.name());
            table["transition_seconds"] = toml_edit::value(i64::from(t.seconds));
        } else {
            table.remove("transition");
            table.remove("transition_seconds");
        }
        table["resampling"] = toml_edit::value(player.resampling_name());
        write_atomically(&self.path, doc.to_string().as_bytes(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_player_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join(format!("streamdockd-player-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# board\nlisten = \"0.0.0.0:7315\"\n\n[player]\nserver = \"10.0.0.2\" # old\n",
        )
        .unwrap();
        let store = Store::new(path.clone(), Vec::new());
        assert!(!store.player().unwrap().enabled);
        let player = PlayerSettings {
            enabled: true,
            name: "Kitchen".into(),
            server: None,
            max_sample_rate: 48_000,
            formats: vec![slimproto::Codec::Pcm],
            output_buffer: std::time::Duration::from_secs(8),
            replay_gain: Some(crate::config::ReplayGain::Smart),
            output: crate::config::OutputChoice::Default,
            transition: Some(crate::config::Transition {
                kind: crate::config::player::TransitionKind::Crossfade,
                seconds: 6,
            }),
            resampling: None,
            max_volume: PlayerSettings::MAX_VOLUME,
        };
        store.set_player(&player).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("resampling = \"pipewire\"")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# board\n"), "comments kept: {text}");
        assert!(!text.contains("server"), "discovery removes the server: {text}");
        assert_eq!(store.player().unwrap(), player);
        let fixed = PlayerSettings {
            server: Some("10.0.0.3:3483".parse().unwrap()),
            ..player
        };
        store.set_player(&fixed).unwrap();
        assert_eq!(store.player().unwrap(), fixed);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

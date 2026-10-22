//! `[network]` in the config file (H15-44): the Wi-Fi mode, changed from the settings page and
//! written with `toml_edit`, so the rest of the file and its comments are kept.

use super::{SettingsError, Store, write_atomically};

impl Store {
    /// Sets `[network] wifi = "<mode>"` (the mode is checked by the caller).
    pub fn set_network_wifi(&self, mode: &str) -> Result<(), SettingsError> {
        let mut doc = self.read()?;
        let network = doc
            .entry("network")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| SettingsError::Invalid("`network` in the config is not a table".into()))?;
        network["wifi"] = toml_edit::value(mode);
        write_atomically(&self.path, doc.to_string().as_bytes(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_wifi_mode_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join(format!("streamdockd-network-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# board\nlisten = \"0.0.0.0:7315\"\n\n[network]\nmanage = true # on the Luckfox\n",
        )
        .unwrap();
        let store = Store::new(path.clone(), Vec::new());
        store.set_network_wifi("off").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# board") && text.contains("manage = true # on the Luckfox"),
            "{text}"
        );
        let parsed = crate::config::Settings::parse(&text).unwrap();
        assert_eq!(parsed.wifi_mode, crate::network::WifiMode::Off);
        assert!(parsed.manage_network);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

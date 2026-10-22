//! Daemon settings: `~/.config/streamdockd/config.toml` (or `$STREAMDOCKD_CONFIG`), then
//! environment overrides. Every setting is optional; a missing file means defaults.
//!
//! ```toml
//! listen = "0.0.0.0:7315"   # web UI and API; default 127.0.0.1:7315
//! also_listen = ["0.0.0.0:80"]  # more addresses for the same UI (port 80 needs CAP_NET_BIND_SERVICE)
//! brightness = 80           # LCD brightness after the dock is opened, 0..=100
//! fake_device = false       # also run the simulated dock FAKE-0001
//! fake_input = false        # let it press keys by itself (only reaches demo modes, H15-33)
//! hardware = true           # look for real docks
//! poll_interval_ms = 1000   # how often to look for (re)plugged docks
//! sleep_after_minutes = 10  # screen off after this long without input (H15-24); 0 = never
//!
//! [night]                   # optional dimmed window, local time (see crate::power)
//! from = "23:00"
//! to = "07:00"
//! brightness = 10
//!
//! [modules.demo]            # one table per module (H15-16); see crate::modules
//! enabled = true
//!
//! mode_order = ["lyrion", "homeassistant"]   # optional tab order by mode id (H15-32)
//!
//! [player]                  # the built-in Lyrion player (H15-55); off by default
//! enabled = true
//! name = "StreamDock"       # the name in Lyrion until it is renamed there
//! server = "192.168.1.2"    # optional, port 3483 by default; found by discovery when unset
//! max_sample_rate = 96000   # 44100, 48000, 96000 (default) or 192000
//!
//! [[profiles]]              # optional (H15-25): named sets of modes, in knob-3 order
//! name = "Office"           # "folder/name" groups profiles in the UI
//! modes = ["homeassistant", "lyrion"]
//! ```
//!
//! Environment overrides: `STREAMDOCKD_ADDR` (listen), `STREAMDOCKD_FAKE_DEVICE` and
//! `STREAMDOCKD_HARDWARE` (any value but `0` is true).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;

use crate::power::{NightFile, Power};

pub mod player;
pub use player::{OutputChoice, PlayerSettings, ReplayGain, Transition};

/// Settings as read from the config file; `None` means "use the default".
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct File {
    listen: Option<SocketAddr>,
    also_listen: Option<Vec<SocketAddr>>,
    brightness: Option<u8>,
    fake_device: Option<bool>,
    fake_input: Option<bool>,
    hardware: Option<bool>,
    poll_interval_ms: Option<u64>,
    sleep_after_minutes: Option<u32>,
    night: Option<NightFile>,
    /// `[modules.<id>]` tables; each module checks its own keys.
    modules: Option<BTreeMap<String, toml::Table>>,
    profiles: Option<Vec<Profile>>,
    mode_order: Option<Vec<String>>,
    network: Option<NetworkFile>,
    audio: Option<AudioFile>,
    player: Option<player::PlayerFile>,
}

/// `[audio]` (H15-50).
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AudioFile {
    /// Let the settings page and the dock manage this machine's audio (PipeWire).
    manage: Option<bool>,
    /// The user PipeWire runs as (default "sound").
    user: Option<String>,
}

/// `[network]` (H15-38).
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct NetworkFile {
    /// Let the settings page manage this machine's Wi-Fi and Bluetooth (NetworkManager, BlueZ).
    manage: Option<bool>,
    /// When the Wi-Fi radio is on: "off", "fallback" (default) or "both" (H15-44).
    wifi: Option<String>,
}

/// A named set of modes (H15-25): knob 3 cycles through these modes only.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    /// Mode ids (see `Screen.modes`), in knob-3 order. Ids of modes that aren't enabled are skipped.
    pub modes: Vec<String>,
}

/// Effective settings.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent on/off settings from the config file"
)]
pub struct Settings {
    pub listen: SocketAddr,
    /// More addresses serving the same UI and API (e.g. port 80 on the Luckfox, H15-34).
    pub also_listen: Vec<SocketAddr>,
    pub brightness: u8,
    pub fake_device: bool,
    /// The simulated dock generates input by itself (demo modes only).
    pub fake_input: bool,
    pub hardware: bool,
    pub poll_interval: Duration,
    /// Sleep and night settings for every dock.
    pub power: Power,
    /// The `[modules.<id>]` tables, by module id (see `crate::modules::from_config`).
    pub modules: BTreeMap<String, toml::Table>,
    /// `[[profiles]]`; empty = every mode, no profiles.
    pub profiles: Vec<Profile>,
    /// Tab order by mode id (H15-32); modes not listed follow in config order.
    pub mode_order: Vec<String>,
    /// `[network] manage`: the API may change this machine's Wi-Fi and Bluetooth (H15-38). Off by
    /// default, so a laptop running streamdockd can't be reconfigured from the LAN.
    pub manage_network: bool,
    /// `[network] wifi` (H15-44).
    pub wifi_mode: crate::network::WifiMode,
    /// `[audio] manage`: the user PipeWire runs as, when the daemon manages this machine's audio
    /// (H15-50); `None` = hands off (the default).
    pub audio_user: Option<String>,
    /// `[player]`: the built-in Lyrion player (H15-55); off by default.
    pub player: PlayerSettings,
    /// The file the settings came from, if it existed.
    pub source: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 7315)),
            also_listen: Vec::new(),
            brightness: streamdock_hid::DEFAULT_BRIGHTNESS,
            fake_device: false,
            fake_input: false,
            hardware: true,
            poll_interval: Duration::from_secs(1),
            power: Power::default(),
            modules: BTreeMap::new(),
            profiles: Vec::new(),
            mode_order: Vec::new(),
            manage_network: false,
            wifi_mode: crate::network::WifiMode::default(),
            audio_user: None,
            player: PlayerSettings::default(),
            source: None,
        }
    }
}

impl Settings {
    /// The config file in use: `$STREAMDOCKD_CONFIG` or the default location. It may not exist.
    #[must_use]
    pub fn path() -> Option<PathBuf> {
        std::env::var_os("STREAMDOCKD_CONFIG")
            .map(PathBuf::from)
            .or_else(default_path)
    }

    /// Loads the config file (if any) and applies environment overrides.
    pub fn load() -> anyhow::Result<Self> {
        let path = std::env::var_os("STREAMDOCKD_CONFIG")
            .map(PathBuf::from)
            .or_else(default_path);
        let mut settings = match &path {
            Some(path) => Self::from_file(path)?,
            None => Self::default(),
        };
        settings.apply_env(|name| std::env::var(name).ok())?;
        Ok(settings)
    }

    /// Reads `path`; a missing file gives the defaults.
    fn from_file(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut settings =
                    Self::parse(&text).with_context(|| format!("invalid config {}", path.display()))?;
                settings.source = Some(path.to_owned());
                Ok(settings)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub(crate) fn parse(text: &str) -> anyhow::Result<Self> {
        let file: File = toml::from_str(text)?;
        let defaults = Self::default();
        if let Some(b) = file.brightness {
            anyhow::ensure!(b <= 100, "brightness must be 0..=100, got {b}");
        }
        let profiles = file.profiles.unwrap_or_default();
        for (i, p) in profiles.iter().enumerate() {
            anyhow::ensure!(!p.name.trim().is_empty(), "profiles: a profile needs a name");
            anyhow::ensure!(!p.modes.is_empty(), "profile {:?} has no modes", p.name);
            anyhow::ensure!(
                !profiles[..i].iter().any(|q| q.name == p.name),
                "profile {:?} is defined twice",
                p.name
            );
        }
        if let Some(ms) = file.poll_interval_ms {
            anyhow::ensure!(ms >= 100, "poll_interval_ms must be at least 100, got {ms}");
        }
        Ok(Self {
            listen: file.listen.unwrap_or(defaults.listen),
            also_listen: file.also_listen.unwrap_or_default(),
            brightness: file.brightness.unwrap_or(defaults.brightness),
            fake_device: file.fake_device.unwrap_or(defaults.fake_device),
            fake_input: file.fake_input.unwrap_or(defaults.fake_input),
            hardware: file.hardware.unwrap_or(defaults.hardware),
            poll_interval: file
                .poll_interval_ms
                .map_or(defaults.poll_interval, Duration::from_millis),
            power: Power::from_file(file.sleep_after_minutes, file.night.as_ref())?,
            modules: file.modules.unwrap_or_default(),
            profiles,
            mode_order: file.mode_order.unwrap_or_default(),
            manage_network: file.network.as_ref().and_then(|n| n.manage).unwrap_or(false),
            wifi_mode: match file.network.as_ref().and_then(|n| n.wifi.as_deref()) {
                Some(mode) => crate::network::WifiMode::parse(mode)?,
                None => crate::network::WifiMode::default(),
            },
            audio_user: file
                .audio
                .filter(|a| a.manage == Some(true))
                .map(|a| a.user.unwrap_or_else(|| "sound".to_owned())),
            player: PlayerSettings::from_file(file.player)?,
            source: None,
        })
    }

    fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) -> anyhow::Result<()> {
        if let Some(addr) = var("STREAMDOCKD_ADDR") {
            self.listen = addr.parse().context("invalid STREAMDOCKD_ADDR")?;
        }
        if let Some(fake) = var("STREAMDOCKD_FAKE_DEVICE") {
            self.fake_device = fake != "0";
        }
        if let Some(input) = var("STREAMDOCKD_FAKE_INPUT") {
            self.fake_input = input != "0";
        }
        if let Some(hardware) = var("STREAMDOCKD_HARDWARE") {
            self.hardware = hardware != "0";
        }
        Ok(())
    }
}

/// `$XDG_CONFIG_HOME/streamdockd/config.toml`, else `~/.config/streamdockd/config.toml`.
fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("streamdockd/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_management_is_opt_in() {
        assert!(!Settings::parse("").unwrap().manage_network, "off by default");
        assert!(
            Settings::parse("[network]\nmanage = true")
                .unwrap()
                .manage_network
        );
        assert!(
            !Settings::parse("[network]\nmanage = false")
                .unwrap()
                .manage_network
        );
        assert!(
            Settings::parse("[network]\nwifi = true").is_err(),
            "unknown keys are an error"
        );
    }

    #[test]
    fn empty_file_is_defaults() {
        assert_eq!(Settings::parse("").unwrap(), Settings::default());
    }

    #[test]
    fn file_values_and_env_overrides() {
        let mut s = Settings::parse(
            "listen = \"0.0.0.0:8000\"\nbrightness = 40\nfake_device = true\nhardware = false\npoll_interval_ms = 500\n",
        )
        .unwrap();
        assert_eq!(s.listen, "0.0.0.0:8000".parse().unwrap());
        assert_eq!(s.brightness, 40);
        assert!(s.fake_device && !s.hardware);
        assert_eq!(s.poll_interval, Duration::from_millis(500));

        s.apply_env(|name| match name {
            "STREAMDOCKD_ADDR" => Some("127.0.0.1:9000".into()),
            "STREAMDOCKD_FAKE_DEVICE" => Some("0".into()),
            "STREAMDOCKD_HARDWARE" => Some("1".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(s.listen, "127.0.0.1:9000".parse().unwrap());
        assert!(!s.fake_device);
        assert!(s.hardware, "STREAMDOCKD_HARDWARE=1 overrides hardware = false");
    }

    #[test]
    fn rejects_bad_values() {
        assert!(Settings::parse("brightness = 101").is_err());
        assert!(Settings::parse("poll_interval_ms = 10").is_err());
        assert!(
            Settings::parse("colour = \"red\"").is_err(),
            "unknown keys are errors"
        );
    }

    #[test]
    fn module_tables() {
        let s = Settings::parse("[modules.demo]\npages = 2\n\n[modules.lyrion]\nenabled = false\n").unwrap();
        assert_eq!(s.modules.keys().collect::<Vec<_>>(), ["demo", "lyrion"]);
        assert_eq!(s.modules["demo"]["pages"].as_integer(), Some(2));
    }

    #[test]
    fn profiles() {
        let s = Settings::parse(
            "[[profiles]]\nname = \"Office\"\nmodes = [\"lyrion\", \"demo-1\"]\n\n\
             [[profiles]]\nname = \"Evening/Music\"\nmodes = [\"lyrion\"]\n",
        )
        .unwrap();
        assert_eq!(s.profiles.len(), 2);
        assert_eq!(s.profiles[0].modes, ["lyrion", "demo-1"]);
        assert!(Settings::parse("[[profiles]]\nname = \"\"\nmodes = [\"a\"]\n").is_err());
        assert!(Settings::parse("[[profiles]]\nname = \"A\"\nmodes = []\n").is_err());
        let twice = "[[profiles]]\nname = \"A\"\nmodes = [\"a\"]\n";
        assert!(Settings::parse(&format!("{twice}{twice}")).is_err());
    }

    #[test]
    fn missing_file_is_defaults() {
        let s = Settings::from_file(Path::new("/nonexistent/streamdockd.toml")).unwrap();
        assert_eq!(s, Settings::default());
    }
}

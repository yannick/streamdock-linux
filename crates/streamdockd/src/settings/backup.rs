//! Backup of the whole configuration as one file (H15-26), like OpenDeck's movable profiles.
//!
//! The file is TOML with the config file embedded as text, so comments survive the round trip:
//!
//! ```toml
//! format = "streamdockd-backup"
//! version = 1
//! created = "2026-10-18T03:00:00+02:00"
//! config = """
//! listen = "0.0.0.0:7315"
//! [modules.lyrion]
//! …
//! """
//! ```
//!
//! Secrets live in their own files and are never part of it; an import lists the ones this
//! machine is missing. An import keeps this machine's own values ([`MACHINE_KEYS`]) and saves the
//! replaced config as `<config>.bak`. Custom keys, profiles and key images join the file when
//! they exist (H15-23, H15-25, H15-28).

use serde::{Deserialize, Serialize};
use streamdock_proto::streamdock::v1 as pb;
use toml_edit::DocumentMut;

use super::{Kind, SettingsError, Store, secret_is_set, specs, write_atomically};
use crate::{config, modules};

const FORMAT: &str = "streamdockd-backup";
const VERSION: u32 = 1;
/// Larger files are refused (a config is a few KB).
const MAX_SIZE: usize = 1024 * 1024;
/// Settings that belong to the machine, not to the setup: an import keeps the local ones.
const MACHINE_KEYS: [&str; 5] = [
    "listen",
    "also_listen",
    "hardware",
    "fake_device",
    "poll_interval_ms",
];

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackupFile {
    format: String,
    version: u32,
    created: String,
    config: String,
}

/// What an import did.
#[derive(Debug, Default)]
pub struct Imported {
    /// Settings outside `[modules.*]` changed: those need a restart.
    pub daemon_changed: bool,
    pub missing_secrets: Vec<pb::MissingSecret>,
    pub notes: Vec<String>,
}

impl Store {
    /// The backup file and a suggested file name.
    pub fn export_backup(&self) -> Result<(Vec<u8>, String), SettingsError> {
        let now = chrono::Local::now();
        let file = BackupFile {
            format: FORMAT.into(),
            version: VERSION,
            created: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
            config: self.read()?.to_string(),
        };
        let body = toml::to_string_pretty(&file).map_err(|e| SettingsError::Io(e.to_string()))?;
        let text = format!(
            "# streamdockd backup: import it on the settings page (/settings).\n\
             # Secrets (tokens) are not included; the import says which ones to enter again.\n{body}"
        );
        Ok((
            text.into_bytes(),
            format!("streamdockd-backup-{}.toml", now.format("%Y-%m-%d")),
        ))
    }

    /// Replaces the config with the one in `data` (see the module docs).
    pub fn import_backup(&self, data: &[u8]) -> Result<Imported, SettingsError> {
        let invalid = |m: String| SettingsError::Invalid(format!("not a usable backup: {m}"));
        if data.len() > MAX_SIZE {
            return Err(invalid(format!("larger than {} KB", MAX_SIZE / 1024)));
        }
        let text = std::str::from_utf8(data).map_err(|_| invalid("not a text file".into()))?;
        let file: BackupFile = toml::from_str(text).map_err(|e| invalid(e.message().to_owned()))?;
        if file.format != FORMAT {
            return Err(invalid(format!(
                "format is {:?}, expected {FORMAT:?}",
                file.format
            )));
        }
        if file.version > VERSION {
            return Err(invalid(format!(
                "version {} is newer than this streamdockd understands ({VERSION}); update streamdockd first",
                file.version
            )));
        }

        let local = self.read()?;
        let without_modules = |text: &str| {
            let mut t: toml::Table = toml::from_str(text).unwrap_or_default();
            t.remove("modules");
            t
        };
        let mut doc = file
            .config
            .parse::<DocumentMut>()
            .map_err(|e| invalid(format!("its config: {e}")))?;
        let mut notes = Vec::new();
        for key in MACHINE_KEYS {
            match local.get(key) {
                Some(item) => {
                    doc[key] = item.clone();
                }
                None => {
                    doc.remove(key);
                }
            }
        }
        notes.push(format!("kept this machine's {}", MACHINE_KEYS.join(", ")));

        // The same checks as at startup: the daemon must be able to start with it.
        let text = doc.to_string();
        let settings = config::Settings::parse(&text).map_err(|e| invalid(format!("{e:#}")))?;
        modules::from_config(&settings.modules, &self.module_context(&settings.modules))
            .map_err(|e| invalid(format!("{e:#}")))?;

        if self.path.exists() {
            let bak = self.path.with_extension("toml.bak");
            std::fs::copy(&self.path, &bak)
                .map_err(|e| SettingsError::Io(format!("saving {}: {e}", bak.display())))?;
            notes.push(format!("previous config saved as {}", bak.display()));
        }
        let daemon_changed = without_modules(&local.to_string()) != without_modules(&text);
        write_atomically(&self.path, text.as_bytes(), None)?;
        notes.push(format!(
            "{} imported (backup from {})",
            self.path.display(),
            file.created
        ));

        let enabled: Vec<&String> = settings
            .modules
            .iter()
            .filter(|(_, t)| t.get("enabled").and_then(toml::Value::as_bool) != Some(false))
            .map(|(id, _)| id)
            .collect();
        let missing_secrets = specs()
            .into_iter()
            .filter(|spec| enabled.iter().any(|id| id.as_str() == spec.id))
            .flat_map(|spec| {
                let table = settings.modules.get(spec.id);
                spec.fields
                    .iter()
                    .filter(|f| f.kind == Kind::Secret && !secret_is_set(table))
                    .map(|f| pb::MissingSecret {
                        module: spec.id.into(),
                        key: f.key.into(),
                        label: format!("{}: {}", spec.title, f.label),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        Ok(Imported {
            daemon_changed,
            missing_secrets,
            notes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str, config: Option<&str>) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("streamdockd-backup-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        if let Some(text) = config {
            std::fs::write(&path, text).unwrap();
        }
        (Store::new(path, Vec::new()), dir)
    }

    const SOURCE: &str = "# my dock\nlisten = \"0.0.0.0:7315\"\nbrightness = 40\n\n\
        [modules.lyrion]\nurl = \"http://lms:9000\" # the NAS\nplayer = \"Kitchen\"\n\n\
        [modules.homeassistant]\nurl = \"https://ha\"\narea = \"Office\"\ntoken_file = \"/nonexistent/token\"\n";

    #[test]
    fn round_trip_keeps_comments_and_this_machines_values() {
        let (from, from_dir) = store("from", Some(SOURCE));
        let (data, name) = from.export_backup().unwrap();
        assert!(name.starts_with("streamdockd-backup-"));
        assert_eq!(
            std::path::Path::new(&name).extension().and_then(|e| e.to_str()),
            Some("toml")
        );
        let text = String::from_utf8(data.clone()).unwrap();
        assert!(text.contains("format = \"streamdockd-backup\""), "{text}");

        let (to, to_dir) = store("to", Some("listen = \"127.0.0.1:9999\"\nhardware = false\n"));
        let imported = to.import_backup(&data).unwrap();
        let written = std::fs::read_to_string(to_dir.join("config.toml")).unwrap();
        assert!(
            written.contains("# the NAS") && written.contains("player = \"Kitchen\""),
            "{written}"
        );
        assert!(written.contains("brightness = 40"));
        assert!(written.contains("listen = \"127.0.0.1:9999\"") && !written.contains("0.0.0.0"));
        assert!(written.contains("hardware = false"));
        assert!(to_dir.join("config.toml.bak").exists());
        // The HA token isn't in the backup and not on this machine.
        assert_eq!(imported.missing_secrets.len(), 1);
        assert_eq!(imported.missing_secrets[0].module, "homeassistant");
        for dir in [from_dir, to_dir] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn export_without_config_file_and_import_into_empty_machine() {
        let (from, from_dir) = store("empty-from", None);
        let (data, _) = from.export_backup().unwrap();
        let (to, to_dir) = store("empty-to", None);
        let imported = to.import_backup(&data).unwrap();
        assert_eq!(imported.missing_secrets.len(), 0);
        assert!(!to_dir.join("config.toml.bak").exists());
        for dir in [from_dir, to_dir] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn bad_backups_are_refused_and_change_nothing() {
        let (to, dir) = store("bad", Some("brightness = 10\n"));
        let wrap = |config: &str, format: &str, version: u32| {
            toml::to_string(&BackupFile {
                format: format.into(),
                version,
                created: "x".into(),
                config: config.into(),
            })
            .unwrap()
            .into_bytes()
        };
        let cases = [
            (b"not toml at all [".to_vec(), "not a usable backup"),
            (wrap("", "something-else", 1), "format"),
            (wrap("", FORMAT, 99), "newer"),
            (wrap("brightness = 300\n", FORMAT, 1), "brightness"),
            (wrap("[modules.lyrion]\nplayer = \"x\"\n", FORMAT, 1), "url"),
            (wrap("[modules.nope]\n", FORMAT, 1), "unknown module"),
            (vec![b' '; MAX_SIZE + 1], "larger"),
        ];
        for (data, expect) in cases {
            let err = to.import_backup(&data).unwrap_err().to_string();
            assert!(err.contains(expect), "{expect:?} not in {err:?}");
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("config.toml")).unwrap(),
            "brightness = 10\n"
        );
        // A disabled module may be incomplete.
        to.import_backup(&wrap("[modules.lyrion]\nenabled = false\n", FORMAT, 1))
            .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}

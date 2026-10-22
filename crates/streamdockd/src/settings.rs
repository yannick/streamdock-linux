//! Settings (H15-22): what the settings screen shows and changes.
//!
//! Each module's settings are described here ([`specs`]), read from and written to the
//! `[modules.<id>]` tables of the config file with `toml_edit`, so comments and layout survive.
//! Changes are validated with the module's own config parser before they are written. Secrets
//! (the Home Assistant token) are write-only: stored in their own mode-600 file, never returned.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use streamdock_proto::streamdock::v1 as pb;
use toml_edit::DocumentMut;

use crate::modules;

mod backup;
mod keys;
mod network;
mod player;
pub mod tabs;
pub mod voice;

/// How a field is edited and stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Url,
    Integer,
    /// Write-only, stored outside the config file (see [`secret_path`]).
    Secret,
}

#[derive(Debug, Clone)]
pub struct FieldSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub required: bool,
    pub default: &'static str,
    /// Whether [`choices`] can suggest values.
    pub choices: bool,
}

#[derive(Debug, Clone)]
pub struct ModuleSpec {
    pub id: &'static str,
    pub title: &'static str,
    pub help: &'static str,
    pub fields: Vec<FieldSpec>,
}

const fn field(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    kind: Kind,
    required: bool,
) -> FieldSpec {
    FieldSpec {
        key,
        label,
        help,
        kind,
        required,
        default: "",
        choices: false,
    }
}

/// Default location of the Home Assistant token (`[modules.homeassistant] token_file`).
pub const HA_TOKEN_FILE: &str = "~/.config/streamdockd/homeassistant.token";

/// The settings of every module that can be enabled from the settings screen.
#[must_use]
pub fn specs() -> Vec<ModuleSpec> {
    vec![
        ModuleSpec {
            id: "homeassistant",
            title: "Lights (Home Assistant)",
            help: "The lights of one Home Assistant area on the LCD keys. Knob 1 = brightness, knob 2 = color temperature.",
            fields: vec![
                field(
                    "url",
                    "Home Assistant URL",
                    "e.g. https://ha.example.org",
                    Kind::Url,
                    true,
                ),
                FieldSpec {
                    choices: true,
                    ..field(
                        "area",
                        "Area",
                        "Area name or id; matched loosely (case, umlauts, spaces).",
                        Kind::Text,
                        true,
                    )
                },
                field(
                    "token",
                    "Access token",
                    "A long-lived access token (HA → profile → Security). Stored in its own file, never shown.",
                    Kind::Secret,
                    true,
                ),
                FieldSpec {
                    default: HA_TOKEN_FILE,
                    ..field(
                        "token_file",
                        "Token file",
                        "Where the token is stored.",
                        Kind::Text,
                        false,
                    )
                },
            ],
        },
        ModuleSpec {
            id: "lyrion",
            title: "Music (Lyrion)",
            help: "Lyrion Music Server playlists whose names start with the prefix, on the LCD keys. The big knob = volume.",
            fields: vec![
                field(
                    "url",
                    "Lyrion URL",
                    "e.g. http://192.168.1.10:9000",
                    Kind::Url,
                    true,
                ),
                FieldSpec {
                    choices: true,
                    default: modules::lyrion::DEFAULT_PLAYER,
                    ..field(
                        "player",
                        "Player",
                        "The Lyrion player the music tab controls. Stored by its MAC, so a rename in Lyrion doesn't matter.",
                        Kind::Text,
                        true,
                    )
                },
                FieldSpec {
                    default: "_dock",
                    ..field(
                        "prefix",
                        "Playlist prefix",
                        "Only playlists starting with this are shown (without it).",
                        Kind::Text,
                        false,
                    )
                },
            ],
        },
        settings_spec(),
        ModuleSpec {
            id: "demo",
            title: "Demo",
            help: "Example mode for testing: keys toggle, knob 1 counts.",
            fields: vec![FieldSpec {
                default: "1",
                ..field(
                    "pages",
                    "Pages",
                    "Number of demo modes, 1-9.",
                    Kind::Integer,
                    false,
                )
            }],
        },
    ]
}

/// The dock's settings screen (H15-91).
fn settings_spec() -> ModuleSpec {
    ModuleSpec {
        id: modules::settings::ID,
        title: "Settings screen",
        help: "A screen on the dock with the paired Bluetooth headsets and speakers: a press connects or disconnects one (pairing stays here).",
        fields: vec![
            FieldSpec {
                default: "Settings",
                ..field(
                    "title",
                    "Name",
                    "The screen's name on the dock.",
                    Kind::Text,
                    false,
                )
            },
            field(
                "speaker_player",
                "Speaker player",
                "Headset mode: the Lyrion player (MAC) of the speaker the music comes back to. Empty: no headset mode. Needs the Lyrion integration.",
                Kind::Text,
                false,
            ),
            field(
                "headset_player",
                "Headset player",
                "Headset mode: the Lyrion player that plays into the headset. Empty: the built-in player.",
                Kind::Text,
                false,
            ),
        ],
    }
}

/// Checks a module table with the module's own parser; `Err` explains what's wrong.
fn validate(id: &str, table: &toml::Table, assets: Option<&crate::assets::Assets>) -> Result<(), String> {
    let result = match id {
        "homeassistant" => modules::homeassistant::checked_tabs(table, assets).map(drop),
        "lyrion" => modules::lyrion::from_table(table).map(drop),
        "demo" => modules::demo::from_table(table).map(drop),
        modules::settings::ID => modules::settings::Config::from_table(table).map(drop),
        other => return Err(format!("unknown module {other:?}")),
    };
    result.map_err(|e| format!("{e:#}"))
}

/// Why a settings change was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Io(String),
}

/// The config file and the secrets next to it.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
    /// Daemon settings to show read-only.
    daemon: Vec<pb::SettingValue>,
    /// Applies saved module settings to the running daemon (none in tests).
    reloader: Option<std::sync::Arc<crate::reload::Reloader>>,
}

impl Store {
    #[must_use]
    pub fn new(path: PathBuf, daemon: Vec<(&str, &str, String)>) -> Self {
        let daemon = daemon
            .into_iter()
            .map(|(key, label, value)| pb::SettingValue {
                key: key.into(),
                label: label.into(),
                value,
                ..Default::default()
            })
            .collect();
        Self {
            path,
            daemon,
            reloader: None,
        }
    }

    #[must_use]
    pub fn with_reloader(mut self, reloader: std::sync::Arc<crate::reload::Reloader>) -> Self {
        self.reloader = Some(reloader);
        self
    }

    /// Applies the saved modules to the running daemon. `false` if that failed or there is no
    /// reloader, so a restart is needed; failures are logged.
    pub async fn apply(&self) -> bool {
        let Some(reloader) = &self.reloader else {
            return false;
        };
        match reloader.apply().await {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "saved settings not applied");
                false
            }
        }
    }

    /// The same asset resolution as startup, for validation before a settings write.
    fn module_context(&self, tables: &std::collections::BTreeMap<String, toml::Table>) -> modules::Context {
        let hub = crate::hub::Hub::new();
        hub.set_assets(std::sync::Arc::new(crate::assets::Assets::beside(&self.path)));
        modules::Context::new(hub, tables)
    }

    fn read(&self) -> Result<DocumentMut, SettingsError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => text
                .parse::<DocumentMut>()
                .map_err(|e| SettingsError::Invalid(format!("{}: {e}", self.path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
            Err(e) => Err(SettingsError::Io(format!("reading {}: {e}", self.path.display()))),
        }
    }

    fn module_table(&self, id: &str) -> Result<Option<toml::Table>, SettingsError> {
        let text = self.read()?.to_string();
        let doc: toml::Table = toml::from_str(&text).map_err(|e| SettingsError::Invalid(e.to_string()))?;
        Ok(doc
            .get("modules")
            .and_then(|m| m.get(id))
            .and_then(|t| t.as_table())
            .cloned())
    }

    /// Everything the settings screen shows.
    pub fn get(&self) -> Result<pb::GetSettingsResponse, SettingsError> {
        let mut modules = Vec::new();
        for spec in specs() {
            let table = self.module_table(spec.id)?;
            let enabled = table
                .as_ref()
                .is_some_and(|t| t.get("enabled").and_then(toml::Value::as_bool).unwrap_or(true));
            let values: HashMap<String, String> = spec
                .fields
                .iter()
                .filter(|f| f.kind != Kind::Secret)
                .filter_map(|f| {
                    let v = table.as_ref()?.get(f.key)?;
                    let text = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
                    Some((f.key.to_owned(), text))
                })
                .collect();
            let fields = spec
                .fields
                .iter()
                .map(|f| pb::SettingField {
                    key: f.key.into(),
                    label: f.label.into(),
                    help: f.help.into(),
                    r#type: setting_type(f.kind).into(),
                    required: f.required,
                    default_value: f.default.into(),
                    secret_set: f.kind == Kind::Secret && secret_is_set(table.as_ref()),
                    has_choices: f.choices,
                    ..Default::default()
                })
                .collect();
            let error = match &table {
                Some(t) if enabled => validate(spec.id, t, Some(&crate::assets::Assets::beside(&self.path)))
                    .err()
                    .unwrap_or_default(),
                _ => String::new(),
            };
            modules.push(pb::ModuleSettings {
                id: spec.id.into(),
                title: spec.title.into(),
                help: spec.help.into(),
                configured: table.is_some(),
                enabled,
                fields,
                values: values.into_iter().collect(),
                error,
                ..Default::default()
            });
        }
        Ok(pb::GetSettingsResponse {
            config_path: self.path.display().to_string(),
            modules,
            daemon: self.daemon.clone(),
            ..Default::default()
        })
    }

    /// Validates and writes one module's settings. Values are strings as typed; empty = unset.
    pub fn update(
        &self,
        id: &str,
        enabled: bool,
        values: &HashMap<String, String>,
    ) -> Result<(), SettingsError> {
        let spec = specs()
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| SettingsError::NotFound(format!("no module {id:?}")))?;
        if let Some(key) = values.keys().find(|k| {
            !spec
                .fields
                .iter()
                .any(|f| f.key == k.as_str() && f.kind != Kind::Secret)
        }) {
            return Err(SettingsError::Invalid(format!("{id} has no setting {key:?}")));
        }
        // The new table: the old one with the form's values; only enabled modules must be valid.
        let mut table = self.module_table(id)?.unwrap_or_default();
        table.insert("enabled".into(), enabled.into());
        for f in spec.fields.iter().filter(|f| f.kind != Kind::Secret) {
            match values.get(f.key).map(|v| v.trim()).filter(|v| !v.is_empty()) {
                Some(text) => {
                    table.insert(f.key.into(), typed(f, text)?);
                }
                None => {
                    table.remove(f.key);
                }
            }
        }
        if enabled {
            validate(id, &table, Some(&crate::assets::Assets::beside(&self.path)))
                .map_err(SettingsError::Invalid)?;
        }

        let mut doc = self.read()?;
        let modules = doc
            .entry("modules")
            .or_insert_with(|| {
                let mut t = toml_edit::Table::new();
                t.set_implicit(true);
                toml_edit::Item::Table(t)
            })
            .as_table_mut()
            .ok_or_else(|| SettingsError::Invalid("`modules` in the config is not a table".into()))?;
        let module = modules
            .entry(id)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| SettingsError::Invalid(format!("`modules.{id}` in the config is not a table")))?;
        module["enabled"] = toml_edit::value(enabled);
        for f in spec.fields.iter().filter(|f| f.kind != Kind::Secret) {
            match table.get(f.key) {
                Some(v) => module[f.key] = toml_edit::value(edit_value(v)),
                None => {
                    module.remove(f.key);
                }
            }
        }
        write_atomically(&self.path, doc.to_string().as_bytes(), None)
    }

    /// Stores a secret (write-only).
    pub fn set_secret(&self, id: &str, key: &str, value: &str) -> Result<(), SettingsError> {
        if (id, key) != ("homeassistant", "token") {
            return Err(SettingsError::NotFound(format!("{id} has no secret {key:?}")));
        }
        let value = value.trim();
        if value.is_empty() || value.contains(char::is_whitespace) {
            return Err(SettingsError::Invalid(
                "the token must be one line without spaces".into(),
            ));
        }
        let path = secret_path(self.module_table(id)?.as_ref());
        write_atomically(&path, format!("{value}\n").as_bytes(), Some(0o600))
    }
}

fn setting_type(kind: Kind) -> pb::SettingType {
    match kind {
        Kind::Text => pb::SettingType::SETTING_TYPE_TEXT,
        Kind::Url => pb::SettingType::SETTING_TYPE_URL,
        Kind::Integer => pb::SettingType::SETTING_TYPE_INTEGER,
        Kind::Secret => pb::SettingType::SETTING_TYPE_SECRET,
    }
}

/// A form value as the TOML type the module expects.
fn typed(f: &FieldSpec, text: &str) -> Result<toml::Value, SettingsError> {
    match f.kind {
        Kind::Integer => text
            .parse::<i64>()
            .map(toml::Value::Integer)
            .map_err(|_| SettingsError::Invalid(format!("{} must be a whole number", f.label))),
        Kind::Url if !(text.starts_with("http://") || text.starts_with("https://")) => Err(
            SettingsError::Invalid(format!("{} must start with http:// or https://", f.label)),
        ),
        _ => Ok(text.into()),
    }
}

fn edit_value(v: &toml::Value) -> toml_edit::Value {
    match v {
        toml::Value::Integer(i) => (*i).into(),
        toml::Value::Boolean(b) => (*b).into(),
        toml::Value::Float(f) => (*f).into(),
        other => other
            .as_str()
            .map_or_else(|| other.to_string(), str::to_owned)
            .into(),
    }
}

/// The token file of the Home Assistant module, from its table or the default.
fn secret_path(table: Option<&toml::Table>) -> PathBuf {
    let raw = table
        .and_then(|t| t.get("token_file"))
        .and_then(toml::Value::as_str)
        .unwrap_or(HA_TOKEN_FILE);
    match (raw.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(raw),
    }
}

fn secret_is_set(table: Option<&toml::Table>) -> bool {
    std::fs::read_to_string(secret_path(table)).is_ok_and(|t| !t.trim().is_empty())
}

/// Writes `bytes` to `path` via a temporary file in the same directory, so a crash never leaves
/// a half-written config. `mode` sets the permissions of new and replaced files (secrets: 600).
fn write_atomically(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<(), SettingsError> {
    let io = |what: &str, e: std::io::Error| SettingsError::Io(format!("{what} {}: {e}", path.display()));
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| io("creating the directory of", e))?;
    let tmp = path.with_extension("tmp-streamdockd");
    {
        use std::io::Write as _;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            let keep = std::fs::metadata(path).ok().map(|m| {
                use std::os::unix::fs::PermissionsExt as _;
                m.permissions().mode() & 0o777
            });
            options.mode(mode.or(keep).unwrap_or(0o644));
        }
        let mut file = options.open(&tmp).map_err(|e| io("writing", e))?;
        file.write_all(bytes).map_err(|e| io("writing", e))?;
        file.sync_all().map_err(|e| io("writing", e))?;
    }
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
            .map_err(|e| io("securing", e))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| io("replacing", e))
}

/// A suggested value and how to show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// What is stored (a Lyrion player's MAC, an area's name).
    pub value: String,
    /// What people read (the player's name).
    pub label: String,
    /// "playing", "offline" or empty.
    pub detail: String,
}

impl Choice {
    fn plain(value: String) -> Self {
        Self {
            label: value.clone(),
            value,
            detail: String::new(),
        }
    }
}

/// Suggestions for a field, from the live system, using the form's current values.
pub async fn choices(
    id: &str,
    key: &str,
    values: &HashMap<String, String>,
) -> Result<Vec<Choice>, SettingsError> {
    let url = values
        .get("url")
        .map(|u| u.trim().trim_end_matches('/'))
        .filter(|u| !u.is_empty());
    let url = url.ok_or_else(|| SettingsError::Invalid("enter the URL first".into()))?;
    match (id, key) {
        ("lyrion", "player") => modules::lyrion::player_choices(url)
            .await
            .map_err(|e| SettingsError::Io(format!("Lyrion at {url}: {e:#}"))),
        ("homeassistant", "area") => {
            let table = Store::current_table_for_choices(values);
            let token = std::fs::read_to_string(secret_path(Some(&table)))
                .map_err(|_| SettingsError::Invalid("store the access token first".into()))?;
            let config = modules::homeassistant::Config::for_area(url, secret_path(Some(&table)), "");
            modules::homeassistant::area_names(&config, token.trim())
                .await
                .map(|names| names.into_iter().map(Choice::plain).collect())
                .map_err(|e| SettingsError::Io(format!("Home Assistant at {url}: {e:#}")))
        }
        _ => Err(SettingsError::NotFound(format!("{id}.{key} has no suggestions"))),
    }
}

impl Store {
    /// A table with the form's `token_file`, to find the token while editing.
    fn current_table_for_choices(values: &HashMap<String, String>) -> toml::Table {
        let mut t = toml::Table::new();
        if let Some(file) = values.get("token_file").filter(|f| !f.trim().is_empty()) {
            t.insert("token_file".into(), file.trim().into());
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path, text: Option<&str>) -> Store {
        let path = dir.join("config.toml");
        if let Some(text) = text {
            std::fs::write(&path, text).unwrap();
        }
        Store::new(path, vec![("listen", "Listen address", "127.0.0.1:7315".into())])
    }

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sd-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The specs must stay in sync with the modules' parsers: a table with every non-secret field
    /// set must be accepted.
    #[test]
    fn specs_match_the_module_parsers() {
        for spec in specs() {
            let mut table = toml::Table::new();
            for f in spec.fields.iter().filter(|f| f.kind != Kind::Secret) {
                let sample = match f.kind {
                    Kind::Url => "http://127.0.0.1:1",
                    Kind::Integer => "1",
                    _ => {
                        if f.default.is_empty() {
                            "x"
                        } else {
                            f.default
                        }
                    }
                };
                table.insert(f.key.into(), typed(f, sample).unwrap());
            }
            validate(spec.id, &table, None).unwrap_or_else(|e| panic!("{}: {e}", spec.id));
        }
    }

    #[test]
    fn update_validates_writes_and_keeps_comments() {
        let dir = tmpdir("update");
        let s = store(
            &dir,
            Some(
                "# my comment\nlisten = \"0.0.0.0:7315\"\n\n[modules.lyrion]\n# player comment\nurl = \"http://a:9000\"\nplayer = \"Old\"\n",
            ),
        );
        let values = HashMap::from([
            ("url".into(), "http://b:9000".into()),
            ("player".into(), "Buero Essence".into()),
            ("prefix".into(), String::new()),
        ]);
        s.update("lyrion", true, &values).unwrap();
        let text = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(
            text.contains("# my comment") && text.contains("# player comment"),
            "{text}"
        );
        assert!(
            text.contains("player = \"Buero Essence\"") && text.contains("url = \"http://b:9000\""),
            "{text}"
        );
        assert!(!text.contains("prefix"), "empty = unset");

        let got = s.get().unwrap();
        let lyrion = got.modules.iter().find(|m| m.id == "lyrion").unwrap();
        assert!(lyrion.configured && lyrion.enabled && lyrion.error.is_empty());
        assert_eq!(lyrion.values["player"], "Buero Essence");

        // Invalid values are refused and nothing is written.
        let bad = HashMap::from([("url".into(), "ftp://x".into()), ("player".into(), "P".into())]);
        assert!(matches!(
            s.update("lyrion", true, &bad),
            Err(SettingsError::Invalid(_))
        ));
        assert!(
            std::fs::read_to_string(dir.join("config.toml"))
                .unwrap()
                .contains("http://b:9000")
        );
        let unknown = HashMap::from([("colour".into(), "red".into())]);
        assert!(s.update("lyrion", true, &unknown).is_err());
        assert!(matches!(
            s.update("nope", true, &HashMap::new()),
            Err(SettingsError::NotFound(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_module_table_is_created_and_disabled_ones_need_not_be_valid() {
        let dir = tmpdir("new");
        let s = store(&dir, None);
        s.update("homeassistant", false, &HashMap::new()).unwrap();
        let text = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(
            text.contains("[modules.homeassistant]") && text.contains("enabled = false"),
            "{text}"
        );
        assert!(
            matches!(
                s.update("homeassistant", true, &HashMap::new()),
                Err(SettingsError::Invalid(_))
            ),
            "url missing"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn secrets_are_write_only_and_private() {
        let dir = tmpdir("secret");
        let token_file = dir.join("ha.token");
        let s = store(
            &dir,
            Some(&format!(
                "[modules.homeassistant]\nurl = \"https://ha\"\narea = \"x\"\ntoken_file = \"{}\"\n",
                token_file.display()
            )),
        );
        let ha = |s: &Store| {
            s.get()
                .unwrap()
                .modules
                .into_iter()
                .find(|m| m.id == "homeassistant")
                .unwrap()
        };
        assert!(
            !ha(&s)
                .fields
                .iter()
                .find(|f| f.key == "token")
                .unwrap()
                .secret_set
        );
        s.set_secret("homeassistant", "token", "abc.def.ghi").unwrap();
        assert_eq!(std::fs::read_to_string(&token_file).unwrap(), "abc.def.ghi\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&token_file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let module = ha(&s);
        assert!(
            module
                .fields
                .iter()
                .find(|f| f.key == "token")
                .unwrap()
                .secret_set
        );
        assert!(!module.values.contains_key("token"), "never returned");
        assert!(s.set_secret("homeassistant", "token", "two words").is_err());
        assert!(s.set_secret("lyrion", "token", "x").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

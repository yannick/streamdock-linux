//! Home Assistant modes (H15-18, tabs H15-32): lights and other entities on the LCD keys.
//!
//! ```toml
//! [modules.homeassistant]
//! url = "http://homeassistant.local:8123"
//! token_file = "~/.config/streamdockd/homeassistant.token" # long-lived access token (default)
//! area = "Gäste Büro"           # first tab (id "homeassistant"): the area's lights; name or id
//! title = "Lights"                # tab name (default "Lights")
//!
//! [[modules.homeassistant.tab]]   # more tabs, each its own mode
//! id = "living"                   # stable id (profiles refer to it); default homeassistant-2, -3 …
//! title = "Living room"
//! keys = [ { key = 1, entity = "light.sofa" },          # entities pinned to keys instead of an
//!          { key = 2, entity = "scene.movie", label = "Movie" } ]   # area: any domain
//! knob1 = "brightness"            # brightness | color_temp | none (defaults: brightness, color_temp)
//! knob2 = "none"
//! level_keys = [ { key = 2, show = "brightness", label = "Hell" },   # bars (H15-97): brightness
//!                { key = 3, show = "color_temp", label = "Farbe" } ] # or colour temperature
//! ```
//!
//! A tab shows either an area's lights (sorted by name) or the entities pinned with `keys`. Press =
//! toggle (lights, switches …) or activate (scenes, scripts, buttons). Knob turns change the
//! brightness or color temperature of the lights that are on (batched); a knob press switches all
//! lights on or off. With every light off, turning the brightness knob right switches all lights of
//! the tab on from 1 %, and turning it down to 0 switches them off (H15-89); colour temperature
//! turns never switch a light on. `level_keys` show the brightness (mean of the lights that are on)
//! and colour temperature as bars that follow a knob turn at once, and a light that is on shows its
//! brightness as a strip along the bottom of its key. The token is read from its file only (never
//! logged); without it the keys say so, and the file is checked again every 10 s, so creating it
//! needs no restart. Every tab keeps its own connection.

mod lights;
mod session;
mod setup;

pub use session::{EntityInfo, area_names, key_entities, tab_entities};

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, bail};

pub use lights::{KnobFn, Pin};

use super::{FaceState, Module, ModuleEvent, ModuleIo, ModuleTask, Page, face};

/// Default location of the long-lived access token.
const DEFAULT_TOKEN_FILE: &str = "~/.config/streamdockd/homeassistant.token";
/// How often a missing token file is checked again.
const TOKEN_RETRY: Duration = Duration::from_secs(10);
/// Reconnect backoff after a lost or failed connection.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// One tab: the connection and what the keys show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Base URL, e.g. `http://homeassistant.local:8123`.
    pub url: String,
    pub token_file: PathBuf,
    /// Mode id and name.
    pub id: String,
    pub title: String,
    /// Whose lights fill the keys when nothing is pinned; may be empty when `pins` are set.
    pub area: String,
    pub pins: Vec<Pin>,
    /// `keys` is set (possibly empty): show the pinned entities, not the area's lights.
    pub pinned: bool,
    pub knobs: [KnobFn; 2],
    /// Keys showing the brightness / colour temperature as bars (H15-97).
    pub levels: Vec<lights::LevelKey>,
}

/// Keys of a tab (top level and `[[tab]]`).
const TAB_KEYS: [&str; 7] = ["id", "title", "area", "keys", "knob1", "knob2", "level_keys"];

impl Config {
    /// A tab with an area and the default knobs (also the connection used by other modules).
    #[must_use]
    pub fn for_area(url: &str, token_file: PathBuf, area: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_owned(),
            token_file,
            id: "homeassistant".into(),
            title: "Lights".into(),
            area: area.to_owned(),
            pins: Vec::new(),
            pinned: false,
            knobs: [KnobFn::Brightness, KnobFn::ColorTemp],
            levels: Vec::new(),
        }
    }

    /// The connection and the first tab of `[modules.homeassistant]`. The area may be missing
    /// (other modules only need `url` and `token_file`); [`tabs`] checks what a tab needs.
    pub fn from_table(table: &toml::Table) -> anyhow::Result<Self> {
        let at = "[modules.homeassistant]";
        let url = string(table, "url", at)?
            .with_context(|| format!("{at} url is missing"))?
            .trim_end_matches('/')
            .to_owned();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            bail!("{at} url must start with https:// or http://");
        }
        let token_file =
            expand_home(&string(table, "token_file", at)?.unwrap_or_else(|| DEFAULT_TOKEN_FILE.into()));
        for key in table.keys() {
            if !["enabled", "url", "token_file", "tab", "assist"].contains(&key.as_str())
                && !TAB_KEYS.contains(&key.as_str())
            {
                bail!("{at} unknown key {key:?}");
            }
        }
        let mut config = Self::for_area(&url, token_file, "");
        config.read_tab(table, at, "homeassistant", "Lights")?;
        Ok(config)
    }

    /// Reads the tab keys of `table` (missing ones keep the defaults given).
    fn read_tab(&mut self, table: &toml::Table, at: &str, id: &str, title: &str) -> anyhow::Result<()> {
        self.id = string(table, "id", at)?.unwrap_or_else(|| id.to_owned());
        anyhow::ensure!(!self.id.is_empty(), "{at} id must not be empty");
        self.title = string(table, "title", at)?.unwrap_or_else(|| title.to_owned());
        self.area = string(table, "area", at)?.unwrap_or_default();
        for (i, key) in ["knob1", "knob2"].into_iter().enumerate() {
            if let Some(f) = string(table, key, at)? {
                self.knobs[i] = KnobFn::parse(&f).map_err(|e| anyhow::anyhow!("{at} {key}: {e}"))?;
            }
        }
        self.levels = level_keys(table, at)?;
        self.pins = Vec::new();
        self.pinned = table.contains_key("keys");
        if let Some(keys) = table.get("keys") {
            let keys = keys
                .as_array()
                .with_context(|| format!("{at} keys must be a list of {{ key, entity, label }}"))?;
            for k in keys {
                let k = k
                    .as_table()
                    .with_context(|| format!("{at} keys: each entry must be a table"))?;
                for name in k.keys() {
                    if !["key", "entity", "label", "action", "service", "icon"].contains(&name.as_str()) {
                        bail!(
                            "{at} keys: unknown setting {name:?} (key, entity, label, action, service, icon)"
                        );
                    }
                }
                let key = k
                    .get("key")
                    .and_then(toml::Value::as_integer)
                    .and_then(|n| u8::try_from(n).ok())
                    .filter(|n| (1..=6).contains(n))
                    .with_context(|| format!("{at} keys: key must be 1-6"))?;
                anyhow::ensure!(
                    !self.pins.iter().any(|p| p.key == key),
                    "{at} keys: key {key} is pinned twice"
                );
                let entity = string(k, "entity", at)?
                    .with_context(|| format!("{at} keys: key {key} needs an entity"))?;
                anyhow::ensure!(
                    entity.contains('.') && !entity.contains(char::is_whitespace),
                    "{at} keys: {entity:?} is not an entity id (like light.desk)"
                );
                let pin = Pin {
                    key,
                    entity,
                    label: string(k, "label", at)?,
                    action: string(k, "action", at)?.unwrap_or_default(),
                    service: string(k, "service", at)?.unwrap_or_default(),
                    icon: string(k, "icon", at)?.unwrap_or_default(),
                };
                pin.validate().map_err(|e| anyhow::anyhow!("{at} keys: {e}"))?;
                self.pins.push(pin);
            }
        }
        Ok(())
    }

    /// Whether the tab has something to show.
    fn shows_something(&self) -> bool {
        !self.area.is_empty() || self.pinned
    }

    /// `wss://…/api/websocket` (or `ws://` for plain http).
    fn websocket_url(&self) -> String {
        let rest = self.url.strip_prefix("https://").map_or_else(
            || format!("ws://{}", self.url.trim_start_matches("http://")),
            |host| format!("wss://{host}"),
        );
        format!("{rest}/api/websocket")
    }
}

/// `level_keys = [{ key = 2, show = "brightness", label = "Hell" }, …]` (H15-97).
fn level_keys(table: &toml::Table, at: &str) -> anyhow::Result<Vec<lights::LevelKey>> {
    let Some(list) = table.get("level_keys") else {
        return Ok(Vec::new());
    };
    let list = list
        .as_array()
        .with_context(|| format!("{at} level_keys must be a list of {{ key, show, label }}"))?;
    let mut out: Vec<lights::LevelKey> = Vec::new();
    for entry in list {
        let t = entry
            .as_table()
            .with_context(|| format!("{at} level_keys: each entry must be a table"))?;
        for name in t.keys() {
            if !["key", "show", "label"].contains(&name.as_str()) {
                bail!("{at} level_keys: unknown setting {name:?} (key, show, label)");
            }
        }
        let key = t
            .get("key")
            .and_then(toml::Value::as_integer)
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| (1..=6).contains(n))
            .with_context(|| format!("{at} level_keys: key must be 1-6"))?;
        anyhow::ensure!(
            !out.iter().any(|l| l.key == key),
            "{at} level_keys: key {key} twice"
        );
        let show = lights::LevelShow::parse(&string(t, "show", at)?.unwrap_or_default())
            .map_err(|e| anyhow::anyhow!("{at} level_keys: {e}"))?;
        let label = string(t, "label", at)?.unwrap_or_else(|| show.default_label().to_owned());
        out.push(lights::LevelKey { key, show, label });
    }
    Ok(out)
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// Reads the token; the error says what to do, never what the token is.
pub(crate) fn read_token(path: &std::path::Path) -> Result<String, String> {
    let token = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let token = token.trim().to_owned();
    if token.is_empty() {
        return Err(format!("{} is empty", path.display()));
    }
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(path) {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o077 != 0 {
            tracing::warn!(file = %path.display(), "token file is readable by others; chmod 600 it");
        }
    }
    Ok(token)
}

fn string(table: &toml::Table, key: &str, at: &str) -> anyhow::Result<Option<String>> {
    table
        .get(key)
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .with_context(|| format!("{at} {key} must be a string"))
        })
        .transpose()
}

/// Every tab of `[modules.homeassistant]`: the top level (when it has an area or keys) and each
/// `[[tab]]`. Ids must be unique.
pub fn tabs(table: &toml::Table) -> anyhow::Result<Vec<Config>> {
    let first = Config::from_table(table)?;
    let mut tabs = Vec::new();
    if first.shows_something() {
        tabs.push(first.clone());
    }
    if let Some(list) = table.get("tab") {
        let list = list
            .as_array()
            .context("[modules.homeassistant] tab must be a list of [[modules.homeassistant.tab]] tables")?;
        for (i, t) in list.iter().enumerate() {
            let n = i + 2;
            let at = format!("[[modules.homeassistant.tab]] #{}", i + 1);
            let t = t.as_table().with_context(|| format!("{at} must be a table"))?;
            for key in t.keys() {
                if !TAB_KEYS.contains(&key.as_str()) {
                    bail!("{at} unknown key {key:?} (known: {})", TAB_KEYS.join(", "));
                }
            }
            let mut tab = first.clone();
            tab.knobs = [KnobFn::Brightness, KnobFn::ColorTemp];
            tab.read_tab(t, &at, &format!("homeassistant-{n}"), &format!("Lights {n}"))?;
            anyhow::ensure!(tab.shows_something(), "{at} needs an area or keys");
            tabs.push(tab);
        }
    }
    for (i, t) in tabs.iter().enumerate() {
        anyhow::ensure!(
            !tabs[..i].iter().any(|o| o.id == t.id),
            "[modules.homeassistant] two tabs have the id {:?}",
            t.id
        );
    }
    Ok(tabs)
}

/// Parses tabs and checks image references against the store belonging to this config.
/// The connection-only Config parser is also used by Assist and does not resolve images.
pub fn checked_tabs(
    table: &toml::Table,
    assets: Option<&crate::assets::Assets>,
) -> anyhow::Result<Vec<Config>> {
    let tabs = tabs(table)?;
    for tab in &tabs {
        for pin in &tab.pins {
            anyhow::ensure!(
                pin.icon.is_empty()
                    || crate::icons::exists(&pin.icon)
                    || assets.is_some_and(|a| a.get(&pin.icon).is_some()),
                "Home Assistant tab {:?} key {}: unknown icon {:?} (built-in or uploaded image required)",
                tab.id,
                pin.key,
                pin.icon
            );
        }
    }
    Ok(tabs)
}

pub fn from_table(
    table: &toml::Table,
    assets: Option<&crate::assets::Assets>,
) -> anyhow::Result<Vec<Box<dyn Module>>> {
    Ok(checked_tabs(table, assets)?
        .into_iter()
        .map(|config| Box::new(HomeAssistant { config }) as Box<dyn Module>)
        .collect())
}

#[derive(Debug)]
pub struct HomeAssistant {
    config: Config,
}

/// Reports problems to the log and the activity feed once each, so a retry loop doesn't repeat
/// the same entry every few seconds.
#[derive(Debug, Default)]
struct Problems {
    last: Option<String>,
}

impl Problems {
    fn report(&mut self, io: &ModuleIo, text: String) {
        if self.last.as_ref() == Some(&text) {
            tracing::debug!(problem = %text, "Home Assistant (unchanged)");
            return;
        }
        tracing::warn!(problem = %text, "Home Assistant");
        io.activity.error(text.clone());
        self.last = Some(text);
    }

    fn clear(&mut self) {
        self.last = None;
    }
}

/// A one-key status page (no token, offline, wrong area …).
fn status_page(label: &str, detail: &str) -> Page {
    Page {
        now_playing: None,
        faces: vec![face(1, label, detail, FaceState::FACE_STATE_UNAVAILABLE)],
        knobs: Vec::new(),
    }
}

/// Waits `delay`, dropping input meanwhile (there is nothing to control). `false` if the daemon
/// is stopping.
async fn wait(io: &mut ModuleIo, delay: Duration) -> bool {
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            () = &mut sleep => return true,
            event = io.events.recv() => match event {
                Some(ModuleEvent::Activated) => io.page.send_modify(|_| {}),
                Some(_) => {}
                None => return false,
            },
        }
    }
}

impl Module for HomeAssistant {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn title(&self) -> &str {
        &self.config.title
    }

    fn kind(&self) -> &'static str {
        "lights"
    }

    fn run(self: Box<Self>, mut io: ModuleIo) -> ModuleTask {
        Box::pin(async move {
            let mut backoff = BACKOFF_MIN;
            let mut problems = Problems::default();
            loop {
                let token = match read_token(&self.config.token_file) {
                    Ok(token) => token,
                    Err(reason) => {
                        problems.report(&io, format!("no token: {reason}"));
                        io.page.send_replace(status_page("No HA token", "see H15-14"));
                        if !wait(&mut io, TOKEN_RETRY).await {
                            return;
                        }
                        continue;
                    }
                };
                match session::run(&self.config, &token, &mut io).await {
                    Ok(session::End::Stopping) => return,
                    Ok(session::End::Connected) => {
                        backoff = BACKOFF_MIN;
                        // The session was connected, so this is news even if it was the last problem.
                        problems.clear();
                        problems.report(&io, "connection lost; reconnecting".to_owned());
                    }
                    Err(e) => {
                        problems.report(&io, format!("{e:#}"));
                        let detail = if e.to_string().contains("auth") {
                            "token rejected"
                        } else {
                            "offline"
                        };
                        io.page.send_replace(status_page("Home Assistant", detail));
                    }
                }
                if !wait(&mut io, backoff).await {
                    return;
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::Input;

    fn table(src: &str) -> toml::Table {
        toml::from_str(src).unwrap()
    }

    #[test]
    fn config_defaults_and_websocket_url() {
        let c = Config::from_table(&table("url = \"https://ha.example/\"\narea = \"Büro\"\n")).unwrap();
        assert_eq!(c.websocket_url(), "wss://ha.example/api/websocket");
        assert!(c.token_file.ends_with(".config/streamdockd/homeassistant.token"));
        let plain = Config::from_table(&table("url = \"http://127.0.0.1:8123\"\narea = \"x\"\n")).unwrap();
        assert_eq!(plain.websocket_url(), "ws://127.0.0.1:8123/api/websocket");
    }

    #[test]
    fn level_keys() {
        let c = Config::from_table(&table(
            "url = \"https://x\"\narea = \"x\"\nlevel_keys = [{ key = 2, show = \"brightness\", label = \"Hell\" }, { key = 3, show = \"color_temp\" }]",
        ))
        .unwrap();
        assert_eq!(
            c.levels,
            [
                lights::LevelKey {
                    key: 2,
                    show: lights::LevelShow::Brightness,
                    label: "Hell".into()
                },
                lights::LevelKey {
                    key: 3,
                    show: lights::LevelShow::ColorTemp,
                    label: "Color temp".into()
                },
            ]
        );
        for bad in [
            "[{ key = 7, show = \"brightness\" }]",
            "[{ key = 2, show = \"hue\" }]",
            "[{ key = 2, show = \"brightness\", colour = 1 }]",
            "[{ key = 2, show = \"brightness\" }, { key = 2, show = \"color_temp\" }]",
        ] {
            let src = format!("url = \"https://x\"\narea = \"x\"\nlevel_keys = {bad}");
            assert!(Config::from_table(&table(&src)).is_err(), "{bad}");
        }
    }

    #[test]
    fn config_errors() {
        assert!(Config::from_table(&table("area = \"x\"")).is_err(), "url missing");
        assert!(Config::from_table(&table("url = \"ftp://x\"\narea = \"x\"")).is_err());
        assert!(Config::from_table(&table("url = \"https://x\"\narea = \"x\"\ncolour = 1")).is_err());
    }

    #[test]
    fn tabs_with_areas_or_pinned_entities() {
        let t = table(
            r#"
            url = "https://ha"
            area = "Office"
            [[tab]]
            id = "living"
            title = "Living room"
            keys = [ { key = 1, entity = "light.sofa" }, { key = 3, entity = "scene.movie", label = "Movie" } ]
            knob1 = "none"
            [[tab]]
            area = "Kitchen"
            "#,
        );
        let found = super::tabs(&t).unwrap();
        let ids: Vec<_> = found.iter().map(|t| (t.id.as_str(), t.title.as_str())).collect();
        assert_eq!(
            ids,
            [
                ("homeassistant", "Lights"),
                ("living", "Living room"),
                ("homeassistant-3", "Lights 3")
            ]
        );
        assert_eq!(found[1].pins[1].label.as_deref(), Some("Movie"));
        assert_eq!(found[1].knobs, [KnobFn::None, KnobFn::ColorTemp]);
        assert_eq!(found[2].url, "https://ha", "tabs share the connection");

        // Connection only: no tab, but other modules can use it.
        let conn = table("url = \"https://ha\"");
        assert_eq!(tabs(&conn).unwrap(), []);
        assert_eq!(Config::from_table(&conn).unwrap().url, "https://ha");

        for (bad, why) in [
            (
                "url = \"https://ha\"\n[[tab]]\ntitle = \"x\"",
                "needs an area or keys",
            ),
            (
                "url = \"https://ha\"\narea = \"a\"\n[[tab]]\nid = \"homeassistant\"\narea = \"b\"",
                "two tabs",
            ),
            (
                "url = \"https://ha\"\nkeys = [ { key = 9, entity = \"light.x\" } ]",
                "1-6",
            ),
            (
                "url = \"https://ha\"\nkeys = [ { key = 1, entity = \"desk\" } ]",
                "entity id",
            ),
            (
                "url = \"https://ha\"\narea = \"a\"\nknob1 = \"volume\"",
                "knob function",
            ),
        ] {
            let err = format!("{:#}", tabs(&table(bad)).unwrap_err());
            assert!(err.contains(why), "{why:?} not in {err:?}");
        }
    }

    #[test]
    fn cover_actions_are_validated_and_update_every_pin() {
        let config = Config::from_table(&table(
            r#"
            url = "https://ha"
            keys = [
                {key=4, entity="cover.test", label="Up", action="open"},
                {key=5, entity="cover.test", label="Stop", action="stop"},
                {key=6, entity="cover.test", label="Down", action="close"}
            ]
        "#,
        ))
        .unwrap();
        let mut lights = lights::pinned(
            &config.pins,
            &serde_json::json!([]),
            &serde_json::json!([
                {"entity_id":"cover.test", "state":"closed", "attributes":{"current_position":0}}
            ]),
        );
        let press = |lights: &lights::Lights, key| lights.on_input(Input::Key { key, pressed: true });
        assert_eq!(press(&lights, 4)[0].service, "open_cover");
        assert_eq!(press(&lights, 5)[0].service, "stop_cover");
        assert_eq!(press(&lights, 6)[0].service, "close_cover");
        assert!(lights.update(
            "cover.test",
            &serde_json::json!({
                "state":"opening", "attributes":{"current_position":42}
            })
        ));
        for key in [4, 5, 6] {
            let calls = press(&lights, key);
            assert_eq!(calls[0].service, "stop_cover");
            assert_eq!(calls[0].entities, ["cover.test"]);
            assert_eq!(calls[0].data, serde_json::json!({}));
        }
        let page = lights.page();
        assert_eq!(page.faces[0].image, "blinds-up");
        assert_eq!(page.faces[1].image, "blinds-stop");
        assert_eq!(page.faces[2].image, "blinds-down");
        assert!(
            page.faces
                .iter()
                .all(|f| f.detail.contains("42 %") && f.detail.contains("opening"))
        );
        assert_eq!(page.faces[0].state, FaceState::FACE_STATE_ON);
        assert_eq!(page.faces[2].state, FaceState::FACE_STATE_IDLE);
        lights.update(
            "cover.test",
            &serde_json::json!({"state":"closing", "attributes":{"current_position":20}}),
        );
        assert_eq!(lights.page().faces[2].state, FaceState::FACE_STATE_ON);
        assert_eq!(lights.page().faces[0].state, FaceState::FACE_STATE_IDLE);
        assert_eq!(press(&lights, 4)[0].service, "stop_cover");
        lights.update(
            "cover.test",
            &serde_json::json!({"state":"open", "attributes":{"current_position":75}}),
        );
        assert_eq!(press(&lights, 4)[0].service, "open_cover");
        assert_eq!(press(&lights, 6)[0].service, "close_cover");
        lights.update("cover.test", &serde_json::json!({"state":"unavailable"}));
        assert!([4, 5, 6].iter().all(|k| press(&lights, *k).is_empty()));
        assert_eq!(
            lights.on_input(Input::Key {
                key: 4,
                pressed: false
            }),
            []
        );
    }

    #[test]
    fn pin_services_have_no_free_data_and_actions_are_cover_only() {
        for bad in [
            r#"entity="light.test", action="open""#,
            r#"entity="cover.test", action="toggle""#,
            r#"entity="cover.test", action="open", service="cover.open_cover""#,
            r#"entity="switch.test", service="bad service""#,
            r#"entity="switch.test", service="switch.turn_on.extra""#,
            r#"entity="switch.test", service="switch.turn_on", data={entity_id="switch.other"}"#,
        ] {
            assert!(
                Config::from_table(&table(&format!("url=\"https://ha\"\nkeys=[{{key=1, {bad}}}]"))).is_err(),
                "{bad}"
            );
        }
        let config = Config::from_table(&table(
            r#"url="https://ha"
            keys=[{key=1,entity="switch.test",service="switch.turn_on"}]"#,
        ))
        .unwrap();
        let lights = lights::pinned(
            &config.pins,
            &serde_json::json!([]),
            &serde_json::json!([
                {"entity_id":"switch.test", "state":"off"}
            ]),
        );
        let calls = lights.on_input(Input::Key {
            key: 1,
            pressed: true,
        });
        assert_eq!(calls[0].domain, "switch");
        assert_eq!(calls[0].service, "turn_on");
        assert_eq!(calls[0].entities, ["switch.test"]);
        assert_eq!(calls[0].data, serde_json::json!({}));
    }

    #[test]
    fn module_parser_accepts_uploaded_icons_and_rejects_unknown_names() {
        let dir = std::env::temp_dir().join(format!("ha-icons-{}", std::process::id()));
        let assets = std::sync::Arc::new(crate::assets::Assets::new(dir.clone()));
        assets
            .put("family-lamp", &crate::icons::preview("home").unwrap())
            .unwrap();
        // A same-name upload is also valid; rendering already prefers it over the built-in.
        assets
            .put("light-on", &crate::icons::preview("home").unwrap())
            .unwrap();
        for (icon, valid) in [
            ("family-lamp", true),
            ("light-on", true),
            ("not-an-icon", false),
            ("../home", false),
        ] {
            let table = table(&format!(
                "url=\"https://ha\"\nkeys=[{{key=1,entity=\"light.test\",icon={icon:?}}}]"
            ));
            let tables = std::collections::BTreeMap::from([("homeassistant".to_owned(), table)]);
            let hub = crate::hub::Hub::new();
            hub.set_assets(std::sync::Arc::clone(&assets));
            let context = crate::modules::Context::new(hub, &tables);
            assert_eq!(
                crate::modules::from_config(&tables, &context).is_ok(),
                valid,
                "{icon}"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn token_file_errors_never_contain_the_token() {
        let dir = std::env::temp_dir().join(format!("sd-ha-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("token");
        std::fs::write(&path, "  sekrit-token \n").unwrap();
        assert_eq!(read_token(&path).unwrap(), "sekrit-token");
        std::fs::write(&path, "").unwrap();
        assert!(read_token(&path).unwrap_err().contains("empty"));
        let missing = read_token(&dir.join("nope")).unwrap_err();
        assert!(missing.contains("cannot read") && !missing.contains("sekrit"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

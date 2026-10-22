//! Modules: integrations that own the dock while their mode is active (H15-16).
//!
//! A module is a task. It gets [`ModuleEvent`]s (input from the dock while it is the active mode,
//! plus activation changes) and publishes a [`Page`]: one [`KeyFace`] per LCD key it uses and the
//! role of each knob. The mode controller (`crate::modes`) renders the active page onto every
//! attached dock and shows it in the web UI. Knob 3 always belongs to the controller (mode switch).
//!
//! Modules are enabled in the config file, one table each:
//!
//! ```toml
//! [modules.demo]
//! enabled = true
//! pages = 2          # module-specific settings follow
//! ```

pub mod demo;
pub mod homeassistant;
pub mod keys;
pub mod lyrion;
pub mod settings;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use anyhow::{Context as _, bail};
use streamdock_proto::streamdock::v1 as pb;
use tokio::sync::{mpsc, watch};

pub use pb::{FaceState, KeyFace, KnobRole, LevelStyle};

/// Input from the dock, routed to the active module. Knob 3 never arrives here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// Keys 1-6 (LCD) and 7-9 (buttons).
    Key { key: u8, pressed: bool },
    /// Knobs 1-2: detents, positive clockwise.
    KnobTurn { knob: u8, delta: i32 },
    /// Knobs 1-2.
    KnobPress { knob: u8, pressed: bool },
}

/// What a module receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleEvent {
    /// The user pressed or turned something while this module is the active mode.
    Input(Input),
    /// This module became the active mode.
    Activated,
    /// Another mode became active.
    Deactivated,
}

/// What a module shows: key faces and knob roles. Keys without a face stay blank.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    pub faces: Vec<KeyFace>,
    pub knobs: Vec<KnobRole>,
    /// Music tabs: the current track (H15-88), for the web UI's now-playing component.
    pub now_playing: Option<streamdock_proto::streamdock::v1::NowPlaying>,
}

/// A module's end of the channels to the mode controller.
#[derive(Debug)]
pub struct ModuleIo {
    pub events: mpsc::Receiver<ModuleEvent>,
    pub page: watch::Sender<Page>,
    /// The activity feed (H15-21): report actions, their results and status changes here.
    pub activity: Activity,
}

/// A module's handle to the activity feed shown in the web UI. Cheap to clone.
///
/// Report what the user will want to see: an action and its outcome ("toggle Deckenspots" ok /
/// failed: reason) and status changes (connected, offline). Don't report every poll.
#[derive(Debug, Clone)]
pub struct Activity {
    hub: Option<std::sync::Arc<crate::hub::Hub>>,
    source: String,
}

impl Activity {
    #[must_use]
    pub fn new(hub: std::sync::Arc<crate::hub::Hub>, source: &str) -> Self {
        Self {
            hub: Some(hub),
            source: source.to_owned(),
        }
    }

    /// A handle that reports nowhere, for tests.
    #[cfg(test)]
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            hub: None,
            source: String::new(),
        }
    }

    /// Something happened or was started.
    pub fn info(&self, text: impl Into<String>) {
        self.report(pb::ActivityLevel::ACTIVITY_LEVEL_INFO, text.into());
    }

    /// An action succeeded.
    pub fn ok(&self, text: impl Into<String>) {
        self.report(pb::ActivityLevel::ACTIVITY_LEVEL_OK, text.into());
    }

    /// An action failed or something broke; say why.
    pub fn error(&self, text: impl Into<String>) {
        self.report(pb::ActivityLevel::ACTIVITY_LEVEL_ERROR, text.into());
    }

    fn report(&self, level: pb::ActivityLevel, text: String) {
        if let Some(hub) = &self.hub {
            hub.report(&self.source, level, text);
        }
    }
}

/// A future a module runs as, until the daemon stops.
pub type ModuleTask = Pin<Box<dyn Future<Output = ()> + Send>>;

/// An integration that can be shown on the dock as a mode.
pub trait Module: Send + 'static {
    /// Stable id, e.g. `"homeassistant"`; unique among the enabled modules.
    fn id(&self) -> &str;
    /// Shown as the mode name, e.g. `"Lights"`.
    fn title(&self) -> &str;
    /// What kind of tab the UI shows it as: "lights", "music", "keys", "demo" (H15-32).
    fn kind(&self) -> &'static str {
        "other"
    }
    /// The module's task: read `io.events`, publish pages on `io.page`.
    fn run(self: Box<Self>, io: ModuleIo) -> ModuleTask;
}

/// What modules may use besides their own table: the hub (e.g. to set the brightness of every
/// dock), the mode switch (bound once the mode controller runs, after the modules are built), and
/// every module's table (so the keys module can reuse the Home Assistant and Lyrion settings).
#[derive(Debug, Clone)]
pub struct Context {
    pub hub: Arc<crate::hub::Hub>,
    pub modes: Arc<OnceLock<crate::modes::ModeSwitch>>,
    pub tables: BTreeMap<String, toml::Table>,
}

impl Context {
    #[must_use]
    pub fn new(hub: Arc<crate::hub::Hub>, tables: &BTreeMap<String, toml::Table>) -> Self {
        Self {
            hub,
            modes: Arc::default(),
            tables: tables.clone(),
        }
    }
}

/// Builds the modules enabled in the config's `[modules.<id>]` tables, in config order.
pub fn from_config(
    tables: &BTreeMap<String, toml::Table>,
    context: &Context,
) -> anyhow::Result<Vec<Box<dyn Module>>> {
    let mut modules: Vec<Box<dyn Module>> = Vec::new();
    for (id, table) in tables {
        let enabled = match table.get("enabled") {
            None => true,
            Some(v) => v
                .as_bool()
                .with_context(|| format!("[modules.{id}] enabled must be true or false"))?,
        };
        if !enabled {
            continue;
        }
        match id.as_str() {
            "demo" => modules.extend(demo::from_table(table)?),
            "homeassistant" => modules.extend(homeassistant::from_table(
                table,
                context.hub.assets().map(AsRef::as_ref),
            )?),
            "keys" => modules.extend(keys::from_table(table, context)?),
            "lyrion" => modules.extend(lyrion::from_table_with(
                table,
                Some(&context.hub),
                Some(&context.modes),
            )?),
            settings::ID => modules.extend(settings::from_table(
                table,
                context.tables.get("lyrion"),
                Some(&context.hub),
            )?),
            other => bail!(
                "[modules.{other}]: unknown module (known: demo, homeassistant, keys, lyrion, settings)"
            ),
        }
    }
    Ok(modules)
}

/// Puts `modules` in the order of the mode ids in `order` (the config's `mode_order`); modes not
/// listed keep their config order after the listed ones.
pub fn sort_modes(modules: &mut [Box<dyn Module>], order: &[String]) {
    modules.sort_by_key(|m| order.iter().position(|id| id == m.id()).unwrap_or(usize::MAX));
}

/// A key face, for modules' convenience.
#[must_use]
pub fn face(key: u8, label: impl Into<String>, detail: impl Into<String>, state: FaceState) -> KeyFace {
    KeyFace {
        key: u32::from(key),
        label: label.into(),
        detail: detail.into(),
        state: state.into(),
        ..Default::default()
    }
}

/// A knob role, for modules' convenience.
#[must_use]
pub fn knob(knob: u8, label: impl Into<String>) -> KnobRole {
    KnobRole {
        knob: u32::from(knob),
        label: label.into(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Context {
        Context::new(crate::hub::Hub::new(), &BTreeMap::new())
    }

    fn tables(src: &str) -> BTreeMap<String, toml::Table> {
        let doc: toml::Table = toml::from_str(src).unwrap();
        doc.into_iter()
            .map(|(k, v)| (k, v.as_table().unwrap().clone()))
            .collect()
    }

    #[test]
    fn builds_enabled_modules_only() {
        let mods = from_config(&tables("[demo]\npages = 2\n"), &context()).unwrap();
        assert_eq!(
            mods.iter().map(|m| m.id()).collect::<Vec<_>>(),
            ["demo-1", "demo-2"]
        );
        assert!(
            from_config(&tables("[demo]\nenabled = false\n"), &context())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_unknown_modules() {
        let err = from_config(&tables("[nope]\n"), &context())
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("unknown module"), "{err}");
    }
}

//! Hot reload of modules (H15-22): after a settings save, a backup import, or an edit of
//! `config.toml` on disk, the modules are rebuilt from the file and replace the running ones.
//!
//! Only `[modules.*]`, `[[profiles]]` and `[player]` (H15-55) are applied live; the daemon's own settings (listen address, docks,
//! brightness, power) still need a restart. Nothing happens when the module tables are unchanged,
//! so the watcher doesn't restart modules again after a save that was already applied.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use streamdock_proto::streamdock::v1 as pb;

use crate::config::{Profile, Settings};
use crate::hub::{DAEMON, Hub};
use crate::modes::ModeSwitch;
use crate::modules;

/// How often the config file is checked for changes.
const WATCH_INTERVAL: Duration = Duration::from_secs(2);

/// What a reload compares and applies.
pub type Applied = (BTreeMap<String, toml::Table>, Vec<Profile>, Vec<String>);

#[derive(Debug)]
pub struct Reloader {
    hub: Arc<Hub>,
    modes: ModeSwitch,
    /// The module tables and profiles running now; the lock also serializes reloads.
    applied: tokio::sync::Mutex<Applied>,
    /// The built-in player, restarted when `[player]` changes.
    player: Option<Arc<crate::player::BuiltinPlayer>>,
    /// The wake word listener, restarted when its settings change (H15-68).
    wake: Option<Arc<crate::assist::wake::WakeWord>>,
}

impl Reloader {
    #[must_use]
    pub fn new(hub: Arc<Hub>, modes: ModeSwitch, applied: Applied) -> Self {
        Self {
            hub,
            modes,
            applied: tokio::sync::Mutex::new(applied),
            player: None,
            wake: None,
        }
    }

    #[must_use]
    pub fn with_player(mut self, player: Arc<crate::player::BuiltinPlayer>) -> Self {
        self.player = Some(player);
        self
    }

    #[must_use]
    pub fn with_wake(mut self, wake: Arc<crate::assist::wake::WakeWord>) -> Self {
        self.wake = Some(wake);
        self
    }

    /// Applies the config file's modules if they changed; `Ok(true)` if the modules were replaced.
    pub async fn apply(&self) -> anyhow::Result<bool> {
        let mut applied = self.applied.lock().await;
        let settings = Settings::load()?;
        if let Some(player) = &self.player {
            player.apply(&settings.player);
        }
        if let Some(wake) = &self.wake {
            wake.apply(&settings.modules);
        }
        if applied.0 == settings.modules && applied.1 == settings.profiles && applied.2 == settings.mode_order
        {
            return Ok(false);
        }
        // A fresh Context, bound to the running mode switch (the keys module's "mode" action).
        let context = modules::Context::new(Arc::clone(&self.hub), &settings.modules);
        let _ = context.modes.set(self.modes.clone());
        let mut built = modules::from_config(&settings.modules, &context)?;
        modules::sort_modes(&mut built, &settings.mode_order);
        let titles: Vec<String> = built.iter().map(|m| m.title().to_owned()).collect();
        self.modes
            .reload(built, settings.profiles.clone())
            .await
            .context("reloading modules")?;
        *applied = (settings.modules, settings.profiles, settings.mode_order);
        self.hub.report(
            DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            if titles.is_empty() {
                "modules reloaded: none enabled".to_owned()
            } else {
                format!("modules reloaded: {}", titles.join(", "))
            },
        );
        Ok(true)
    }

    /// Applies edits of the config file made outside the UI. Runs until the daemon stops.
    pub async fn watch(self: Arc<Self>, path: PathBuf) {
        let mut seen = modified(&path);
        // The last error reported, so a broken file is reported once, not every interval.
        let mut reported: Option<String> = None;
        loop {
            tokio::time::sleep(WATCH_INTERVAL).await;
            let now = modified(&path);
            if now == seen {
                continue;
            }
            seen = now;
            // Every write lands here (API saves and edits on disk), so screens reload once.
            self.hub.config_changed();
            match self.apply().await {
                Ok(changed) => {
                    reported = None;
                    if changed {
                        tracing::info!(config = %path.display(), "config changed on disk; modules reloaded");
                    }
                }
                Err(e) => {
                    let text = format!("{} not applied, modules keep running: {e:#}", path.display());
                    if reported.as_ref() != Some(&text) {
                        tracing::warn!("{text}");
                        self.hub
                            .report(DAEMON, pb::ActivityLevel::ACTIVITY_LEVEL_ERROR, text.clone());
                        reported = Some(text);
                    }
                }
            }
        }
    }
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

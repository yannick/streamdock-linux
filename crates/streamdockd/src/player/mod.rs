//! The built-in Lyrion player (H15-55): runs the `slimproto` client with `[player]` from the config,
//! publishes its state to the hub (`WatchEventsResponse.player`) and keeps a rename from Lyrion in
//! the state file. Started, restarted and stopped by [`BuiltinPlayer::apply`] on every (re)load.
//!
//! The audio side is the `player` crate (decode + output, H15-47): it plays through `pw-cat` into the
//! native DSP where the daemon manages the audio (`[audio] manage`), else into the default sink, and
//! Lyrion's volume sets the selected output's volume, as the dock's volume knob does.

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use streamdock_proto::streamdock::v1 as pb;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::{OutputChoice, PlayerSettings};
use crate::hub::{DAEMON, Hub};
use crate::state::StateFile;

mod lyrion;
mod message;

use lyrion::{NO_VOLUME, TrackInfo};
pub use message::{settings_from_message, settings_message};

/// The latency asked of PipeWire (H15-47).
const LATENCY: Duration = Duration::from_millis(100);

/// Starts and stops the player as the config changes.
#[derive(Debug)]
pub struct BuiltinPlayer {
    hub: Arc<Hub>,
    state: Arc<StateFile>,
    /// Seeds the player id: the machine and the config file, so a dev instance on the same machine
    /// is a different player than production; plus the state file's generation (a reset id).
    seed: Vec<u8>,
    running: Mutex<Option<Running>>,
}

#[derive(Debug)]
struct Running {
    settings: PlayerSettings,
    /// Shared with [`follow`], so a rename from here isn't taken for one from Lyrion.
    names: Arc<Mutex<Names>>,
    /// Renames the running player without a restart.
    rename: watch::Sender<String>,
    tasks: [JoinHandle<()>; 4],
}

impl Drop for Running {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl BuiltinPlayer {
    #[must_use]
    pub fn new(hub: Arc<Hub>, state: Arc<StateFile>, config_path: &Path) -> Arc<Self> {
        let mut seed = machine_id().into_bytes();
        seed.push(0);
        seed.extend_from_slice(config_path.as_os_str().as_encoded_bytes());
        Arc::new(Self {
            hub,
            state,
            seed,
            running: Mutex::new(None),
        })
    }

    /// The player id in Lyrion, e.g. "02:1a:…".
    #[must_use]
    pub fn player_id(&self) -> String {
        mac_text(self.mac())
    }

    fn mac(&self) -> [u8; 6] {
        let mut seed = self.seed.clone();
        let generation = self.state.player_id_generation();
        if generation > 0 {
            seed.push(0);
            seed.extend_from_slice(generation.to_string().as_bytes());
        }
        slimproto::mac_from_seed(&seed)
    }

    /// The running player's volume limit (`[player] max_volume`, H15-91); `None` while off.
    #[must_use]
    pub fn max_volume(&self) -> Option<u32> {
        let running = self.running.lock().expect("player lock poisoned");
        running.as_ref().map(|r| r.settings.max_volume)
    }

    /// Connects again (the configured server, or discovery). `false` while the player is off.
    pub fn restart(&self) -> bool {
        let mut running = self.running.lock().expect("player lock poisoned");
        let Some(settings) = running.take().map(|r| r.settings.clone()) else {
            return false;
        };
        *running = Some(self.start(&settings));
        true
    }

    /// A new player id (restarting the player if it runs); returns it.
    pub fn reset_id(&self) -> String {
        self.state.next_player_id_generation();
        self.restart();
        let id = self.player_id();
        tracing::info!(id, "new player id");
        id
    }

    /// Runs the player with these settings: starts, renames (only the name changed), restarts or
    /// stops it. Nothing happens when the settings are those it runs with.
    pub fn apply(&self, settings: &PlayerSettings) {
        let mut running = self.running.lock().expect("player lock poisoned");
        if let Some(r) = running.as_mut()
            && settings.enabled
            && same_but_name(&r.settings, settings)
        {
            if r.settings.name != settings.name {
                self.rename(r, &settings.name);
            }
            return;
        }
        let was_running = running.take().is_some();
        if !settings.enabled {
            if was_running {
                self.hub.set_player_state(pb::PlayerState {
                    player_id: self.player_id(),
                    ..Default::default()
                });
                self.hub.report(
                    DAEMON,
                    pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
                    "player off".to_owned(),
                );
            }
            return;
        }
        *running = Some(self.start(settings));
        self.hub.report(
            DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
            format!("player {}", if was_running { "restarted" } else { "started" }),
        );
    }

    /// A new name in the config: the running player takes it at once (and Lyrion with it).
    fn rename(&self, running: &mut Running, name: &str) {
        {
            let mut names = running.names.lock().expect("player lock poisoned");
            name.clone_into(&mut names.configured);
            name.clone_into(&mut names.current);
        }
        self.state.set_player_name(name, name);
        name.clone_into(&mut running.settings.name);
        let _ = running.rename.send(name.to_owned());
        self.hub.report(
            DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
            format!("player renamed to {name:?}"),
        );
    }

    fn start(&self, settings: &PlayerSettings) -> Running {
        let config = slimproto::Config {
            name: self.state.player_name(&settings.name),
            mac: self.mac(),
            server: settings.server,
            max_sample_rate: settings.max_sample_rate,
            firmware: env!("CARGO_PKG_VERSION").to_owned(),
            codecs: settings.formats.clone(),
        };
        let id = mac_text(config.mac);
        tracing::info!(name = %config.name, id, server = ?config.server, "player starting");
        let (tx, rx) = watch::channel(slimproto::PlayerState::default());
        let (rename, names_rx) = watch::channel(config.name.clone());
        let names = Arc::new(Mutex::new(Names {
            configured: settings.name.clone(),
            current: config.name.clone(),
        }));
        let (track_tx, track_rx) = watch::channel(TrackInfo::default());
        let prefs = lyrion::Prefs {
            replay_gain: settings.replay_gain,
            transition: settings.transition,
        };
        let sync = tokio::spawn(lyrion::sync(id.clone(), prefs, rx.clone(), track_tx));
        let follow = tokio::spawn(follow(
            Arc::clone(&self.hub),
            Arc::clone(&self.state),
            Arc::clone(&names),
            id.clone(),
            (rx, track_rx),
        ));
        let output = match &settings.output {
            OutputChoice::Named(name) => Some(name.clone()),
            OutputChoice::Dsp | OutputChoice::Default => None,
        };
        let lyrion_volume = Arc::new(AtomicU32::new(NO_VOLUME));
        let back = tokio::spawn(lyrion::volume_back(
            Arc::clone(&self.hub),
            id,
            output,
            Arc::clone(&lyrion_volume),
        ));
        let playback = playback(&self.hub, settings, lyrion_volume);
        let client = tokio::spawn(slimproto::run_named(config, playback, tx, names_rx));
        Running {
            settings: settings.clone(),
            names,
            rename,
            tasks: [client, follow, back, sync],
        }
    }
}

/// Only the names differ (or nothing): a rename, no restart.
fn same_but_name(a: &PlayerSettings, b: &PlayerSettings) -> bool {
    let b = PlayerSettings {
        name: a.name.clone(),
        ..b.clone()
    };
    *a == b
}

/// The environment for PipeWire clients we start (`pw-cat`, `pw-record`): the managed PipeWire's
/// socket (the board's user `sound`) when this daemon manages the audio.
pub fn pipewire_env(hub: &Hub) -> Vec<(String, String)> {
    hub.audio()
        .iter()
        .map(|a| {
            (
                "PIPEWIRE_RUNTIME_DIR".to_owned(),
                a.runtime_dir().display().to_string(),
            )
        })
        .collect()
}

/// A `pw-cat` device into `output`, shared by the player and the assist answers (H15-43).
pub fn pw_cat(hub: &Hub, output: &OutputChoice) -> player::dsp::DspDevice<player::output::PwCat> {
    let mut env = pipewire_env(hub);
    // H15-62: dedicated realtime client config, only for this child (not pw-dump/wpctl).
    if let Ok(config) = std::env::var("STREAMDOCKD_PW_CAT_CONFIG") {
        env.push(("PIPEWIRE_CONFIG_NAME".to_owned(), config));
    }
    let controls = if matches!(output, OutputChoice::Dsp) {
        hub.audio().map(|audio| audio.dsp_controls())
    } else {
        None
    };
    player::dsp::DspDevice::new(
        player::output::PwCat::new("pw-cat", env, target(hub.audio().cloned(), output), LATENCY),
        controls,
    )
}

/// The audio side: decoder + output into PipeWire, or a silent player if its threads can't start.
fn playback(
    hub: &Arc<Hub>,
    settings: &PlayerSettings,
    lyrion_volume: Arc<AtomicU32>,
) -> Arc<dyn slimproto::Playback> {
    // Dev instances and tests against a real Lyrion: follow it, play nothing (H15-61).
    if std::env::var_os("STREAMDOCKD_PLAYER_SILENT").is_some_and(|v| v != "0") {
        tracing::info!("player plays silently (STREAMDOCKD_PLAYER_SILENT)");
        return Arc::new(slimproto::Silent::default());
    }
    let audio = hub.audio().cloned();
    let device = player::output::resample::ResampleDevice::new(
        pw_cat(hub, &settings.output),
        output_rate(audio.clone(), settings),
        settings.resampling.unwrap_or_default(),
    );
    // The output whose volume is Lyrion's: a fixed one, else the selected one.
    let volume_output = match &settings.output {
        OutputChoice::Named(name) => Some(name.clone()),
        OutputChoice::Dsp | OutputChoice::Default => None,
    };
    // Lyrion is the one source of truth for this player's volume (H15-47): its 0-100 % spans the
    // output's whole range, boost included (the DSP's limiter keeps it from clipping). Above
    // `max_volume` it plays at that (H15-91), and that's what counts as Lyrion's volume here.
    let max_volume = settings.max_volume;
    let on_volume: player::VolumeHandler = Box::new(move |_left, _right, percent| {
        let percent = percent.min(max_volume);
        lyrion_volume.store(percent, Ordering::Relaxed);
        if let Some(audio) = &audio
            && !audio.set_player_volume(volume_output.as_deref(), output_percent(percent))
        {
            tracing::debug!(percent, "player volume: no output selected");
        }
    });
    match player::Player::new(device, settings.effective_output_buffer(), on_volume) {
        Ok(player) => player,
        Err(e) => {
            tracing::warn!(error = %e, "player audio unavailable; playing silently");
            hub.report(
                DAEMON,
                pb::ActivityLevel::ACTIVITY_LEVEL_ERROR,
                format!("player audio unavailable: {e}"),
            );
            Arc::new(slimproto::Silent::default())
        }
    }
}

/// The rate the player converts to (H15-80): the rate PipeWire runs the output at, asked at every
/// track start; `None` (PipeWire converts) without `resampling` or while the rate is unknown.
fn output_rate(
    audio: Option<Arc<crate::audio::Audio>>,
    settings: &PlayerSettings,
) -> player::output::resample::TargetRate {
    let (Some(audio), Some(_)) = (audio, settings.resampling) else {
        return Arc::new(|| None);
    };
    let output = settings.output.clone();
    Arc::new(move || {
        let rate = match &output {
            OutputChoice::Named(name) => audio.output_sample_rate(name),
            OutputChoice::Dsp | OutputChoice::Default => audio.selected_sample_rate(),
        };
        (rate > 0).then_some(rate)
    })
}

/// The PipeWire node `pw-cat` plays into, resolved at every open: a fixed output,
/// or `None` (the default output).
fn target(
    audio: Option<Arc<crate::audio::Audio>>,
    output: &OutputChoice,
) -> Arc<dyn Fn() -> Option<u32> + Send + Sync> {
    match output.clone() {
        OutputChoice::Dsp | OutputChoice::Default => Arc::new(|| None),
        OutputChoice::Named(name) => Arc::new(move || audio.as_ref().and_then(|a| a.output_node_id(&name))),
    }
}

/// The player's name in the config, and the one it runs with (a rename from Lyrion).
#[derive(Debug)]
struct Names {
    configured: String,
    current: String,
}

/// Lyrion's volume (0-100 %) as the output's (0-[`crate::audio::MAX_VOLUME`] %).
pub fn output_percent(lyrion: u32) -> u32 {
    (lyrion.min(100) * crate::audio::MAX_VOLUME + 50) / 100
}

/// The output's volume as Lyrion's, the inverse of [`output_percent`].
pub fn lyrion_percent(output: u32) -> u32 {
    let max = crate::audio::MAX_VOLUME;
    (output.min(max) * 100 + max / 2) / max
}

/// Publishes every state change and remembers renames from Lyrion. Ends with the client.
async fn follow(
    hub: Arc<Hub>,
    state: Arc<StateFile>,
    names: Arc<Mutex<Names>>,
    id: String,
    (mut rx, mut track): (
        watch::Receiver<slimproto::PlayerState>,
        watch::Receiver<TrackInfo>,
    ),
) {
    // The name as last reported by the client: a change to a name we didn't ask for (`current`) is
    // a rename in Lyrion.
    let mut seen = names.lock().expect("player lock poisoned").current.clone();
    loop {
        let current = rx.borrow_and_update().clone();
        if current.name != seen && !current.name.is_empty() {
            seen.clone_from(&current.name);
            let mut names = names.lock().expect("player lock poisoned");
            if current.name != names.current {
                hub.report(
                    DAEMON,
                    pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
                    format!("player renamed in Lyrion to {:?}", current.name),
                );
                state.set_player_name(&names.configured, &current.name);
                names.current.clone_from(&current.name);
            }
        }
        let output_rate = hub.audio().map_or(0, |a| a.selected_sample_rate());
        let mut state = message(&current, &track.borrow_and_update(), &id);
        state.output_sample_rate = output_rate;
        hub.set_player_state(state);
        tokio::select! {
            changed = rx.changed() => if changed.is_err() { return },
            changed = track.changed() => if changed.is_err() { return },
        }
    }
}

fn message(state: &slimproto::PlayerState, track: &TrackInfo, id: &str) -> pb::PlayerState {
    pb::PlayerState {
        format: state.format.clone(),
        output_fill: u32::from(state.output_fill),
        decode_fill: u32::from(state.decode_fill),
        underruns: state.underruns,
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        bitrate: track.bitrate.clone(),
        sample_rate: state.sample_rate,
        source_sample_rate: track.sample_rate,
        enabled: true,
        name: state.name.clone(),
        player_id: id.to_owned(),
        connected: state.connected,
        server: state.server.map(|s| s.to_string()).unwrap_or_default(),
        playing: state.playing,
        paused: state.paused,
        problem: state.problem.clone(),
        ..Default::default()
    }
}

/// `02:1a:…`, as Lyrion shows a player id.
fn mac_text(mac: [u8; 6]) -> String {
    mac.map(|b| format!("{b:02x}")).join(":")
}

/// This machine's id (systemd's `/etc/machine-id`), else its host name.
fn machine_id() -> String {
    ["/etc/machine-id", "/var/lib/dbus/machine-id", "/etc/hostname"]
        .iter()
        .find_map(|path| {
            let text = std::fs::read_to_string(path).ok()?;
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_owned())
        })
        .unwrap_or_else(|| "streamdockd".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lyrion_volume_spans_the_output_range() {
        assert_eq!(output_percent(0), 0);
        assert_eq!(output_percent(50), 75);
        assert_eq!(output_percent(100), 150);
        assert_eq!(output_percent(120), 150, "clamped");
        for v in 0..=100 {
            assert_eq!(lyrion_percent(output_percent(v)), v, "round trip at {v}");
        }
        assert_eq!(lyrion_percent(74), 49);
    }

    #[test]
    fn player_ids_look_like_macs() {
        assert_eq!(mac_text([0x02, 0x1a, 0, 0xff, 0x10, 0x01]), "02:1a:00:ff:10:01");
    }

    #[tokio::test]
    async fn publishes_state_and_keeps_renames() {
        let hub = Hub::new();
        let state = Arc::new(StateFile::in_memory());
        let mut events = hub.subscribe().events;
        let (tx, rx) = watch::channel(slimproto::PlayerState::default());
        let task = tokio::spawn(follow(
            Arc::clone(&hub),
            Arc::clone(&state),
            Arc::new(Mutex::new(Names {
                configured: "StreamDock".into(),
                current: "StreamDock".into(),
            })),
            "02:00:00:00:00:01".into(),
            (rx, watch::channel(TrackInfo::default()).1),
        ));
        tx.send_modify(|s| s.name = "StreamDock".into());
        tx.send_modify(|s| s.connected = true);
        tx.send_modify(|s| s.name = "Kitchen".into());
        drop(tx);
        task.await.unwrap();
        let mut last = None;
        while let Ok(event) = events.try_recv() {
            if let crate::hub::HubEvent::Player(p) = event {
                last = Some(p);
            }
        }
        let last = last.expect("player state published");
        assert!(last.enabled && last.connected);
        assert_eq!(last.name, "Kitchen");
        assert_eq!(last.player_id, "02:00:00:00:00:01");
        assert_eq!(state.player_name("StreamDock"), "Kitchen", "rename kept");
    }
}

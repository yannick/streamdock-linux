//! The in-process event hub: which docks are attached, and every event they produce.
//!
//! Device tasks (`crate::fake` today, the real device manager in H15-8) [`Hub::attach`] a dock,
//! publish its input through the returned [`Attached`] handle and execute the [`Command`]s that
//! arrive on [`Attached::commands`]. API clients [`Hub::subscribe`] to the event stream and send
//! commands with [`Hub::command`].
//!
//! Events go through a `tokio::sync::broadcast` channel: publishing never waits for a subscriber,
//! and a subscriber that falls more than [`EVENT_BUFFER`] events behind skips the oldest ones
//! instead of slowing anyone down.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use streamdock_hid::InputEvent;
use streamdock_proto::streamdock::v1 as pb;
use streamdock_proto::streamdock::v1::__buffa::oneof::device_event::Kind;
use streamdock_proto::streamdock::v1::__buffa::oneof::knob_event::Action;
use tokio::sync::{broadcast, mpsc, oneshot};

/// Activity entries kept for clients that connect later.
pub const ACTIVITY_HISTORY: usize = 200;
/// Activity source of the daemon itself (modules use their id).
pub const DAEMON: &str = "daemon";

/// Events a subscriber may lag behind before it skips the oldest ones.
pub const EVENT_BUFFER: usize = 1024;
/// Commands that may queue up for one device.
const COMMAND_BUFFER: usize = 32;

/// Something an API client wants a device to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// LCD brightness, 0..=100.
    SetBrightness(u8),
    /// Show an encoded image (PNG/JPEG, upright, any size) on LCD key 1..=6.
    SetKeyImage { key: u8, image: Vec<u8> },
    /// Clear LCD key 1..=6.
    ClearKeyImage(u8),
}

/// A [`Command`] for a device task, with the channel to report its outcome on.
#[derive(Debug)]
pub struct CommandRequest {
    pub command: Command,
    pub reply: oneshot::Sender<Result<(), String>>,
}

/// Why a command could not be carried out.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CommandError {
    #[error("no device {0:?} is attached")]
    NotFound(String),
    #[error("device {0:?} went away")]
    Gone(String),
    #[error("device {id:?} failed: {message}")]
    Failed { id: String, message: String },
}

/// Something that happened, as broadcast to subscribers.
#[derive(Debug, Clone, PartialEq)]
pub enum HubEvent {
    /// A device was attached, detached or updated, or the user pressed or turned something.
    Device(pb::DeviceEvent),
    /// What the dock shows changed (mode or key faces); replaces the previous screen.
    Screen(pb::Screen),
    /// Something the daemon or a module did (activity feed).
    Activity(pb::Activity),
    /// This machine's audio changed (H15-50); replaces the previous state.
    Audio(pb::AudioState),
    /// This machine's network and Bluetooth changed (H15-49); replaces the previous state.
    Network(Box<pb::NetworkState>),
    /// The config or the uploaded images changed (H15-49).
    Config(pb::ConfigState),
    /// The wake word listener changed (H15-68); replaces the previous state.
    Wake(pb::WakeWordState),
    /// The built-in Lyrion player changed (H15-55); replaces the previous state.
    Player(pb::PlayerState),
}

/// A consistent snapshot plus everything that happens after it.
#[derive(Debug)]
pub struct Subscription {
    /// The devices attached when subscribing.
    pub devices: Vec<pb::Device>,
    /// The screen when subscribing.
    pub screen: pb::Screen,
    /// The key images the attached docks show, as `(device id, image)`.
    pub key_images: Vec<(String, pb::KeyImage)>,
    /// The recent activity, oldest first.
    pub activity: Vec<pb::Activity>,
    /// This machine's audio, where the daemon manages it (H15-50).
    pub audio: Option<pb::AudioState>,
    /// The network state, once the daemon has read it (only where it manages the network).
    pub network: Option<pb::NetworkState>,
    /// The config and image revisions.
    pub config: pb::ConfigState,
    /// The built-in player, where it is enabled.
    pub player: Option<pb::PlayerState>,
    /// The wake word listener, once it was enabled (H15-68).
    pub wake: Option<pb::WakeWordState>,
    /// Everything after the snapshot.
    pub events: broadcast::Receiver<HubEvent>,
}

/// The hub. Cheap to share: wrap it in an `Arc`.
#[derive(Debug)]
pub struct Hub {
    events: broadcast::Sender<HubEvent>,
    devices: RwLock<BTreeMap<String, Entry>>,
    screen: Mutex<pb::Screen>,
    image_version: AtomicU64,
    activity: Mutex<VecDeque<pb::Activity>>,
    activity_id: AtomicU64,
    /// Uploaded key images (H15-28); unset in tests and tools, where faces are text only.
    assets: std::sync::OnceLock<Arc<crate::assets::Assets>>,
    /// This machine's Wi-Fi and Bluetooth (H15-38); unset in tests and tools.
    network: std::sync::OnceLock<Arc<crate::network::Network>>,
    /// This machine's audio (H15-50); unset unless `[audio] manage = true`.
    audio: std::sync::OnceLock<Arc<crate::audio::Audio>>,
    /// The latest audio state, for new subscribers.
    audio_state: Mutex<Option<pb::AudioState>>,
    /// Their latest state, as pushed to the UI (H15-49).
    network_state: Mutex<Option<pb::NetworkState>>,
    /// Config and image revisions; they start at the start time, so they differ after a restart.
    config: Mutex<pb::ConfigState>,
    /// The built-in player (H15-55); unset in tests and tools.
    builtin_player: std::sync::OnceLock<Arc<crate::player::BuiltinPlayer>>,
    /// The output priority (H15-79); only where the daemon manages the audio.
    output_memory: std::sync::OnceLock<Arc<crate::output_memory::OutputMemory>>,
    /// Bluetooth codecs per device (H15-78); only where the daemon manages the audio.
    bt_codecs: std::sync::OnceLock<Arc<crate::bt_codec::BtCodecs>>,
    /// The wake word service (H15-68); unset in tests and tools.
    wake_word: std::sync::OnceLock<Arc<crate::assist::wake::WakeWord>>,
    /// The built-in player's latest state (H15-55); `None` until it was first enabled.
    player_state: Mutex<Option<pb::PlayerState>>,
    /// The wake word listener's latest state (H15-68).
    wake_state: Mutex<Option<pb::WakeWordState>>,
}

/// The image a dock key shows, as last sent successfully.
#[derive(Debug, Clone)]
struct StoredImage {
    version: u64,
    /// `None` when the key is blank.
    bytes: Option<Arc<[u8]>>,
}

impl StoredImage {
    fn message(&self, key: u8) -> pb::KeyImage {
        pb::KeyImage {
            key: u32::from(key),
            version: self.version,
            blank: self.bytes.is_none(),
            ..Default::default()
        }
    }
}

#[derive(Debug)]
struct Entry {
    device: pb::Device,
    commands: mpsc::Sender<CommandRequest>,
    /// Per LCD key 1-6; `None` until something was sent to it since the dock was attached.
    images: [Option<StoredImage>; 6],
}

impl Hub {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            events: broadcast::channel(EVENT_BUFFER).0,
            devices: RwLock::new(BTreeMap::new()),
            screen: Mutex::new(pb::Screen::default()),
            network_state: Mutex::new(None),
            builtin_player: std::sync::OnceLock::new(),
            player_state: Mutex::new(None),
            wake_word: std::sync::OnceLock::new(),
            bt_codecs: std::sync::OnceLock::new(),
            output_memory: std::sync::OnceLock::new(),
            wake_state: Mutex::new(None),
            config: Mutex::new(pb::ConfigState {
                revision: unix_ms(),
                assets_revision: unix_ms(),
                ..Default::default()
            }),
            // Versions continue from the start time, so `?v=` URLs never repeat those of an earlier
            // run (H15-94).
            image_version: AtomicU64::new(unix_ms()),
            activity: Mutex::new(VecDeque::with_capacity(ACTIVITY_HISTORY)),
            activity_id: AtomicU64::new(0),
            assets: std::sync::OnceLock::new(),
            network: std::sync::OnceLock::new(),
            audio: std::sync::OnceLock::new(),
            audio_state: Mutex::default(),
        })
    }

    /// Binds the key image store; only the first call counts.
    pub fn set_assets(&self, assets: Arc<crate::assets::Assets>) {
        let _ = self.assets.set(assets);
    }

    /// Binds the network manager; only the first call counts.
    pub fn set_network(&self, network: Arc<crate::network::Network>) {
        let _ = self.network.set(network);
    }

    /// This machine's Wi-Fi and Bluetooth, if the daemon has them.
    #[must_use]
    pub fn network(&self) -> Option<&Arc<crate::network::Network>> {
        self.network.get()
    }

    /// The key image store, if the daemon has one.
    #[must_use]
    pub fn assets(&self) -> Option<&Arc<crate::assets::Assets>> {
        self.assets.get()
    }

    /// Registers an attached dock and announces it. Dropping the returned handle detaches it.
    ///
    /// # Panics
    ///
    /// If a device with the same id is already attached.
    pub fn attach(self: &Arc<Self>, device: pb::Device) -> Attached {
        let id = device.id.clone();
        let (tx, rx) = mpsc::channel(COMMAND_BUFFER);
        let mut devices = self.devices.write().expect("hub lock poisoned");
        assert!(!devices.contains_key(&id), "device {id:?} is already attached");
        // Publish while holding the lock, so `subscribe` sees each device exactly once: either in
        // its snapshot or as this event.
        self.publish(&id, Kind::Attached(Box::new(device.clone())));
        devices.insert(
            id.clone(),
            Entry {
                device,
                commands: tx,
                images: Default::default(),
            },
        );
        drop(devices);
        tracing::info!(device = %id, "device attached");
        self.report(
            DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
            format!("dock {id} attached"),
        );
        Attached {
            hub: Arc::clone(self),
            id,
            commands: rx,
        }
    }

    /// The attached devices, ordered by id.
    #[must_use]
    pub fn devices(&self) -> Vec<pb::Device> {
        let devices = self.devices.read().expect("hub lock poisoned");
        devices.values().map(|e| e.device.clone()).collect()
    }

    /// Subscribes to all future events and returns the devices and the screen right now.
    #[must_use]
    pub fn subscribe(&self) -> Subscription {
        // Lock order: devices, then screen (`set_screen` only takes the screen lock).
        let devices = self.devices.read().expect("hub lock poisoned");
        let screen = self.screen.lock().expect("hub lock poisoned");
        let key_images = devices
            .values()
            .flat_map(|e| {
                (1u8..)
                    .zip(&e.images)
                    .filter_map(|(key, image)| image.as_ref().map(|i| (e.device.id.clone(), i.message(key))))
            })
            .collect();
        // Lock order: devices, screen, activity (`report` only takes the activity lock).
        let activity = self.activity.lock().expect("hub lock poisoned");
        // Lock order: …, activity, audio (`set_audio` only takes the audio lock).
        let audio = self.audio_state.lock().expect("hub lock poisoned");
        // Then the network state (`set_network_state` only takes that lock).
        let network = self.network_state.lock().expect("hub lock poisoned");
        let config = self.config.lock().expect("hub lock poisoned");
        let player = self.player_state.lock().expect("hub lock poisoned");
        Subscription {
            config: config.clone(),
            player: player.clone(),
            wake: self.wake_state.lock().expect("hub lock poisoned").clone(),
            audio: audio.clone(),
            network: network.clone(),
            devices: devices.values().map(|e| e.device.clone()).collect(),
            screen: screen.clone(),
            key_images,
            activity: activity.iter().cloned().collect(),
            events: self.events.subscribe(),
        }
    }

    /// The image LCD key `key` (1-6) of `device_id` shows: its version and bytes (PNG or JPEG, as
    /// sent). `None` if unknown, blank or out of range.
    #[must_use]
    pub fn key_image(&self, device_id: &str, key: u8) -> Option<(u64, Arc<[u8]>)> {
        let devices = self.devices.read().expect("hub lock poisoned");
        let image = devices
            .get(device_id)?
            .images
            .get(usize::from(key).checked_sub(1)?)?
            .as_ref()?;
        Some((image.version, Arc::clone(image.bytes.as_ref()?)))
    }

    /// Adds an entry to the activity feed and announces it. `source` is [`DAEMON`] or a module id.
    pub fn report(&self, source: &str, level: pb::ActivityLevel, text: impl Into<String>) {
        let entry = pb::Activity {
            id: self.activity_id.fetch_add(1, Ordering::Relaxed) + 1,
            timestamp_unix_ms: unix_ms(),
            source: source.to_owned(),
            level: level.into(),
            text: text.into(),
            ..Default::default()
        };
        let mut history = self.activity.lock().expect("hub lock poisoned");
        if history.len() == ACTIVITY_HISTORY {
            history.pop_front();
        }
        history.push_back(entry.clone());
        // Sent while holding the lock, so a subscriber sees each entry once (history or live).
        let _ = self.events.send(HubEvent::Activity(entry));
    }

    /// Remembers what a key shows now and announces it.
    fn record_image(&self, device_id: &str, key: u8, bytes: Option<Arc<[u8]>>) {
        let mut devices = self.devices.write().expect("hub lock poisoned");
        let Some(slot) = devices
            .get_mut(device_id)
            .and_then(|e| e.images.get_mut(usize::from(key).checked_sub(1)?))
        else {
            return;
        };
        let image = StoredImage {
            version: self.image_version.fetch_add(1, Ordering::Relaxed) + 1,
            bytes,
        };
        self.publish(device_id, Kind::KeyImage(Box::new(image.message(key))));
        *slot = Some(image);
    }

    /// Binds the audio manager; only the first call counts.
    pub fn set_audio_manager(&self, audio: Arc<crate::audio::Audio>) {
        let _ = self.audio.set(audio);
    }

    /// The audio manager, where the daemon manages the audio.
    #[must_use]
    pub fn audio(&self) -> Option<&Arc<crate::audio::Audio>> {
        self.audio.get()
    }

    /// Replaces the audio state and announces it, unless nothing changed.
    pub fn set_audio(&self, state: pb::AudioState) {
        let mut current = self.audio_state.lock().expect("hub lock poisoned");
        if current.as_ref() != Some(&state) {
            *current = Some(state.clone());
            // Sent while holding the lock, so subscribers see each state once.
            let _ = self.events.send(HubEvent::Audio(state));
        }
    }

    /// The screen right now.
    #[must_use]
    pub fn screen(&self) -> pb::Screen {
        self.screen.lock().expect("hub lock poisoned").clone()
    }

    /// Replaces the screen and announces it, unless nothing changed.
    pub fn set_screen(&self, screen: pb::Screen) {
        let mut current = self.screen.lock().expect("hub lock poisoned");
        if *current != screen {
            *current = screen.clone();
            // Sent while holding the lock, so subscribers see each screen once.
            let _ = self.events.send(HubEvent::Screen(screen));
        }
    }

    /// Sends a command to a device and waits until the device task has carried it out.
    /// The config file changed: announce a new config revision.
    pub fn config_changed(&self) {
        self.bump_config(|c| c.revision += 1);
    }

    /// An image was uploaded, replaced or deleted: announce a new image revision.
    pub fn assets_changed(&self) {
        self.bump_config(|c| c.assets_revision += 1);
    }

    fn bump_config(&self, change: impl FnOnce(&mut pb::ConfigState)) {
        let mut config = self.config.lock().expect("hub lock poisoned");
        change(&mut config);
        let _ = self.events.send(HubEvent::Config(config.clone()));
    }

    /// Replaces the network state and announces it, unless nothing changed.
    pub fn set_network_state(&self, state: pb::NetworkState) {
        let mut current = self.network_state.lock().expect("hub lock poisoned");
        if current.as_ref() != Some(&state) {
            *current = Some(state.clone());
            let _ = self.events.send(HubEvent::Network(Box::new(state)));
        }
    }

    /// Publishes input for an attached dock as if the dock had sent it (the web UI, H15-51), so it
    /// takes the same path as the dock's own input.
    ///
    /// # Errors
    ///
    /// [`CommandError::NotFound`] when no such dock is attached.
    pub fn send_input(&self, device_id: &str, input: InputEvent) -> Result<(), CommandError> {
        let devices = self.devices.read().expect("hub lock poisoned");
        if !devices.contains_key(device_id) {
            return Err(CommandError::NotFound(device_id.to_owned()));
        }
        // Under the lock, so the input never follows the dock's `detached` event.
        self.publish(device_id, input_kind(input));
        Ok(())
    }

    pub fn set_builtin_player(&self, player: Arc<crate::player::BuiltinPlayer>) {
        let _ = self.builtin_player.set(player);
    }

    #[must_use]
    pub fn builtin_player(&self) -> Option<&Arc<crate::player::BuiltinPlayer>> {
        self.builtin_player.get()
    }

    /// The built-in player's latest state, if it ran since the start.
    #[must_use]
    pub fn player_state(&self) -> Option<pb::PlayerState> {
        self.player_state.lock().expect("hub lock poisoned").clone()
    }

    /// Replaces the built-in player's state and announces it, unless nothing changed.
    pub fn set_player_state(&self, state: pb::PlayerState) {
        let mut current = self.player_state.lock().expect("hub lock poisoned");
        if current.as_ref() != Some(&state) {
            *current = Some(state.clone());
            let _ = self.events.send(HubEvent::Player(state));
        }
    }

    /// Binds the output priority service; only the first call counts.
    pub fn set_output_memory(&self, memory: Arc<crate::output_memory::OutputMemory>) {
        let _ = self.output_memory.set(memory);
    }

    /// The output priority service, where the daemon manages the audio.
    #[must_use]
    pub fn output_memory(&self) -> Option<&Arc<crate::output_memory::OutputMemory>> {
        self.output_memory.get()
    }

    /// The latest audio state, where the daemon manages the audio.
    #[must_use]
    pub fn audio_state(&self) -> Option<pb::AudioState> {
        self.audio_state.lock().expect("hub lock poisoned").clone()
    }

    /// Binds the Bluetooth codec service; only the first call counts.
    pub fn set_bt_codecs(&self, codecs: Arc<crate::bt_codec::BtCodecs>) {
        let _ = self.bt_codecs.set(codecs);
    }

    /// The Bluetooth codec service, where the daemon manages the audio.
    #[must_use]
    pub fn bt_codecs(&self) -> Option<&Arc<crate::bt_codec::BtCodecs>> {
        self.bt_codecs.get()
    }

    /// Binds the wake word service; only the first call counts.
    pub fn set_wake_word(&self, wake: Arc<crate::assist::wake::WakeWord>) {
        let _ = self.wake_word.set(wake);
    }

    /// The wake word service, if the daemon runs one.
    #[must_use]
    pub fn wake_word(&self) -> Option<&Arc<crate::assist::wake::WakeWord>> {
        self.wake_word.get()
    }

    /// The wake word listener's latest state, if it ran since the start.
    #[must_use]
    pub fn wake_state(&self) -> Option<pb::WakeWordState> {
        self.wake_state.lock().expect("hub lock poisoned").clone()
    }

    /// Replaces the wake word listener's state and announces it, unless nothing changed.
    pub fn set_wake_state(&self, state: pb::WakeWordState) {
        let mut current = self.wake_state.lock().expect("hub lock poisoned");
        if current.as_ref() != Some(&state) {
            *current = Some(state.clone());
            let _ = self.events.send(HubEvent::Wake(state));
        }
    }

    pub async fn command(&self, device_id: &str, command: Command) -> Result<(), CommandError> {
        let tx = {
            let devices = self.devices.read().expect("hub lock poisoned");
            let entry = devices
                .get(device_id)
                .ok_or_else(|| CommandError::NotFound(device_id.to_owned()))?;
            entry.commands.clone()
        };
        let gone = || CommandError::Gone(device_id.to_owned());
        let (reply, outcome) = oneshot::channel();
        let brightness = match command {
            Command::SetBrightness(percent) => Some(percent),
            _ => None,
        };
        let image = match &command {
            Command::SetKeyImage { key, image } => Some((*key, Some(Arc::<[u8]>::from(image.as_slice())))),
            Command::ClearKeyImage(key) => Some((*key, None)),
            Command::SetBrightness(_) => None,
        };
        tx.send(CommandRequest { command, reply })
            .await
            .map_err(|_| gone())?;
        outcome
            .await
            .map_err(|_| gone())?
            .map_err(|message| CommandError::Failed {
                id: device_id.to_owned(),
                message,
            })?;
        if let Some(percent) = brightness {
            self.update(device_id, |d| d.brightness = u32::from(percent));
            self.report(
                DAEMON,
                pb::ActivityLevel::ACTIVITY_LEVEL_OK,
                format!("brightness {percent} %"),
            );
        }
        if let Some((key, bytes)) = image {
            self.record_image(device_id, key, bytes);
        }
        Ok(())
    }

    /// Changes a device's properties and announces the new state.
    fn update(&self, device_id: &str, change: impl FnOnce(&mut pb::Device)) {
        let mut devices = self.devices.write().expect("hub lock poisoned");
        if let Some(entry) = devices.get_mut(device_id) {
            change(&mut entry.device);
            self.publish(device_id, Kind::Updated(Box::new(entry.device.clone())));
        }
    }

    fn publish(&self, device_id: &str, kind: Kind) {
        let event = pb::DeviceEvent {
            device_id: device_id.to_owned(),
            timestamp_unix_ms: unix_ms(),
            kind: Some(kind),
            ..Default::default()
        };
        // An error only means nobody is subscribed right now.
        let _ = self.events.send(HubEvent::Device(event));
    }

    fn detach(&self, device_id: &str) {
        let mut devices = self.devices.write().expect("hub lock poisoned");
        if devices.remove(device_id).is_some() {
            self.publish(device_id, Kind::Detached(Box::default()));
            tracing::info!(device = %device_id, "device detached");
            drop(devices);
            self.report(
                DAEMON,
                pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
                format!("dock {device_id} detached"),
            );
        }
    }
}

/// An attached dock, owned by its device task. Dropping it detaches the dock.
#[derive(Debug)]
pub struct Attached {
    hub: Arc<Hub>,
    id: String,
    /// Commands for this dock. Carry each out and answer on its `reply` channel.
    pub commands: mpsc::Receiver<CommandRequest>,
}

impl Attached {
    /// The device id (its USB serial number).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Publishes something the user did on the dock.
    pub fn publish_input(&self, input: InputEvent) {
        self.hub.publish(&self.id, input_kind(input));
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        self.hub.detach(&self.id);
    }
}

/// Maps a driver input event to its API representation.
#[must_use]
pub fn input_kind(input: InputEvent) -> Kind {
    let key = |key: streamdock_hid::Key, pressed| {
        Kind::Key(Box::new(pb::KeyEvent {
            key: u32::from(key.number()),
            pressed,
            ..Default::default()
        }))
    };
    let knob = |knob: streamdock_hid::Knob, action| {
        Kind::Knob(Box::new(pb::KnobEvent {
            knob: u32::from(knob.number()),
            action: Some(action),
            ..Default::default()
        }))
    };
    match input {
        InputEvent::KeyDown(k) => key(k, true),
        InputEvent::KeyUp(k) => key(k, false),
        InputEvent::KnobDown(k) => knob(k, Action::Pressed(true)),
        InputEvent::KnobUp(k) => knob(k, Action::Pressed(false)),
        InputEvent::KnobTurn { knob: k, delta } => knob(k, Action::Turned(i32::from(delta))),
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use streamdock_hid::{Key, Knob};

    use super::*;

    /// The next device event's kind, skipping activity entries.
    async fn next_kind(rx: &mut broadcast::Receiver<HubEvent>) -> Kind {
        loop {
            match rx.recv().await.unwrap() {
                HubEvent::Device(e) => return e.kind.expect("device event without kind"),
                HubEvent::Activity(_) | HubEvent::Audio(_) => {}
                HubEvent::Screen(s) => panic!("expected a device event, got screen {s:?}"),
                HubEvent::Network(n) => panic!("expected a device event, got network {n:?}"),
                HubEvent::Config(c) => panic!("expected a device event, got config {c:?}"),
                HubEvent::Player(p) => panic!("expected a device event, got player {p:?}"),
                HubEvent::Wake(w) => panic!("expected a device event, got wake word {w:?}"),
            }
        }
    }

    #[tokio::test]
    async fn activity_is_kept_announced_and_capped() {
        let hub = Hub::new();
        let mut rx = hub.subscribe().events;
        let dock = hub.attach(device("A"));
        hub.report(
            "lyrion",
            pb::ActivityLevel::ACTIVITY_LEVEL_ERROR,
            "player not found",
        );
        drop(dock);
        let texts: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|e| match e {
                HubEvent::Activity(a) => Some((a.source, a.text)),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            [
                ("daemon".to_owned(), "dock A attached".to_owned()),
                ("lyrion".to_owned(), "player not found".to_owned()),
                ("daemon".to_owned(), "dock A detached".to_owned()),
            ]
        );
        for i in 0..ACTIVITY_HISTORY + 10 {
            hub.report("demo", pb::ActivityLevel::ACTIVITY_LEVEL_INFO, format!("{i}"));
        }
        let history = hub.subscribe().activity;
        assert_eq!(history.len(), ACTIVITY_HISTORY);
        assert_eq!(history.last().unwrap().text, format!("{}", ACTIVITY_HISTORY + 9));
        assert!(
            history.windows(2).all(|w| w[0].id < w[1].id),
            "oldest first, ids increase"
        );
    }

    #[tokio::test]
    async fn key_images_are_recorded_announced_and_snapshotted() {
        let hub = Hub::new();
        let mut dock = hub.attach(device("A"));
        let task = tokio::spawn(async move {
            for _ in 0..2 {
                let req = dock.commands.recv().await.unwrap();
                req.reply.send(Ok(())).unwrap();
            }
            dock
        });
        let mut rx = hub.subscribe().events;
        let png = b"\x89PNG fake".to_vec();
        hub.command(
            "A",
            Command::SetKeyImage {
                key: 3,
                image: png.clone(),
            },
        )
        .await
        .unwrap();
        let (v1, bytes) = hub.key_image("A", 3).unwrap();
        assert_eq!(&*bytes, png.as_slice());
        assert!(
            matches!(next_kind(&mut rx).await, Kind::KeyImage(k) if k.key == 3 && k.version == v1 && !k.blank)
        );

        hub.command("A", Command::ClearKeyImage(3)).await.unwrap();
        assert!(hub.key_image("A", 3).is_none(), "blank keys have no bytes");
        let blank = match next_kind(&mut rx).await {
            Kind::KeyImage(k) => *k,
            other => panic!("{other:?}"),
        };
        assert!(blank.blank && blank.version > v1);
        // New subscribers get the current images in their snapshot.
        let sub = hub.subscribe();
        assert_eq!(sub.key_images.len(), 1);
        assert_eq!(sub.key_images[0].0, "A");
        assert!(sub.key_images[0].1.blank);
        assert!(hub.key_image("A", 9).is_none() && hub.key_image("nope", 1).is_none());
        drop(task.await.unwrap());
    }

    #[tokio::test]
    async fn screen_changes_are_announced_once_and_snapshotted() {
        let hub = Hub::new();
        let mut rx = hub.subscribe().events;
        let screen = pb::Screen {
            active_mode: "demo".into(),
            ..Default::default()
        };
        hub.set_screen(screen.clone());
        hub.set_screen(screen.clone()); // unchanged: not announced again
        assert_eq!(rx.try_recv().unwrap(), HubEvent::Screen(screen.clone()));
        assert!(rx.try_recv().is_err());
        assert_eq!(hub.subscribe().screen, screen);
    }

    #[tokio::test]
    async fn network_changes_are_announced_once_and_snapshotted() {
        let hub = Hub::new();
        assert_eq!(hub.subscribe().network, None);
        let mut rx = hub.subscribe().events;
        let state = pb::NetworkState {
            bluetooth_scanning: true,
            ..Default::default()
        };
        hub.set_network_state(state.clone());
        hub.set_network_state(state.clone()); // unchanged: not announced again
        assert_eq!(rx.try_recv().unwrap(), HubEvent::Network(Box::new(state.clone())));
        assert!(rx.try_recv().is_err());
        assert_eq!(hub.subscribe().network, Some(state));
    }

    #[tokio::test]
    async fn config_revisions_are_announced_and_snapshotted() {
        let hub = Hub::new();
        let start = hub.subscribe().config;
        let mut rx = hub.subscribe().events;
        hub.config_changed();
        hub.assets_changed();
        let Ok(HubEvent::Config(c)) = rx.try_recv() else {
            panic!("no config event")
        };
        assert_eq!(c.revision, start.revision + 1);
        let Ok(HubEvent::Config(c)) = rx.try_recv() else {
            panic!("no config event")
        };
        assert_eq!(c.assets_revision, start.assets_revision + 1);
        assert_eq!(hub.subscribe().config, c);
    }

    fn device(id: &str) -> pb::Device {
        pb::Device {
            id: id.to_owned(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn attach_input_detach_reach_subscribers_in_order() {
        let hub = Hub::new();
        let sub = hub.subscribe();
        assert_eq!(sub.devices, []);
        let mut rx = sub.events;

        let dock = hub.attach(device("A"));
        dock.publish_input(InputEvent::KeyDown(Key::new(4).unwrap()));
        dock.publish_input(InputEvent::KnobTurn {
            knob: Knob::new(2).unwrap(),
            delta: -1,
        });
        drop(dock);

        let kinds: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|e| match e {
                HubEvent::Device(e) => e.kind,
                _ => None,
            })
            .collect();
        assert_eq!(kinds.len(), 4);
        assert!(matches!(&kinds[0], Kind::Attached(d) if d.id == "A"));
        assert!(matches!(&kinds[1], Kind::Key(k) if k.key == 4 && k.pressed));
        assert!(matches!(&kinds[2], Kind::Knob(k) if k.knob == 2 && k.action == Some(Action::Turned(-1))));
        assert!(matches!(&kinds[3], Kind::Detached(_)));
        assert_eq!(hub.devices(), []);
    }

    #[tokio::test]
    async fn subscribe_snapshots_attached_devices() {
        let hub = Hub::new();
        let _a = hub.attach(device("A"));
        let mut sub = hub.subscribe();
        assert_eq!(
            sub.devices.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
            ["A"]
        );
        assert!(
            sub.events.try_recv().is_err(),
            "the attach happened before subscribing"
        );
    }

    #[tokio::test]
    async fn command_round_trip_updates_brightness() {
        let hub = Hub::new();
        let mut dock = hub.attach(device("A"));
        let task = tokio::spawn(async move {
            let req = dock.commands.recv().await.unwrap();
            assert_eq!(req.command, Command::SetBrightness(40));
            req.reply.send(Ok(())).unwrap();
            dock
        });
        let mut rx = hub.subscribe().events;
        hub.command("A", Command::SetBrightness(40)).await.unwrap();
        assert_eq!(hub.devices()[0].brightness, 40);
        assert!(matches!(next_kind(&mut rx).await, Kind::Updated(d) if d.brightness == 40));
        drop(task.await.unwrap());
    }

    /// H15-51: input from the API is published like the dock's own, for attached docks only.
    #[tokio::test]
    async fn send_input_publishes_like_the_dock() {
        let hub = Hub::new();
        assert_eq!(
            hub.send_input("A", InputEvent::KeyDown(Key::new(2).unwrap())),
            Err(CommandError::NotFound("A".into()))
        );
        let _dock = hub.attach(device("A"));
        let mut rx = hub.subscribe().events;
        hub.send_input("A", InputEvent::KeyDown(Key::new(2).unwrap()))
            .unwrap();
        assert!(matches!(next_kind(&mut rx).await, Kind::Key(k) if k.key == 2 && k.pressed));
    }

    #[tokio::test]
    async fn command_errors() {
        let hub = Hub::new();
        assert_eq!(
            hub.command("nope", Command::ClearKeyImage(1)).await,
            Err(CommandError::NotFound("nope".into()))
        );
        let mut dock = hub.attach(device("A"));
        let task = tokio::spawn(async move {
            let req = dock.commands.recv().await.unwrap();
            req.reply.send(Err("usb stall".into())).unwrap();
            dock
        });
        assert!(matches!(
            hub.command("A", Command::ClearKeyImage(1)).await,
            Err(CommandError::Failed { message, .. }) if message == "usb stall"
        ));
        drop(task.await.unwrap());
    }
}

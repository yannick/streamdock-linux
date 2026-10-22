//! The settings screen on the dock (H15-91): the paired Bluetooth audio devices (headsets,
//! speakers) with their state and battery; a press connects or disconnects one. Simple enough for
//! a child: pairing a new device stays on the web page.
//!
//! ```toml
//! [modules.settings]
//! title = "Settings"                    # the mode name; optional
//! speaker_player = "aa:aa:00:00:00:06"  # headset mode: the speaker's Lyrion player; optional
//! headset_player = "…"                  # optional; default: the built-in player
//! ```
//!
//! Keys 1-4 show the devices, connected ones first; with more, key 6 and knob 1 page through
//! them. Key 5 is the headset mode (H15-89), shown while a headset is connected or the music is
//! on it: Lyrion moves the queue between the speaker's player and the headset's (`music.rs`). The
//! headset's volume limit is the built-in player's `[player] max_volume`. The screen is read
//! while shown (every 3 s) and right after every press.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::mpsc;

use super::{FaceState, Input, KeyFace, Module, ModuleEvent, ModuleIo, ModuleTask, Page, face, knob};
use crate::hub::Hub;

mod music;
mod transfer;

pub use music::{Lyrion, Music, Off};

/// The module id, `[modules.settings]`.
pub const ID: &str = "settings";
/// Devices per page: keys 1-4.
const PER_PAGE: usize = 4;
/// The headset mode's key.
const MODE_KEY: u8 = 5;
/// The key that pages on.
const MORE_KEY: u8 = 6;
/// How often the list is read while the screen is shown.
const POLL: Duration = Duration::from_secs(3);

/// `[modules.settings]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub title: String,
    /// The speaker's Lyrion player; `None`: no headset mode.
    pub speaker_player: Option<String>,
    /// The headset's Lyrion player; `None`: the built-in player.
    pub headset_player: Option<String>,
}

impl Config {
    pub fn from_table(table: &toml::Table) -> anyhow::Result<Self> {
        let text = |key: &str| -> anyhow::Result<Option<String>> {
            match table.get(key) {
                None => Ok(None),
                Some(v) => v
                    .as_str()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(|t| Some(t.to_owned()))
                    .with_context(|| format!("[modules.settings] {key} must be a non-empty string")),
            }
        };
        Ok(Self {
            title: text("title")?.unwrap_or_else(|| "Settings".to_owned()),
            speaker_player: text("speaker_player")?,
            headset_player: text("headset_player")?,
        })
    }
}

/// Builds the settings screen from `[modules.settings]`; `lyrion` is `[modules.lyrion]` (its
/// server), needed for the headset mode.
pub fn from_table(
    table: &toml::Table,
    lyrion: Option<&toml::Table>,
    hub: Option<&Arc<Hub>>,
) -> anyhow::Result<Vec<Box<dyn Module>>> {
    let Config {
        title,
        speaker_player,
        headset_player,
    } = Config::from_table(table)?;
    let radio = Bluez { hub: hub.cloned() };
    let Some(speaker) = speaker_player else {
        return Ok(vec![Box::new(Settings {
            title,
            radio,
            music: Off,
        })]);
    };
    let url = lyrion
        .and_then(|t| t.get("url"))
        .and_then(toml::Value::as_str)
        .context("[modules.settings] speaker_player needs [modules.lyrion] url")?;
    let music = Lyrion {
        client: crate::modules::lyrion::client::Client::new(url)?,
        speaker,
        headset: headset_player,
        hub: hub.cloned(),
    };
    Ok(vec![Box::new(Settings { title, radio, music })])
}

/// A paired audio device as the screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub address: String,
    pub name: String,
    pub connected: bool,
    pub battery: Option<u8>,
    /// Headphones or a headset (else a speaker).
    pub headset: bool,
}

/// What the screen needs from Bluetooth.
pub trait Radio: Send + Sync + 'static {
    /// The paired audio devices; `Err` when Bluetooth isn't there.
    fn devices(&self) -> impl Future<Output = anyhow::Result<Vec<Entry>>> + Send;
    /// Connects (`on`) or disconnects `address`.
    fn set(&self, address: &str, on: bool) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// BlueZ through the daemon's network service (the Luckfox, `[network] manage = true`).
#[derive(Debug, Clone)]
pub struct Bluez {
    hub: Option<Arc<Hub>>,
}

impl Bluez {
    async fn bluetooth(&self) -> anyhow::Result<&crate::network::bluetooth::Bluetooth> {
        let network = self
            .hub
            .as_ref()
            .and_then(|h| h.network())
            .context("Bluetooth isn't managed here")?;
        network.bluetooth().await
    }
}

impl Radio for Bluez {
    async fn devices(&self) -> anyhow::Result<Vec<Entry>> {
        let devices = self.bluetooth().await?.devices().await?;
        Ok(devices
            .into_iter()
            .filter(|d| d.paired && d.audio)
            .map(|d| Entry {
                headset: d.icon.contains("headset") || d.icon.contains("headphone"),
                address: d.address,
                name: d.name,
                connected: d.connected,
                battery: d.battery,
            })
            .collect())
    }

    async fn set(&self, address: &str, on: bool) -> anyhow::Result<()> {
        let bluetooth = self.bluetooth().await?;
        if on {
            bluetooth.connect(address).await
        } else {
            bluetooth.disconnect(address).await
        }
    }
}

#[derive(Debug)]
pub struct Settings<R, M> {
    title: String,
    radio: R,
    music: M,
}

/// What a press asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    /// Connect (`on`) or disconnect a device.
    Device { address: String, on: bool },
    /// Headset mode: the music to this headset (`Some(address)`) or back to the speaker.
    Mode { headset: Option<String> },
}

/// What the screen shows.
#[derive(Debug, Default)]
struct Screen {
    entries: Vec<Entry>,
    page: usize,
    /// Why the list couldn't be read.
    unavailable: Option<String>,
    /// Whether the music is on the headset; `None` without headset mode (or unknown).
    on_headset: Option<bool>,
    /// The press in flight.
    busy: Option<Action>,
    /// The device whose last press failed.
    failed: Option<String>,
    /// The last headset-mode switch failed.
    mode_failed: bool,
}

impl Screen {
    fn pages(&self) -> usize {
        self.entries.len().div_ceil(PER_PAGE).max(1)
    }

    fn turn(&mut self, delta: i32) {
        let pages = i64::try_from(self.pages()).unwrap_or(1);
        let page = i64::try_from(self.page).unwrap_or(0) + i64::from(delta);
        self.page = usize::try_from(page.rem_euclid(pages)).unwrap_or(0);
    }

    fn refreshed(&mut self, devices: anyhow::Result<Vec<Entry>>) {
        match devices {
            Ok(entries) => {
                self.entries = entries;
                self.unavailable = None;
                self.page = self.page.min(self.pages() - 1);
            }
            Err(e) => self.unavailable = Some(format!("{e:#}")),
        }
    }

    /// The first connected headset, which headset mode plays into.
    fn headset(&self) -> Option<&Entry> {
        self.entries.iter().find(|e| e.headset && e.connected)
    }

    /// A press or turn; `Some` when something is to be done.
    fn input(&mut self, input: Input) -> Option<Action> {
        match input {
            Input::Key {
                key: k @ 1..=4,
                pressed: true,
            } => self.press(usize::from(k - 1)),
            Input::Key {
                key: MODE_KEY,
                pressed: true,
            } => self.switch_mode(),
            Input::Key {
                key: MORE_KEY,
                pressed: true,
            } => {
                self.turn(1);
                None
            }
            Input::KnobTurn { knob: 1, delta } => {
                self.turn(delta);
                None
            }
            _ => None,
        }
    }

    fn press(&mut self, slot: usize) -> Option<Action> {
        if self.busy.is_some() || self.unavailable.is_some() {
            return None;
        }
        let entry = self.entries.get(self.page * PER_PAGE + slot)?;
        let action = Action::Device {
            address: entry.address.clone(),
            on: !entry.connected,
        };
        self.failed = None;
        self.busy = Some(action.clone());
        Some(action)
    }

    fn switch_mode(&mut self) -> Option<Action> {
        if self.busy.is_some() {
            return None;
        }
        let action = if self.on_headset? {
            Action::Mode { headset: None }
        } else {
            Action::Mode {
                headset: Some(self.headset()?.address.clone()),
            }
        };
        self.mode_failed = false;
        self.busy = Some(action.clone());
        Some(action)
    }

    fn done(&mut self, ok: bool) {
        match self.busy.take() {
            Some(Action::Device { address, .. }) if !ok => self.failed = Some(address),
            Some(Action::Mode { .. }) => self.mode_failed = !ok,
            _ => {}
        }
    }

    fn page(&self) -> Page {
        let mut faces = Vec::new();
        if let Some(why) = &self.unavailable {
            tracing::debug!(why, "settings screen: Bluetooth unavailable");
            faces.push(with_image(
                face(1, "Bluetooth", "not available", FaceState::FACE_STATE_UNAVAILABLE),
                "bluetooth",
            ));
        } else if self.entries.is_empty() {
            faces.push(with_image(
                face(
                    1,
                    "No devices",
                    "pair on the web page",
                    FaceState::FACE_STATE_IDLE,
                ),
                "bluetooth",
            ));
        } else {
            faces.extend(self.device_faces());
        }
        faces.extend(self.mode_face());
        let mut knobs = Vec::new();
        if self.pages() > 1 {
            let detail = format!("page {}/{}", self.page + 1, self.pages());
            faces.push(with_image(
                face(MORE_KEY, "More", detail, FaceState::FACE_STATE_IDLE),
                "next",
            ));
            knobs.push(knob(1, "Devices"));
        }
        Page {
            faces,
            knobs,
            now_playing: None,
        }
    }

    /// Key 5: "Headset" while the music is there, "Speaker" while a headset is connected to switch
    /// to; nothing otherwise.
    fn mode_face(&self) -> Option<KeyFace> {
        let on_headset = self.on_headset?;
        if !on_headset && self.headset().is_none() {
            return None;
        }
        let switching = matches!(self.busy, Some(Action::Mode { .. }));
        let (label, image, hint, state) = if on_headset {
            (
                "Headset",
                "headphones",
                "tap for speaker",
                FaceState::FACE_STATE_ON,
            )
        } else {
            (
                "Speaker",
                "speaker",
                "tap for headset",
                FaceState::FACE_STATE_IDLE,
            )
        };
        let (detail, state) = if switching {
            ("switching…", FaceState::FACE_STATE_ACTIVE)
        } else if self.mode_failed {
            ("didn't work", FaceState::FACE_STATE_UNAVAILABLE)
        } else {
            (hint, state)
        };
        Some(with_image(face(MODE_KEY, label, detail, state), image))
    }

    fn device_faces(&self) -> impl Iterator<Item = KeyFace> + '_ {
        let first = self.page * PER_PAGE;
        self.entries
            .iter()
            .skip(first)
            .take(PER_PAGE)
            .zip(1u8..)
            .map(|(entry, key)| {
                let busy = match &self.busy {
                    Some(Action::Device { address, on }) if *address == entry.address => Some(*on),
                    _ => None,
                };
                let (detail, state) = match busy {
                    Some(true) => ("connecting…".to_owned(), FaceState::FACE_STATE_ACTIVE),
                    Some(false) => ("disconnecting…".to_owned(), FaceState::FACE_STATE_ACTIVE),
                    None if self.failed.as_deref() == Some(&entry.address) => {
                        ("didn't work".to_owned(), FaceState::FACE_STATE_UNAVAILABLE)
                    }
                    None if entry.connected => (connected(entry.battery), FaceState::FACE_STATE_ON),
                    None => ("tap to connect".to_owned(), FaceState::FACE_STATE_IDLE),
                };
                let image = if entry.headset { "headphones" } else { "speaker" };
                with_image(face(key, entry.name.clone(), detail, state), image)
            })
    }
}

fn connected(battery: Option<u8>) -> String {
    battery.map_or_else(|| "connected".to_owned(), |b| format!("connected · {b} %"))
}

fn with_image(mut face: KeyFace, image: &str) -> KeyFace {
    image.clone_into(&mut face.image);
    face
}

impl<R: Radio, M: Music> Module for Settings<R, M> {
    fn id(&self) -> &str {
        ID
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn kind(&self) -> &'static str {
        "settings"
    }

    fn run(self: Box<Self>, io: ModuleIo) -> ModuleTask {
        let services = Arc::new(Services {
            radio: self.radio,
            music: self.music,
        });
        Box::pin(run(services, io))
    }
}

/// What the screen acts through.
#[derive(Debug)]
struct Services<R, M> {
    radio: R,
    music: M,
}

impl<R: Radio, M: Music> Services<R, M> {
    async fn refresh(&self, screen: &mut Screen) {
        screen.refreshed(self.radio.devices().await);
        screen.on_headset = self.music.on_headset().await.ok();
    }

    async fn act(&self, action: &Action) -> anyhow::Result<()> {
        match action {
            Action::Device { address, on } => self.radio.set(address, *on).await,
            Action::Mode { headset } => self.music.switch(headset.as_deref()).await,
        }
    }
}

type Done = (Action, Result<(), String>);

async fn run<R: Radio, M: Music>(services: Arc<Services<R, M>>, mut io: ModuleIo) {
    let mut screen = Screen::default();
    let (done_tx, mut done_rx) = mpsc::channel::<Done>(4);
    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut active = false;
    services.refresh(&mut screen).await;
    io.page.send_replace(screen.page());
    loop {
        tokio::select! {
            event = io.events.recv() => {
                let Some(event) = event else { return };
                match event {
                    ModuleEvent::Activated => active = true,
                    ModuleEvent::Deactivated => active = false,
                    ModuleEvent::Input(input) => {
                        if let Some(action) = screen.input(input) {
                            spawn_action(&services, action, &done_tx, &io, &screen);
                        }
                        io.page.send_replace(screen.page());
                        continue;
                    }
                }
            }
            Some((action, result)) = done_rx.recv() => {
                report(&io, &screen, &action, &result);
                screen.done(result.is_ok());
            }
            _ = poll.tick(), if active => {}
        }
        services.refresh(&mut screen).await;
        io.page.send_replace(screen.page());
    }
}

fn spawn_action<R: Radio, M: Music>(
    services: &Arc<Services<R, M>>,
    action: Action,
    done: &mpsc::Sender<Done>,
    io: &ModuleIo,
    screen: &Screen,
) {
    io.activity.info(describe(screen, &action, None));
    let services = Arc::clone(services);
    let done = done.clone();
    tokio::spawn(async move {
        let result = services.act(&action).await.map_err(|e| format!("{e:#}"));
        drop(done.send((action, result)).await);
    });
}

fn report(io: &ModuleIo, screen: &Screen, action: &Action, result: &Result<(), String>) {
    match result {
        Ok(()) => io.activity.ok(describe(screen, action, Some(true))),
        Err(e) => io
            .activity
            .error(format!("{}: {e}", describe(screen, action, Some(false)))),
    }
}

/// "Bluetooth: connecting WH-1000XM4" (`None`: started), "… connected" / "… not connected".
fn describe(screen: &Screen, action: &Action, outcome: Option<bool>) -> String {
    match action {
        Action::Device { address, on } => {
            let name = screen
                .entries
                .iter()
                .find(|e| e.address == *address)
                .map_or(address.as_str(), |e| e.name.as_str());
            let verb = match (on, outcome) {
                (true, None) => "connecting",
                (false, None) => "disconnecting",
                (true, Some(_)) => "connected",
                (false, Some(_)) => "disconnected",
            };
            let not = if outcome == Some(false) { "not " } else { "" };
            match outcome {
                None => format!("Bluetooth: {verb} {name}"),
                Some(_) => format!("Bluetooth: {name} {not}{verb}"),
            }
        }
        Action::Mode { headset } => {
            let to = if headset.is_some() { "headset" } else { "speaker" };
            match outcome {
                None => format!("music: moving to the {to}"),
                Some(true) => format!("music: on the {to}"),
                Some(false) => format!("music: not moved to the {to}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, connected: bool, battery: Option<u8>, headset: bool) -> Entry {
        Entry {
            address: format!("00:00:00:00:00:{:02X}", name.len()),
            name: name.into(),
            connected,
            battery,
            headset,
        }
    }

    fn screen(entries: Vec<Entry>) -> Screen {
        let mut s = Screen::default();
        s.refreshed(Ok(entries));
        s
    }

    fn press(key: u8) -> Input {
        Input::Key { key, pressed: true }
    }

    #[test]
    fn devices_show_their_state_and_battery() {
        let s = screen(vec![
            entry("WH-1000XM4", true, Some(80), true),
            entry("SRS-X88 Bedroom", false, None, false),
        ]);
        let page = s.page();
        let shown: Vec<_> = page
            .faces
            .iter()
            .map(|f| (f.key, f.label.as_str(), f.detail.as_str(), f.image.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (1, "WH-1000XM4", "connected · 80 %", "headphones"),
                (2, "SRS-X88 Bedroom", "tap to connect", "speaker"),
            ]
        );
        assert_eq!(
            page.faces[0].state,
            face(1, "", "", FaceState::FACE_STATE_ON).state
        );
        assert!(page.knobs.is_empty(), "one page: no paging");
    }

    #[test]
    fn a_press_connects_or_disconnects_one_at_a_time() {
        let mut s = screen(vec![
            entry("WH-1000XM4", true, None, true),
            entry("SRS-X88", false, None, false),
        ]);
        let off = s.input(press(1)).unwrap();
        assert_eq!(
            off,
            Action::Device {
                address: "00:00:00:00:00:0A".into(),
                on: false
            }
        );
        assert_eq!(s.page().faces[0].detail, "disconnecting…");
        assert_eq!(s.input(press(2)), None, "one at a time");
        s.done(true);
        let on = s.input(press(2)).unwrap();
        assert!(matches!(on, Action::Device { on: true, .. }));
        s.done(false);
        assert_eq!(s.page().faces[1].detail, "didn't work");
        assert_eq!(s.input(press(3)), None, "an empty key");
    }

    #[test]
    fn more_than_four_devices_page_with_key_6_and_knob_1() {
        let mut s = screen(
            (1..=6)
                .map(|n| entry(&"x".repeat(n), false, None, true))
                .collect(),
        );
        let page = s.page();
        assert_eq!(page.faces.len(), 5, "four devices and More");
        assert_eq!(page.faces[4].detail, "page 1/2");
        assert_eq!(page.knobs[0].label, "Devices");
        s.input(press(MORE_KEY));
        assert_eq!(s.page().faces.iter().filter(|f| f.key <= 4).count(), 2);
        s.input(Input::KnobTurn { knob: 1, delta: -1 });
        assert_eq!(s.page, 0);
        s.input(Input::KnobTurn { knob: 1, delta: -1 });
        assert_eq!(s.page, 1, "wraps around");
        let pressed = s.input(press(2)).unwrap();
        assert!(
            matches!(pressed, Action::Device { address, .. } if address == "00:00:00:00:00:06"),
            "the second device of page 2"
        );
        s.done(true);
        s.refreshed(Ok(vec![entry("x", false, None, true)]));
        assert_eq!(s.page, 0, "a shorter list keeps the page in range");
    }

    #[test]
    fn without_bluetooth_or_devices_the_screen_says_so() {
        let mut s = Screen::default();
        s.refreshed(Err(anyhow::anyhow!("Bluetooth isn't managed here")));
        let page = s.page();
        assert_eq!(
            (page.faces[0].label.as_str(), page.faces[0].detail.as_str()),
            ("Bluetooth", "not available")
        );
        assert_eq!(s.input(press(1)), None);
        let empty = screen(Vec::new()).page();
        assert_eq!(empty.faces[0].label, "No devices");
    }

    /// A radio that answers at once and records what it was asked.
    #[derive(Debug, Default)]
    struct Fake {
        devices: std::sync::Mutex<Vec<Entry>>,
        asked: std::sync::Mutex<Vec<(String, bool)>>,
    }

    impl Radio for Fake {
        async fn devices(&self) -> anyhow::Result<Vec<Entry>> {
            tokio::task::yield_now().await;
            Ok(self.devices.lock().unwrap().clone())
        }

        async fn set(&self, address: &str, on: bool) -> anyhow::Result<()> {
            tokio::task::yield_now().await;
            self.asked.lock().unwrap().push((address.to_owned(), on));
            for d in self.devices.lock().unwrap().iter_mut() {
                if d.address == address {
                    d.connected = on;
                }
            }
            Ok(())
        }
    }

    /// Music that moves at once and records where to.
    #[derive(Debug, Default)]
    struct FakeMusic {
        on_headset: std::sync::Mutex<bool>,
        asked: std::sync::Mutex<Vec<Option<String>>>,
    }

    impl Music for FakeMusic {
        async fn on_headset(&self) -> anyhow::Result<bool> {
            tokio::task::yield_now().await;
            Ok(*self.on_headset.lock().unwrap())
        }

        async fn switch(&self, headset: Option<&str>) -> anyhow::Result<()> {
            tokio::task::yield_now().await;
            self.asked.lock().unwrap().push(headset.map(str::to_owned));
            *self.on_headset.lock().unwrap() = headset.is_some();
            Ok(())
        }
    }

    /// Waits until key `key` shows a detail starting with `prefix`.
    async fn shows(pages: &mut tokio::sync::watch::Receiver<Page>, key: u32, prefix: &str) -> String {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let detail = pages
                    .borrow()
                    .faces
                    .iter()
                    .find(|f| f.key == key)
                    .map(|f| f.detail.clone());
                if let Some(d) = detail.filter(|d| d.starts_with(prefix)) {
                    return d;
                }
                pages.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("key {key} never showed {prefix:?}"))
    }

    #[tokio::test]
    async fn the_task_connects_a_headset_and_moves_the_music_to_it() {
        let services = Arc::new(Services {
            radio: Fake::default(),
            music: FakeMusic::default(),
        });
        services
            .radio
            .devices
            .lock()
            .unwrap()
            .push(entry("WH-1000XM4", false, Some(55), true));
        let (events_tx, events) = mpsc::channel(8);
        let (page, mut pages) = tokio::sync::watch::channel(Page::default());
        let io = ModuleIo {
            events,
            page,
            activity: super::super::Activity::disabled(),
        };
        let task = tokio::spawn(run(Arc::clone(&services), io));
        events_tx.send(ModuleEvent::Activated).await.unwrap();
        events_tx.send(ModuleEvent::Input(press(1))).await.unwrap();
        assert_eq!(shows(&mut pages, 1, "connected").await, "connected · 55 %");
        assert_eq!(
            *services.radio.asked.lock().unwrap(),
            [("00:00:00:00:00:0A".to_owned(), true)]
        );
        assert_eq!(shows(&mut pages, MODE_KEY.into(), "tap").await, "tap for headset");
        events_tx.send(ModuleEvent::Input(press(MODE_KEY))).await.unwrap();
        assert_eq!(
            shows(&mut pages, MODE_KEY.into(), "tap for s").await,
            "tap for speaker"
        );
        assert_eq!(
            *services.music.asked.lock().unwrap(),
            [Some("00:00:00:00:00:0A".to_owned())]
        );
        task.abort();
    }

    #[test]
    fn key_5_offers_the_headset_only_with_one_connected() {
        let mut s = screen(vec![
            entry("SRS-X88", true, None, false),
            entry("WH-1000XM4", false, None, true),
        ]);
        assert!(s.mode_face().is_none(), "no headset mode configured");
        s.on_headset = Some(false);
        assert!(s.mode_face().is_none(), "no headset connected");
        assert_eq!(s.input(press(MODE_KEY)), None);
        s.entries[1].connected = true;
        assert_eq!(s.mode_face().unwrap().label, "Speaker");
        let to_headset = s.input(press(MODE_KEY)).unwrap();
        assert_eq!(
            to_headset,
            Action::Mode {
                headset: Some("00:00:00:00:00:0A".into())
            }
        );
        assert_eq!(s.mode_face().unwrap().detail, "switching…");
        assert_eq!(s.input(press(1)), None, "one thing at a time");
        s.done(false);
        assert_eq!(s.mode_face().unwrap().detail, "didn't work");
        s.on_headset = Some(true);
        s.entries[1].connected = false;
        let face = s.mode_face().unwrap();
        assert_eq!(
            (face.label.as_str(), face.detail.as_str()),
            ("Headset", "didn't work")
        );
        assert_eq!(
            s.input(press(MODE_KEY)),
            Some(Action::Mode { headset: None }),
            "back to the speaker, also when the headset is gone"
        );
    }

    #[test]
    fn config() {
        let parse = |t: &str| Config::from_table(&toml::from_str(t).unwrap());
        let c = parse("speaker_player = \"aa:aa:00:00:00:06\"\n").unwrap();
        assert_eq!(
            (c.title.as_str(), c.speaker_player.as_deref()),
            ("Settings", Some("aa:aa:00:00:00:06"))
        );
        assert!(parse("title = \"\"\n").is_err());
        let no_lyrion = from_table(&toml::from_str("speaker_player = \"x\"\n").unwrap(), None, None);
        assert!(no_lyrion.is_err(), "headset mode needs the Lyrion server");
        assert!(from_table(&toml::Table::new(), None, None).is_ok());
    }
}

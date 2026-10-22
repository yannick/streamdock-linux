//! The mode controller (H15-16): which module owns the dock, input routing, and keeping every
//! attached dock and the web UI in sync with the active module's page.
//!
//! - Knob 3 switches modes (decided on H15-14): turn = next/previous, press = next. It is never
//!   passed to modules; all other keys and knobs go to the active module.
//! - The active page is published as `streamdock.v1.Screen` through the hub, and rendered onto every
//!   attached dock by one uploader task per dock, which only re-sends keys whose face changed.
//! - After a switch, key 2 shows the new mode's name for [`BANNER`].
//! - With no module enabled the controller does nothing: the dock keeps whatever the API sets.
//! - Profiles (H15-25): with `[[profiles]]` configured, knob 3 only cycles the active profile's
//!   modes. Every module keeps running whatever the profile, so switching is instant and keeps
//!   module state. The active profile is saved in the state file.
//! - [`ModeSwitch::reload`] replaces the modules while the daemon runs (H15-22): the old module
//!   tasks are stopped, the new ones started, and the active mode stays if it still exists.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use streamdock_proto::streamdock::v1 as pb;
use streamdock_proto::streamdock::v1::__buffa::oneof::device_event::Kind;
use streamdock_proto::streamdock::v1::__buffa::oneof::knob_event::Action;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::Profile;
use crate::hub::{Command, CommandError, Hub, HubEvent};
use crate::modules::{Activity, FaceState, Input, KeyFace, Module, ModuleEvent, ModuleIo, Page, face};
use crate::render;
use crate::state::StateFile;

/// The knob that switches modes.
pub const MODE_KNOB: u32 = 3;
/// How long key 2 shows the mode name after a switch.
pub const BANNER: Duration = Duration::from_millis(900);
/// Input events that may queue up for a busy module before new ones are dropped.
const MODULE_QUEUE: usize = 64;
/// LCD keys that show faces.
const LCD_KEYS: std::ops::RangeInclusive<u32> = 1..=6;
/// The key that shows the mode name after a switch.
const BANNER_KEY: u8 = 2;
/// How long a module that was just started gets to publish its first page before the docks are
/// updated (otherwise they would be blanked and redrawn).
const FIRST_PAGE: Duration = Duration::from_millis(500);

/// Why a mode switch was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ModeError {
    #[error("no mode {0:?} (enabled modes are listed in Screen.modes)")]
    Unknown(String),
    #[error("no profile {0:?} (profiles are listed in Screen.profiles)")]
    UnknownProfile(String),
    #[error("the mode controller is not running")]
    Stopped,
}

enum Request {
    SetMode(String, oneshot::Sender<Result<(), ModeError>>),
    SetProfile(String, oneshot::Sender<Result<(), ModeError>>),
    Reload(Vec<Box<dyn Module>>, Vec<Profile>, oneshot::Sender<()>),
}

/// The configured profiles and where the active one is remembered.
#[derive(Debug, Default)]
pub struct Profiles {
    pub list: Vec<Profile>,
    /// `None`: the choice is not saved (tests).
    pub state: Option<Arc<StateFile>>,
}

/// Switches modes from outside the controller, e.g. the `SetMode` RPC. Cheap to clone.
#[derive(Debug, Clone)]
pub struct ModeSwitch {
    tx: Option<mpsc::Sender<Request>>,
}

impl ModeSwitch {
    /// A switch without a controller (tests): every `set` fails with `Unknown`, `reload` with
    /// `Stopped`.
    #[cfg(test)]
    #[must_use]
    pub fn disabled() -> Self {
        Self { tx: None }
    }

    /// Switches to the mode `id` and waits until the controller has done it.
    pub async fn set(&self, id: &str) -> Result<(), ModeError> {
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| ModeError::Unknown(id.to_owned()))?;
        let (reply, done) = oneshot::channel();
        tx.send(Request::SetMode(id.to_owned(), reply))
            .await
            .map_err(|_| ModeError::Stopped)?;
        done.await.map_err(|_| ModeError::Stopped)?
    }

    /// Switches to the profile `name` and waits until the controller has done it.
    pub async fn set_profile(&self, name: &str) -> Result<(), ModeError> {
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| ModeError::UnknownProfile(name.to_owned()))?;
        let (reply, done) = oneshot::channel();
        tx.send(Request::SetProfile(name.to_owned(), reply))
            .await
            .map_err(|_| ModeError::Stopped)?;
        done.await.map_err(|_| ModeError::Stopped)?
    }

    /// Replaces the running modules and the profiles, and waits until the modules are started.
    pub async fn reload(
        &self,
        modules: Vec<Box<dyn Module>>,
        profiles: Vec<Profile>,
    ) -> Result<(), ModeError> {
        let tx = self.tx.as_ref().ok_or(ModeError::Stopped)?;
        let (reply, done) = oneshot::channel();
        tx.send(Request::Reload(modules, profiles, reply))
            .await
            .map_err(|_| ModeError::Stopped)?;
        done.await.map_err(|_| ModeError::Stopped)
    }
}

/// Starts the modules and the controller (also without modules, so a reload can add some).
pub fn start(hub: Arc<Hub>, modules: Vec<Box<dyn Module>>, profiles: Profiles) -> ModeSwitch {
    let (tx, requests) = mpsc::channel(8);
    let entries = spawn_modules(&hub, modules);
    tokio::spawn(Controller::new(hub, entries, profiles).run(requests));
    ModeSwitch { tx: Some(tx) }
}

fn spawn_modules(hub: &Arc<Hub>, modules: Vec<Box<dyn Module>>) -> Vec<Entry> {
    modules
        .into_iter()
        .map(|module| {
            let (events_tx, events) = mpsc::channel(MODULE_QUEUE);
            let (page_tx, page) = watch::channel(Page::default());
            let id = module.id().to_owned();
            let title = module.title().to_owned();
            let kind = module.kind();
            let activity = Activity::new(Arc::clone(hub), &id);
            let task = tokio::spawn(module.run(ModuleIo {
                events,
                page: page_tx,
                activity,
            }));
            Entry {
                id,
                title,
                kind,
                events: events_tx,
                page,
                task,
            }
        })
        .collect()
}

#[derive(Debug)]
struct Entry {
    id: String,
    title: String,
    kind: &'static str,
    events: mpsc::Sender<ModuleEvent>,
    page: watch::Receiver<Page>,
    task: JoinHandle<()>,
}

struct Controller {
    hub: Arc<Hub>,
    entries: Vec<Entry>,
    active: usize,
    banner_until: Option<Instant>,
    /// Faces for keys 1-6 as they should be on every dock; uploaders follow it.
    desired: watch::Sender<Vec<KeyFace>>,
    uploaders: HashMap<String, JoinHandle<()>>,
    /// Attached docks, so uploaders can be started when modules appear on a reload.
    devices: HashSet<String>,
    profiles: Vec<Profile>,
    /// Index into `profiles`; `None` without profiles.
    profile: Option<usize>,
    state: Option<Arc<StateFile>>,
}

impl Controller {
    fn new(hub: Arc<Hub>, entries: Vec<Entry>, profiles: Profiles) -> Self {
        let saved = profiles.state.as_ref().and_then(|s| s.profile());
        let mut controller = Self {
            hub,
            entries,
            active: 0,
            banner_until: None,
            desired: watch::channel(Vec::new()).0,
            uploaders: HashMap::new(),
            devices: HashSet::new(),
            profiles: Vec::new(),
            profile: None,
            state: profiles.state,
        };
        controller.set_profiles(profiles.list, saved.as_deref());
        controller.active = controller.visible().first().copied().unwrap_or(0);
        controller
    }

    /// Replaces the profile list, keeping `keep` (by name) active when it still exists.
    fn set_profiles(&mut self, list: Vec<Profile>, keep: Option<&str>) {
        self.profile = if list.is_empty() {
            None
        } else {
            Some(
                keep.and_then(|n| list.iter().position(|p| p.name == n))
                    .unwrap_or(0),
            )
        };
        self.profiles = list;
    }

    /// Indexes into `entries` of the modes knob 3 cycles through, in order.
    fn visible(&self) -> Vec<usize> {
        let all = || (0..self.entries.len()).collect();
        let Some(profile) = self.profile.map(|i| &self.profiles[i]) else {
            return all();
        };
        let visible: Vec<usize> = profile
            .modes
            .iter()
            .filter_map(|id| self.entries.iter().position(|e| &e.id == id))
            .collect();
        if visible.is_empty() && !self.entries.is_empty() {
            tracing::warn!(profile = %profile.name, "no mode of the profile is enabled; showing all modes");
            return all();
        }
        visible
    }

    async fn switch_profile(&mut self, name: &str) -> Result<(), ModeError> {
        let i = self
            .profiles
            .iter()
            .position(|p| p.name == name)
            .ok_or_else(|| ModeError::UnknownProfile(name.to_owned()))?;
        if self.profile == Some(i) {
            return Ok(());
        }
        self.profile = Some(i);
        if let Some(state) = &self.state {
            state.set_profile(name);
        }
        tracing::info!(profile = name, "profile switched");
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
            format!("profile {name}"),
        );
        let visible = self.visible();
        match visible.first() {
            Some(&first) if !visible.contains(&self.active) => self.switch_to(first).await,
            _ => self.publish(),
        }
        Ok(())
    }

    async fn run(mut self, mut requests: mpsc::Receiver<Request>) {
        let sub = self.hub.subscribe();
        let mut events = sub.events;
        self.devices = sub.devices.into_iter().map(|d| d.id).collect();
        self.activate(true).await;

        loop {
            let banner = self.banner_until;
            // A clone, so the other branches can borrow `self`; `publish` marks the original seen.
            let mut page = self.entries.get(self.active).map(|e| e.page.clone());
            tokio::select! {
                event = events.recv() => if !self.on_hub_event(event).await { break },
                changed = page_changed(page.as_mut()) => self.on_page_changed(changed.is_ok()),
                Some(request) = requests.recv() => self.on_request(request).await,
                () = sleep_until(banner) => {
                    self.banner_until = None;
                    self.publish();
                }
            }
        }
        self.stop();
    }

    /// Handles one hub event; `false` once the hub is gone.
    async fn on_hub_event(&mut self, event: Result<HubEvent, RecvError>) -> bool {
        match event {
            Ok(HubEvent::Device(event)) => self.on_device_event(event).await,
            Ok(
                HubEvent::Screen(_)
                | HubEvent::Activity(_)
                | HubEvent::Audio(_)
                | HubEvent::Network(_)
                | HubEvent::Config(_)
                | HubEvent::Player(_)
                | HubEvent::Wake(_),
            ) => {}
            Err(RecvError::Lagged(n)) => tracing::warn!(skipped = n, "mode controller fell behind"),
            Err(RecvError::Closed) => return false,
        }
        true
    }

    /// The active module published a page, or stopped (`alive` false): show what it left.
    fn on_page_changed(&mut self, alive: bool) {
        if !alive {
            tracing::error!(mode = %self.entries[self.active].id, "module stopped");
        }
        self.publish();
    }

    async fn on_request(&mut self, request: Request) {
        match request {
            Request::Reload(modules, profiles, reply) => {
                self.reload(modules, profiles).await;
                let _ = reply.send(());
            }
            Request::SetProfile(name, reply) => {
                let result = self.switch_profile(&name).await;
                let _ = reply.send(result);
            }
            Request::SetMode(id, reply) => {
                let result = self.set_mode(id).await;
                let _ = reply.send(result);
            }
        }
    }

    async fn set_mode(&mut self, id: String) -> Result<(), ModeError> {
        let i = self
            .entries
            .iter()
            .position(|e| e.id == id)
            .ok_or(ModeError::Unknown(id))?;
        self.switch_to(i).await;
        Ok(())
    }

    /// Aborts the uploaders and the module tasks when the controller ends.
    fn stop(&mut self) {
        for (_, uploader) in self.uploaders.drain() {
            uploader.abort();
        }
        for entry in self.entries.drain(..) {
            entry.task.abort();
        }
    }

    /// Takes over the docks for the active module, if there is one. `fresh_uploaders` after a
    /// time without modules: what a dock shows may have changed meanwhile (API).
    async fn activate(&mut self, fresh_uploaders: bool) {
        if self.entries.is_empty() {
            return;
        }
        let _ = self.entries[self.active]
            .events
            .send(ModuleEvent::Activated)
            .await;
        let mut page = self.entries[self.active].page.clone();
        if !page.has_changed().unwrap_or(true) {
            let _ = tokio::time::timeout(FIRST_PAGE, page.changed()).await;
        }
        self.entries[self.active].page.mark_changed();
        self.publish();
        if fresh_uploaders {
            let devices: Vec<String> = self.devices.iter().cloned().collect();
            for device in devices {
                self.start_uploader(device);
            }
        }
    }

    async fn reload(&mut self, modules: Vec<Box<dyn Module>>, profiles: Vec<Profile>) {
        let had_modules = !self.entries.is_empty();
        let active = self.entries.get(self.active).map(|e| e.id.clone());
        for entry in self.entries.drain(..) {
            entry.task.abort();
        }
        self.entries = spawn_modules(&self.hub, modules);
        let profile = self.profile.map(|i| self.profiles[i].name.clone());
        self.set_profiles(profiles, profile.as_deref());
        let visible = self.visible();
        self.active = active
            .and_then(|id| self.entries.iter().position(|e| e.id == id))
            .filter(|i| visible.contains(i))
            .or_else(|| visible.first().copied())
            .unwrap_or(0);
        self.banner_until = None;
        let ids: Vec<&str> = self.entries.iter().map(|e| e.id.as_str()).collect();
        tracing::info!(?ids, "modules reloaded");
        if self.entries.is_empty() {
            // Blank the keys once, then leave the docks to the API as at a start without modules.
            self.desired.send_replace(Vec::new());
            self.hub.set_screen(pb::Screen::default());
        } else {
            self.activate(!had_modules).await;
        }
    }

    async fn on_device_event(&mut self, event: pb::DeviceEvent) {
        match event.kind {
            Some(Kind::Attached(_)) => {
                self.devices.insert(event.device_id.clone());
                if !self.entries.is_empty() {
                    self.start_uploader(event.device_id);
                }
            }
            Some(Kind::Detached(_)) => {
                self.devices.remove(&event.device_id);
                if let Some(uploader) = self.uploaders.remove(&event.device_id) {
                    uploader.abort();
                }
            }
            Some(Kind::Key(k)) => {
                if let Ok(key) = u8::try_from(k.key) {
                    self.forward(Input::Key {
                        key,
                        pressed: k.pressed,
                    });
                }
            }
            Some(Kind::Knob(k)) if k.knob == MODE_KNOB => match k.action {
                Some(Action::Turned(delta)) if delta != 0 => self.step(delta.signum()).await,
                Some(Action::Pressed(true)) => self.step(1).await,
                _ => {}
            },
            Some(Kind::Knob(k)) => {
                let Ok(knob) = u8::try_from(k.knob) else { return };
                match k.action {
                    Some(Action::Turned(delta)) => self.forward(Input::KnobTurn { knob, delta }),
                    Some(Action::Pressed(pressed)) => self.forward(Input::KnobPress { knob, pressed }),
                    None => {}
                }
            }
            Some(Kind::Updated(_) | Kind::KeyImage(_)) | None => {}
        }
    }

    fn forward(&self, input: Input) {
        let Some(entry) = self.entries.get(self.active) else {
            return;
        };
        if entry.events.try_send(ModuleEvent::Input(input)).is_err() {
            tracing::warn!(mode = %entry.id, ?input, "module busy or stopped; input dropped");
        }
    }

    /// Turns through the visible modes; from a mode outside the profile (set by the API or a
    /// key), any turn goes to the profile's first mode.
    async fn step(&mut self, by: i32) {
        let visible = self.visible();
        let Some(&first) = visible.first() else { return };
        let next = match visible.iter().position(|&i| i == self.active) {
            Some(at) => {
                let n = i32::try_from(visible.len()).expect("few modules");
                let at = i32::try_from(at).expect("few modules");
                visible[usize::try_from((at + by).rem_euclid(n)).expect("non-negative")]
            }
            None => first,
        };
        self.switch_to(next).await;
    }

    async fn switch_to(&mut self, next: usize) {
        if next == self.active {
            return;
        }
        let _ = self.entries[self.active]
            .events
            .send(ModuleEvent::Deactivated)
            .await;
        self.active = next;
        let _ = self.entries[self.active]
            .events
            .send(ModuleEvent::Activated)
            .await;
        self.entries[self.active].page.mark_changed();
        self.banner_until = Some(Instant::now() + BANNER);
        tracing::info!(mode = %self.entries[self.active].id, "mode switched");
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
            format!("mode {}", self.entries[self.active].title),
        );
        self.publish();
    }

    /// Publishes the active page (with the mode banner, if showing) to the UI and the docks.
    fn publish(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let page = self.entries[self.active].page.borrow_and_update().clone();
        let entry = &self.entries[self.active];
        let mut faces: Vec<KeyFace> = page
            .faces
            .into_iter()
            .filter(|f| LCD_KEYS.contains(&f.key))
            .collect();
        if self.banner_until.is_some() {
            faces.retain(|f| f.key != u32::from(BANNER_KEY));
            faces.push(face(
                BANNER_KEY,
                entry.title.clone(),
                "mode",
                FaceState::FACE_STATE_ACTIVE,
            ));
        }
        faces.sort_by_key(|f| f.key);
        // The web UI draws the now-playing band itself (H15-88): its faces go out without the
        // scroll position and progress, so a scrolling band doesn't re-send the screen every frame.
        let screen_faces = faces
            .iter()
            .cloned()
            .map(|mut f| {
                if let Some(band) = f.band.as_option_mut() {
                    band.offset = 0;
                    band.progress = 0;
                }
                f
            })
            .collect();
        self.hub.set_screen(pb::Screen {
            modes: self
                .visible()
                .into_iter()
                .map(|i| pb::Mode {
                    id: self.entries[i].id.clone(),
                    title: self.entries[i].title.clone(),
                    kind: self.entries[i].kind.to_owned(),
                    ..Default::default()
                })
                .collect(),
            active_mode: entry.id.clone(),
            faces: screen_faces,
            knobs: page.knobs,
            now_playing: page.now_playing.into(),
            profiles: self.profiles.iter().map(|p| p.name.clone()).collect(),
            active_profile: self
                .profile
                .map(|i| self.profiles[i].name.clone())
                .unwrap_or_default(),
            ..Default::default()
        });
        self.desired.send_replace(faces);
    }

    fn start_uploader(&mut self, device_id: String) {
        let task = tokio::spawn(upload(
            Arc::clone(&self.hub),
            device_id.clone(),
            self.desired.subscribe(),
        ));
        if let Some(old) = self.uploaders.insert(device_id, task) {
            old.abort();
        }
    }
}

/// Keeps one dock's LCD keys equal to the desired faces, re-sending only what changed. Faces with a
/// key image are sent again when the uploaded images change (H15-28).
async fn upload(hub: Arc<Hub>, device_id: String, mut desired: watch::Receiver<Vec<KeyFace>>) {
    let mut shown: Shown = Default::default();
    let mut assets = hub.assets().map(|a| a.generation());
    loop {
        let faces = desired.borrow_and_update().clone();
        if !sync_keys(&hub, &device_id, &faces, &mut shown).await {
            return;
        }
        tokio::select! {
            changed = desired.changed() => if changed.is_err() { return },
            Ok(()) = assets_changed(assets.as_mut()) => forget_images(&mut shown),
        }
    }
}

/// What a dock shows, per key 1-6: `None` = unknown (send it), `Some(None)` = blank.
type Shown = [Option<Option<KeyFace>>; 6];

/// Sends every key whose face differs from what the dock shows; `false` once the dock is gone.
async fn sync_keys(hub: &Hub, device_id: &str, faces: &[KeyFace], shown: &mut Shown) -> bool {
    for key in 1..=6u8 {
        let want = faces.iter().find(|f| f.key == u32::from(key)).cloned();
        let slot = &mut shown[usize::from(key - 1)];
        if slot.as_ref() == Some(&want) {
            continue;
        }
        match hub.command(device_id, key_command(hub, key, want.as_ref())).await {
            Ok(()) => *slot = Some(want),
            Err(CommandError::NotFound(_) | CommandError::Gone(_)) => return false,
            Err(e) => tracing::warn!(device = %device_id, key, error = %e, "updating a key failed"),
        }
    }
    true
}

/// The command that shows `face` on `key` (rendered with its image, if the store has it), or blanks it.
fn key_command(hub: &Hub, key: u8, face: Option<&KeyFace>) -> Command {
    let Some(face) = face else {
        return Command::ClearKeyImage(key);
    };
    if let Some(band) = face.band.as_option() {
        return Command::SetKeyImage {
            key,
            image: render::render_band_png(band),
        };
    }
    // An uploaded picture of that name wins over a built-in icon (H15-48).
    let picture = (!face.image.is_empty())
        .then(|| hub.assets().and_then(|a| a.get(&face.image)))
        .flatten();
    let icon = picture.is_none() && crate::icons::exists(&face.image);
    let image = picture
        .as_deref()
        .map(render::KeyImage::Picture)
        .or(icon.then_some(render::KeyImage::Icon(face.image.as_str())));
    if image.is_none() && !face.image.is_empty() {
        tracing::debug!(key, image = %face.image, "key image not found; text only");
    }
    Command::SetKeyImage {
        key,
        image: render::render_png_with(face, image),
    }
}

/// Marks the keys that show an uploaded image as unknown, so they are sent again.
fn forget_images(shown: &mut Shown) {
    for slot in shown {
        if slot
            .as_ref()
            .is_some_and(|f| f.as_ref().is_some_and(|f| !f.image.is_empty()))
        {
            *slot = None;
        }
    }
}

/// The next change of the uploaded key images; never, without a store.
async fn assets_changed(assets: Option<&mut watch::Receiver<u64>>) -> Result<(), watch::error::RecvError> {
    match assets {
        Some(assets) => assets.changed().await,
        None => std::future::pending().await,
    }
}

/// The active page's next change; never, without modules.
async fn page_changed(page: Option<&mut watch::Receiver<Page>>) -> Result<(), watch::error::RecvError> {
    match page {
        Some(page) => page.changed().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use streamdock_hid::{InputEvent, Key, Knob};

    use super::*;
    use crate::hub::{Attached, CommandRequest};
    use crate::modules::demo;

    fn demo_modules(pages: i64) -> Vec<Box<dyn Module>> {
        let mut table = toml::Table::new();
        table.insert("pages".into(), pages.into());
        demo::from_table(&table).unwrap()
    }

    fn dock(hub: &Arc<Hub>) -> Attached {
        hub.attach(pb::Device {
            id: "D".into(),
            ..Default::default()
        })
    }

    /// Waits for the next screen that satisfies `pred`.
    async fn screen_where(
        rx: &mut tokio::sync::broadcast::Receiver<HubEvent>,
        pred: impl Fn(&pb::Screen) -> bool,
    ) -> pb::Screen {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(HubEvent::Screen(s)) = rx.recv().await
                    && pred(&s)
                {
                    return s;
                }
            }
        })
        .await
        .expect("timed out waiting for a screen")
    }

    /// Answers the dock's commands and records them; returns them after `n` commands.
    async fn commands(attached: &mut Attached, n: usize) -> Vec<Command> {
        let mut seen = Vec::new();
        while seen.len() < n {
            let CommandRequest { command, reply } =
                tokio::time::timeout(Duration::from_secs(5), attached.commands.recv())
                    .await
                    .unwrap()
                    .unwrap();
            let _ = reply.send(Ok(()));
            seen.push(command);
        }
        seen
    }

    #[tokio::test]
    async fn knob_3_switches_modes_and_other_input_reaches_the_active_module() {
        let hub = Hub::new();
        let mut rx = hub.subscribe().events;
        let mut attached = dock(&hub);
        let _switch = start(Arc::clone(&hub), demo_modules(2), Profiles::default());

        let s = screen_where(&mut rx, |s| s.active_mode == "demo-1" && !s.faces.is_empty()).await;
        assert_eq!(
            s.modes.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["demo-1", "demo-2"]
        );
        assert_eq!(s.faces.len(), 6);
        // The dock gets all six keys.
        let sent = commands(&mut attached, 6).await;
        assert!(sent.iter().all(|c| matches!(c, Command::SetKeyImage { .. })));

        // Key 4 toggles in demo-1 only.
        attached.publish_input(InputEvent::KeyDown(Key::new(4).unwrap()));
        let s = screen_where(&mut rx, |s| {
            s.faces.iter().any(|f| f.key == 4 && f.detail == "on")
        })
        .await;
        assert_eq!(s.active_mode, "demo-1");
        // Only key 4 is re-sent.
        assert!(matches!(
            commands(&mut attached, 1).await[..],
            [Command::SetKeyImage { key: 4, .. }]
        ));

        // Knob 3 clockwise: demo-2, with the mode name on key 2.
        attached.publish_input(InputEvent::KnobTurn {
            knob: Knob::new(3).unwrap(),
            delta: 1,
        });
        let s = screen_where(&mut rx, |s| s.active_mode == "demo-2").await;
        let banner = s.faces.iter().find(|f| f.key == 2).unwrap();
        assert_eq!(
            (banner.label.as_str(), banner.detail.as_str()),
            ("Demo 2", "mode")
        );
        assert!(
            s.faces.iter().all(|f| f.detail != "on"),
            "demo-2 has its own state"
        );
        // After the banner, key 2 shows demo-2's own face again.
        let s = screen_where(&mut rx, |s| {
            s.faces.iter().any(|f| f.key == 2 && f.detail != "mode")
        })
        .await;
        assert_eq!(s.active_mode, "demo-2");

        // Knob 3 counter-clockwise wraps back; demo-1 kept its state.
        attached.publish_input(InputEvent::KnobTurn {
            knob: Knob::new(3).unwrap(),
            delta: -1,
        });
        screen_where(&mut rx, |s| {
            s.active_mode == "demo-1" && s.faces.iter().any(|f| f.key == 4 && f.detail == "on")
        })
        .await;
    }

    #[tokio::test]
    async fn knob_1_goes_to_the_module_and_set_mode_works() {
        let hub = Hub::new();
        let mut rx = hub.subscribe().events;
        let attached = dock(&hub);
        let switch = start(Arc::clone(&hub), demo_modules(2), Profiles::default());
        screen_where(&mut rx, |s| !s.faces.is_empty()).await;

        attached.publish_input(InputEvent::KnobTurn {
            knob: Knob::new(1).unwrap(),
            delta: 3,
        });
        screen_where(&mut rx, |s| {
            s.faces.iter().any(|f| f.key == 1 && f.detail == "count 3")
        })
        .await;

        switch.set("demo-2").await.unwrap();
        screen_where(&mut rx, |s| s.active_mode == "demo-2").await;
        assert_eq!(switch.set("nope").await, Err(ModeError::Unknown("nope".into())));
        assert_eq!(
            ModeSwitch::disabled().set("demo-1").await,
            Err(ModeError::Unknown("demo-1".into()))
        );
        drop(attached);
    }

    #[tokio::test]
    async fn a_dock_attached_later_gets_the_current_page() {
        let hub = Hub::new();
        let mut rx = hub.subscribe().events;
        let _switch = start(Arc::clone(&hub), demo_modules(1), Profiles::default());
        screen_where(&mut rx, |s| !s.faces.is_empty()).await;
        let mut attached = dock(&hub);
        let sent = commands(&mut attached, 6).await;
        let keys: Vec<_> = sent
            .iter()
            .filter_map(|c| match c {
                Command::SetKeyImage { key, image } => {
                    assert_eq!(&image[1..4], b"PNG");
                    Some(*key)
                }
                _ => None,
            })
            .collect();
        assert_eq!(keys, [1, 2, 3, 4, 5, 6]);
    }

    #[tokio::test]
    async fn reload_replaces_modules_and_keeps_the_active_mode() {
        let hub = Hub::new();
        let mut rx = hub.subscribe().events;
        let mut attached = dock(&hub);
        let switch = start(Arc::clone(&hub), demo_modules(2), Profiles::default());
        screen_where(&mut rx, |s| !s.faces.is_empty()).await;
        commands(&mut attached, 6).await;
        switch.set("demo-2").await.unwrap();
        screen_where(&mut rx, |s| s.active_mode == "demo-2").await;

        // Three pages now: demo-2 still exists and stays active, with fresh module state.
        switch.reload(demo_modules(3), Vec::new()).await.unwrap();
        let s = screen_where(&mut rx, |s| s.modes.len() == 3).await;
        assert_eq!(s.active_mode, "demo-2");

        // One page: demo-2 is gone, so the first mode takes over.
        switch.reload(demo_modules(1), Vec::new()).await.unwrap();
        let s = screen_where(&mut rx, |s| s.modes.len() == 1).await;
        assert_eq!(s.active_mode, "demo-1");

        // Input reaches the new module (knob 1 counts).
        attached.publish_input(InputEvent::KnobTurn {
            knob: Knob::new(1).unwrap(),
            delta: 2,
        });
        screen_where(&mut rx, |s| {
            s.faces.iter().any(|f| f.key == 1 && f.detail == "count 2")
        })
        .await;
    }

    #[tokio::test]
    async fn reload_from_and_to_no_modules() {
        let hub = Hub::new();
        let mut rx = hub.subscribe().events;
        let mut attached = dock(&hub);
        let switch = start(Arc::clone(&hub), Vec::new(), Profiles::default());

        // Modules appear: the attached dock gets the page.
        switch.reload(demo_modules(1), Vec::new()).await.unwrap();
        screen_where(&mut rx, |s| s.active_mode == "demo-1").await;
        let sent = commands(&mut attached, 6).await;
        assert!(sent.iter().all(|c| matches!(c, Command::SetKeyImage { .. })));

        // All gone: the keys are blanked once and the screen has no modes.
        switch.reload(Vec::new(), Vec::new()).await.unwrap();
        screen_where(&mut rx, |s| s.modes.is_empty()).await;
        let sent = commands(&mut attached, 6).await;
        assert!(sent.iter().all(|c| matches!(c, Command::ClearKeyImage(_))));
        assert_eq!(
            ModeSwitch::disabled().reload(Vec::new(), Vec::new()).await,
            Err(ModeError::Stopped)
        );
    }

    fn profile(name: &str, modes: &[&str]) -> Profile {
        Profile {
            name: name.into(),
            modes: modes.iter().map(|m| (*m).to_owned()).collect(),
        }
    }

    #[tokio::test]
    async fn profiles_limit_knob_3_and_the_choice_is_saved() {
        let hub = Hub::new();
        let mut rx = hub.subscribe().events;
        let attached = dock(&hub);
        let state = Arc::new(StateFile::in_memory());
        state.set_profile("Evening");
        let switch = start(
            Arc::clone(&hub),
            demo_modules(3),
            Profiles {
                list: vec![
                    profile("Office", &["demo-1", "demo-2"]),
                    profile("Evening", &["demo-3", "nope", "demo-1"]),
                ],
                state: Some(Arc::clone(&state)),
            },
        );
        // The saved profile is active; unknown ids are skipped; its first mode is shown.
        let s = screen_where(&mut rx, |s| !s.faces.is_empty()).await;
        assert_eq!(
            (s.active_profile.as_str(), s.active_mode.as_str()),
            ("Evening", "demo-3")
        );
        assert_eq!(s.profiles, ["Office", "Evening"]);
        assert_eq!(
            s.modes.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["demo-3", "demo-1"]
        );

        // Knob 3 cycles within the profile: demo-3 -> demo-1 -> demo-3.
        let turn = |delta| InputEvent::KnobTurn {
            knob: Knob::new(3).unwrap(),
            delta,
        };
        attached.publish_input(turn(1));
        screen_where(&mut rx, |s| s.active_mode == "demo-1").await;
        attached.publish_input(turn(1));
        screen_where(&mut rx, |s| s.active_mode == "demo-3").await;

        // Office doesn't have demo-3: its first mode takes over, and the choice is saved.
        switch.set_profile("Office").await.unwrap();
        let s = screen_where(&mut rx, |s| s.active_profile == "Office").await;
        assert_eq!(s.active_mode, "demo-1");
        assert_eq!(state.profile().as_deref(), Some("Office"));
        assert_eq!(
            switch.set_profile("Nope").await,
            Err(ModeError::UnknownProfile("Nope".into()))
        );

        // A reload keeps the profile by name; dropping profiles shows every mode.
        switch
            .reload(demo_modules(3), vec![profile("Office", &["demo-2"])])
            .await
            .unwrap();
        let s = screen_where(&mut rx, |s| s.modes.len() == 1).await;
        assert_eq!(
            (s.active_profile.as_str(), s.active_mode.as_str()),
            ("Office", "demo-2")
        );
        switch.reload(demo_modules(3), Vec::new()).await.unwrap();
        let s = screen_where(&mut rx, |s| s.modes.len() == 3).await;
        assert!(s.profiles.is_empty() && s.active_profile.is_empty());
        assert_eq!(s.active_mode, "demo-2");
    }
}

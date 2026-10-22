//! The device manager: finds real docks, attaches them to the [`Hub`], forwards their input and
//! carries out commands. Survives unplugging and replugging.
//!
//! Hotplug is a poll of `streamdock_hid::list()` every `poll_interval` (it only reads sysfs). Each
//! dock found gets its own task, keyed by USB serial number. A dock task ends when the dock stops
//! answering (read or write error); dropping its [`Attached`] announces the detach, and the next
//! poll opens it again once it is back.
//!
//! The manager remembers what the API set on each dock (brightness, key images), keyed by serial,
//! and re-applies it when the dock comes back, so a replug restores the keys. This lives in memory
//! only; a daemon restart starts blank.
//!
//! Only one daemon drives a dock: before opening it, a dock task takes an exclusive lock on
//! `$XDG_RUNTIME_DIR/streamdockd/<serial>.lock` (hidraw itself lets any number of processes open
//! the dock, and a second opener would reset it). The lock is released when the task ends.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use image::DynamicImage;
use streamdock_hid::{Dock, DockInfo, Error as HidError, InputEvent, KEEP_ALIVE_INTERVAL, Key, Knob};
use streamdock_proto::streamdock::v1 as pb;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

use crate::hub::{Attached, Command, Hub};
use crate::power::{Power, minute_now};
use crate::state::StateFile;

/// Model name reported for real docks.
pub const MODEL: &str = "StreamDock N3";
/// Wait between attempts to open a dock that is listed but can't be opened (busy, permissions).
const OPEN_RETRY: Duration = Duration::from_secs(5);
/// How long a dock task gets to clear the screen on shutdown.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// What the API last set on one dock, re-applied after a replug.
#[derive(Debug, Default, Clone)]
struct Remembered {
    brightness: Option<u8>,
    /// Decoded images by LCD key number (1..=6).
    images: BTreeMap<u8, DynamicImage>,
}

impl Remembered {
    /// Records a command that succeeded on the dock.
    fn record(&mut self, command: &Command, decoded: Option<DynamicImage>) {
        match command {
            Command::SetBrightness(percent) => self.brightness = Some(*percent),
            Command::SetKeyImage { key, .. } => {
                if let Some(image) = decoded {
                    self.images.insert(*key, image);
                }
            }
            Command::ClearKeyImage(key) => {
                self.images.remove(key);
            }
        }
    }
}

/// How often the night window is checked.
const NIGHT_CHECK: Duration = Duration::from_secs(30);

/// Device manager settings.
#[derive(Debug)]
pub struct Options {
    /// Brightness for a dock that has none saved yet.
    pub brightness: u8,
    pub poll_interval: Duration,
    pub power: Power,
    /// Saved brightness per dock, surviving restarts.
    pub state: Arc<StateFile>,
}

/// After a wake, input is ignored this long: asleep, the dock reports one press as a stray release
/// first, then the real press and release (seen on the real dock: `KeyUp`, then 40 ms later
/// `KeyDown`, then `KeyUp`).
const WAKE_GRACE: Duration = Duration::from_millis(400);

/// Sleep and wake (H15-24): asleep, the first input only wakes the dock. Nothing that belongs to
/// that press is forwarded: input during [`WAKE_GRACE`], and the release of a press that started
/// in it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Wake {
    asleep: bool,
    /// Input before this is part of the wake press.
    quiet_until: Option<tokio::time::Instant>,
    swallow: Option<Swallow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Swallow {
    KeyUp(Key),
    KnobUp(Knob),
}

impl Swallow {
    fn is_release(self, event: InputEvent) -> bool {
        match (self, event) {
            (Self::KeyUp(k), InputEvent::KeyUp(up)) => k == up,
            (Self::KnobUp(k), InputEvent::KnobUp(up)) => k == up,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WakeAction {
    /// Pass the input on.
    Forward,
    /// Wake the screen; don't pass the input on.
    Wake,
    /// Part of the press that woke the dock; drop it.
    Drop,
}

impl Wake {
    fn on_input(&mut self, event: InputEvent, now: tokio::time::Instant) -> WakeAction {
        if self.asleep {
            self.asleep = false;
            self.quiet_until = Some(now + WAKE_GRACE);
            self.swallow = Self::release_of(event);
            return WakeAction::Wake;
        }
        if self.quiet_until.is_some_and(|until| now < until) {
            // Still the wake press: drop it. A press there owes us its release; a release there
            // settles that.
            if let Some(release) = Self::release_of(event) {
                self.swallow = Some(release);
            } else if self.swallow.is_some_and(|s| s.is_release(event)) {
                self.swallow = None;
            }
            return WakeAction::Drop;
        }
        self.quiet_until = None;
        if self.swallow.is_some_and(|s| s.is_release(event)) {
            self.swallow = None;
            return WakeAction::Drop;
        }
        WakeAction::Forward
    }

    /// The release that ends a press, for presses.
    fn release_of(event: InputEvent) -> Option<Swallow> {
        match event {
            InputEvent::KeyDown(k) => Some(Swallow::KeyUp(k)),
            InputEvent::KnobDown(k) => Some(Swallow::KnobUp(k)),
            _ => None,
        }
    }
}

/// The keep-alive timer, past its first (immediate) tick: the dock was just initialised.
async fn keep_alive_timer() -> tokio::time::Interval {
    let mut timer = tokio::time::interval(KEEP_ALIVE_INTERVAL);
    timer.tick().await;
    timer
}

/// Remembered state of every dock seen since the daemon started, by serial.
type Memory = Arc<Mutex<HashMap<String, Remembered>>>;

/// Runs the device manager until `shutdown` turns true, then shuts every dock down cleanly.
pub async fn run(hub: Arc<Hub>, options: Options, mut shutdown: watch::Receiver<bool>) {
    let mut manager = Manager {
        hub,
        options: Arc::new(options),
        open: HashSet::new(),
        tasks: JoinSet::new(),
        memory: Arc::default(),
        warned_list: false,
    };
    loop {
        manager.reap();
        manager.discover(&shutdown).await;
        tokio::select! {
            () = tokio::time::sleep(manager.options.poll_interval) => {}
            _ = shutdown.changed() => break,
        }
    }
    // Dock tasks see the same shutdown signal and clear their screens.
    let tasks = &mut manager.tasks;
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
}

/// The device manager's state between polls.
struct Manager {
    hub: Arc<Hub>,
    options: Arc<Options>,
    /// Serials with a running dock task.
    open: HashSet<String>,
    tasks: JoinSet<String>,
    memory: Memory,
    warned_list: bool,
}

impl Manager {
    /// Forgets docks whose task ended, so the next poll opens them again.
    fn reap(&mut self) {
        while let Some(done) = self.tasks.try_join_next() {
            match done {
                Ok(serial) => {
                    self.open.remove(&serial);
                }
                Err(e) => tracing::error!(error = %e, "dock task failed"),
            }
        }
    }

    /// Starts a task for every listed dock that has none.
    async fn discover(&mut self, shutdown: &watch::Receiver<bool>) {
        let docks = match streamdock_hid::list().await {
            Ok(docks) => docks,
            Err(e) => return self.list_failed(&e),
        };
        self.warned_list = false;
        for info in docks {
            let serial = info.serial_number().unwrap_or("unknown").to_owned();
            if self.open.insert(serial.clone()) {
                let dock = DockTask {
                    hub: Arc::clone(&self.hub),
                    options: Arc::clone(&self.options),
                    memory: Arc::clone(&self.memory),
                    shutdown: shutdown.clone(),
                };
                self.tasks.spawn(dock.serve(info, serial));
            }
        }
    }

    fn list_failed(&mut self, e: &HidError) {
        if self.warned_list {
            tracing::debug!(error = %e, "cannot list docks");
        } else {
            self.warned_list = true;
            let problem = dock_problem(e, Path::new("/sys"));
            tracing::warn!(error = %e, "{problem}; will keep trying");
            // The dashboard shows this instead of a bare "plug in the dock" (H15-40).
            self.hub.report(
                crate::hub::DAEMON,
                pb::ActivityLevel::ACTIVITY_LEVEL_ERROR,
                problem,
            );
        }
    }
}

/// Why docks can't be listed, in words a user can act on: the usual cause on small boards is a
/// kernel without hidraw (H15-40). `sys` is `/sys` (a directory in tests).
fn dock_problem(e: &HidError, sys: &Path) -> String {
    let hidraw = sys.join("class/hidraw").exists();
    let dock_on_usb = std::fs::read_dir(sys.join("bus/usb/devices"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|d| {
            let read = |f: &str| std::fs::read_to_string(d.path().join(f)).unwrap_or_default();
            read("idVendor").trim() == "6603" && read("idProduct").trim() == "1003"
        });
    match (hidraw, dock_on_usb) {
        (false, true) => "a StreamDock is on USB, but this kernel has no hidraw support (CONFIG_HIDRAW), so it can't be used".to_owned(),
        (false, false) => "this kernel has no hidraw support (CONFIG_HIDRAW): docks can't be used".to_owned(),
        (true, _) => format!("cannot list docks: {e}"),
    }
}

/// What a dock task needs from the manager.
struct DockTask {
    hub: Arc<Hub>,
    options: Arc<Options>,
    memory: Memory,
    shutdown: watch::Receiver<bool>,
}

impl DockTask {
    /// Opens one dock and serves it until it goes away or the daemon shuts down. Returns its serial.
    async fn serve(mut self, info: DockInfo, serial: String) -> String {
        let Some(_lock) = lock_dock(&serial, &mut self.shutdown).await else {
            return serial;
        };
        let Some(dock) = open_dock(&info, &serial, &mut self.shutdown).await else {
            return serial;
        };
        let mut session = Session::new(dock, serial, &self.options, self.memory);
        session.restore().await;
        let mut attached = self.hub.attach(session.device());
        let (reader, events) = spawn_reader(Arc::clone(&session.dock));
        let reason = session.run(&mut attached, events, &mut self.shutdown).await;
        reader.abort();
        tracing::info!(serial = %session.serial, %reason, "dock gone");
        drop(attached); // announces the detach
        session.serial
    }
}

/// One open dock: what it shows and its power state. Each event handler returns `Err(reason)` when
/// the dock has to be given up.
struct Session {
    dock: Arc<Dock>,
    serial: String,
    options: Arc<Options>,
    memory: Memory,
    power: Power,
    /// The user's brightness (saved across restarts).
    day: u8,
    /// Whether the night window applies right now.
    night: bool,
    wake: Wake,
    idle_deadline: Option<tokio::time::Instant>,
}

impl Session {
    fn new(dock: Arc<Dock>, serial: String, options: &Arc<Options>, memory: Memory) -> Self {
        let power = options.power;
        let day = options.state.brightness(&serial).unwrap_or(options.brightness);
        let mut session = Self {
            dock,
            serial,
            options: Arc::clone(options),
            memory,
            power,
            day,
            night: power.is_night(minute_now()),
            wake: Wake::default(),
            idle_deadline: None,
        };
        session.reset_idle();
        session
    }

    /// Serves the dock until it has to be given up; returns why.
    async fn run(
        &mut self,
        attached: &mut Attached,
        mut events: mpsc::Receiver<InputResult>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> String {
        let mut keep_alive = keep_alive_timer().await;
        let mut night_check = tokio::time::interval(NIGHT_CHECK);
        loop {
            let step = tokio::select! {
                event = events.recv() => self.on_input(event, attached).await,
                Some(req) = attached.commands.recv() => self.on_command(req).await,
                () = sleep_until(self.idle_deadline), if !self.wake.asleep => self.on_idle().await,
                _ = night_check.tick() => self.on_night_check().await,
                _ = keep_alive.tick() => self.keep_alive().await,
                _ = shutdown.changed() => Err(self.shut_down().await),
            };
            if let Err(reason) = step {
                return reason;
            }
        }
    }

    fn device(&self) -> pb::Device {
        pb::Device {
            id: self.serial.clone(),
            model: MODEL.to_owned(),
            firmware_version: self.dock.firmware_version().unwrap_or_default().to_owned(),
            vendor_id: u32::from(streamdock_hid::VENDOR_ID),
            product_id: u32::from(streamdock_hid::PRODUCT_ID),
            brightness: u32::from(self.day),
            ..Default::default()
        }
    }

    /// What the screen should show right now: the night brightness inside the window, else `day`.
    fn brightness(&self) -> u8 {
        self.power.brightness_at(minute_now(), self.day)
    }

    fn reset_idle(&mut self) {
        self.idle_deadline = self.power.sleep_after.map(|d| tokio::time::Instant::now() + d);
    }

    async fn restore(&self) {
        restore(&self.dock, &self.serial, &self.memory, self.brightness()).await;
    }

    async fn on_input(&mut self, event: Option<InputResult>, attached: &Attached) -> Result<(), String> {
        let event = match event {
            Some(Ok(event)) => event,
            Some(Err(e)) => return Err(format!("read failed: {e}")),
            None => return Err("reader stopped".to_owned()),
        };
        self.reset_idle();
        match self.wake.on_input(event, tokio::time::Instant::now()) {
            WakeAction::Forward => publish(attached, event),
            WakeAction::Drop => {}
            WakeAction::Wake => self.wake_up(event).await?,
        }
        Ok(())
    }

    async fn wake_up(&self, event: InputEvent) -> Result<(), String> {
        tracing::info!(serial = %self.serial, ?event, "dock woken (input not forwarded)");
        let brightness = self.brightness();
        self.dock
            .wake(brightness)
            .await
            .map_err(|e| format!("wake failed: {e}"))?;
        restore(&self.dock, &self.serial, &self.memory, brightness).await;
        Ok(())
    }

    async fn on_command(&mut self, req: crate::hub::CommandRequest) -> Result<(), String> {
        let outcome = execute_and_remember(
            &self.dock,
            &self.serial,
            &self.options,
            &self.memory,
            &req.command,
        )
        .await;
        if let (Ok(()), Command::SetBrightness(percent)) = (&outcome, &req.command) {
            self.day = *percent;
        }
        let _ = req.reply.send(outcome);
        Ok(())
    }

    async fn on_idle(&mut self) -> Result<(), String> {
        tracing::info!(serial = %self.serial, "dock idle; screen off");
        self.dock
            .sleep()
            .await
            .map_err(|e| format!("sleep failed: {e}"))?;
        self.wake.asleep = true;
        self.idle_deadline = None;
        Ok(())
    }

    async fn on_night_check(&mut self) -> Result<(), String> {
        let night = self.power.is_night(minute_now());
        if night == self.night {
            return Ok(());
        }
        self.night = night;
        let brightness = self.brightness();
        tracing::info!(serial = %self.serial, night, brightness, "night window changed");
        if self.wake.asleep {
            return Ok(());
        }
        self.dock
            .set_brightness(brightness)
            .await
            .map_err(|e| format!("setting the brightness failed: {e}"))
    }

    async fn keep_alive(&self) -> Result<(), String> {
        self.dock
            .keep_alive()
            .await
            .map_err(|e| format!("keep-alive failed: {e}"))
    }

    /// Clears the dock for a clean exit; returns the reason the session ends.
    async fn shut_down(&self) -> String {
        if let Err(e) = self.dock.shutdown().await {
            tracing::debug!(serial = %self.serial, error = %e, "clearing the dock on shutdown failed");
        }
        "daemon shutting down".to_owned()
    }
}

/// Runs an API command on the dock and remembers what it set (key images for replugs and wakes,
/// brightness across restarts).
async fn execute_and_remember(
    dock: &Dock,
    serial: &str,
    options: &Options,
    memory: &Memory,
    command: &Command,
) -> Result<(), String> {
    match execute(dock, command).await {
        Ok(decoded) => {
            if let Command::SetBrightness(percent) = command {
                options.state.set_brightness(serial, *percent);
            }
            memory
                .lock()
                .expect("memory lock poisoned")
                .entry(serial.to_owned())
                .or_default()
                .record(command, decoded);
            Ok(())
        }
        Err(e) => {
            tracing::warn!(%serial, ?command, error = %e, "command failed");
            Err(e)
        }
    }
}

/// Waits until `deadline`; never resolves for `None`.
async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

/// Directory for the per-dock lock files.
fn lock_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
        .join("streamdockd")
}

/// Lock file path for a dock; the serial is reduced to `[A-Za-z0-9_-]`.
fn lock_path(serial: &str) -> PathBuf {
    let name: String = serial
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    lock_dir().join(format!("{name}.lock"))
}

/// Tries once to take the exclusive lock for a dock. `Ok(None)`: another process holds it.
fn try_lock_dock(serial: &str) -> std::io::Result<Option<File>> {
    let path = lock_path(serial);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// Waits for the dock's lock, so a second daemon leaves a dock alone while another drives it.
/// `Some(Some(file))`: locked while `file` lives. `Some(None)`: locking itself failed (e.g. no
/// writable lock directory); go on without it. `None`: shutting down.
async fn lock_dock(serial: &str, shutdown: &mut watch::Receiver<bool>) -> Option<Option<File>> {
    let mut warned = false;
    loop {
        match try_lock_dock(serial) {
            Ok(Some(lock)) => return Some(Some(lock)),
            Ok(None) => warn_locked_once(serial, &mut warned),
            Err(e) => {
                // Without a working lock we'd rather drive the dock than not at all.
                tracing::warn!(%serial, error = %e, "cannot take the dock lock; continuing without it");
                return Some(None);
            }
        }
        if !wait_to_retry(shutdown).await {
            return None;
        }
    }
}

fn warn_locked_once(serial: &str, warned: &mut bool) {
    if !std::mem::replace(warned, true) {
        tracing::warn!(%serial, lock = %lock_path(serial).display(), "another streamdockd drives this dock; leaving it alone");
    }
}

/// Waits [`OPEN_RETRY`]; `false` if the daemon shuts down first.
async fn wait_to_retry(shutdown: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        () = tokio::time::sleep(OPEN_RETRY) => true,
        _ = shutdown.changed() => false,
    }
}

/// Opens the dock, retrying while it is listed but busy. `None`: it went away or we shut down.
async fn open_dock(info: &DockInfo, serial: &str, shutdown: &mut watch::Receiver<bool>) -> Option<Arc<Dock>> {
    let mut warned = false;
    loop {
        match Dock::open(info).await {
            Ok(dock) => return Some(Arc::new(dock)),
            Err(HidError::NotFound) => return None,
            Err(e) => warn_open_failed(serial, &e, &mut warned),
        }
        if !wait_to_retry(shutdown).await {
            return None;
        }
    }
}

fn warn_open_failed(serial: &str, e: &HidError, warned: &mut bool) {
    if std::mem::replace(warned, true) {
        tracing::debug!(%serial, error = %e, "opening the dock failed again");
    } else {
        tracing::warn!(%serial, error = %e, "cannot open the dock (in use by another program?); retrying");
    }
}

/// Re-applies what the API set before (after a replug or a wake): the brightness given, and the
/// remembered key images.
async fn restore(dock: &Dock, serial: &str, memory: &Memory, brightness: u8) {
    let remembered = memory
        .lock()
        .expect("memory lock poisoned")
        .get(serial)
        .cloned()
        .unwrap_or_default();
    if let Err(e) = dock.set_brightness(brightness).await {
        warn_restore(serial, None, &e);
    }
    for (number, image) in &remembered.images {
        let Some(key) = Key::new(*number) else { continue };
        if let Err(e) = dock.set_key_image(key, image).await {
            warn_restore(serial, Some(*number), &e);
        }
    }
    log_restored(serial, remembered.images.len());
}

/// A failed restore step: the brightness (`key` = `None`) or a key image.
fn warn_restore(serial: &str, key: Option<u8>, e: &HidError) {
    match key {
        None => tracing::warn!(%serial, error = %e, "setting the brightness failed"),
        Some(key) => tracing::warn!(%serial, key, error = %e, "restoring the key image failed"),
    }
}

fn log_restored(serial: &str, keys: usize) {
    if keys > 0 {
        tracing::info!(%serial, keys, "key images restored");
    }
}

type InputResult = Result<InputEvent, HidError>;

/// Reads run in their own task: a cancelled read could lose a report, so the serve loop only
/// ever receives finished events.
fn spawn_reader(dock: Arc<Dock>) -> (tokio::task::JoinHandle<()>, mpsc::Receiver<InputResult>) {
    let (events_tx, events) = mpsc::channel(64);
    let reader = tokio::spawn(async move {
        loop {
            match dock.next_event(None).await {
                Ok(Some(event)) => {
                    if events_tx.send(Ok(event)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = events_tx.send(Err(e)).await;
                    break;
                }
            }
        }
    });
    (reader, events)
}

fn publish(attached: &Attached, event: InputEvent) {
    tracing::debug!(device = attached.id(), ?event, "input");
    attached.publish_input(event);
}

/// Carries out one API command on the dock. Returns the decoded image for `SetKeyImage`.
async fn execute(dock: &Dock, command: &Command) -> Result<Option<DynamicImage>, String> {
    let lcd_key = |n: u8| {
        Key::new(n)
            .filter(|k| k.is_lcd())
            .ok_or_else(|| format!("key {n} has no display"))
    };
    match command {
        Command::SetBrightness(percent) => {
            dock.set_brightness(*percent).await.map_err(|e| e.to_string())?;
            Ok(None)
        }
        Command::SetKeyImage { key, image } => {
            let key = lcd_key(*key)?;
            let image =
                image::load_from_memory(image).map_err(|e| format!("cannot decode the image: {e}"))?;
            dock.set_key_image(key, &image).await.map_err(|e| e.to_string())?;
            Ok(Some(image))
        }
        Command::ClearKeyImage(key) => {
            dock.clear_key(lcd_key(*key)?).await.map_err(|e| e.to_string())?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img() -> DynamicImage {
        DynamicImage::new_rgb8(4, 4)
    }

    #[test]
    fn remembers_brightness_and_images_until_cleared() {
        let mut r = Remembered::default();
        r.record(&Command::SetBrightness(30), None);
        r.record(
            &Command::SetKeyImage {
                key: 2,
                image: vec![1],
            },
            Some(img()),
        );
        r.record(
            &Command::SetKeyImage {
                key: 5,
                image: vec![1],
            },
            Some(img()),
        );
        r.record(&Command::SetBrightness(60), None);
        assert_eq!(r.brightness, Some(60));
        assert_eq!(r.images.keys().copied().collect::<Vec<_>>(), [2, 5]);

        r.record(&Command::ClearKeyImage(2), None);
        assert_eq!(r.images.keys().copied().collect::<Vec<_>>(), [5]);
    }

    fn key(n: u8) -> Key {
        Key::new(n).unwrap()
    }

    fn at(t0: tokio::time::Instant, ms: u64) -> tokio::time::Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn the_real_docks_wake_sequence_triggers_nothing() {
        // Seen on the real dock while asleep: a stray KeyUp, then the press 40 ms later.
        let t0 = tokio::time::Instant::now();
        let mut w = Wake {
            asleep: true,
            ..Wake::default()
        };
        assert_eq!(w.on_input(InputEvent::KeyUp(key(1)), t0), WakeAction::Wake);
        assert_eq!(
            w.on_input(InputEvent::KeyDown(key(1)), at(t0, 40)),
            WakeAction::Drop
        );
        assert_eq!(
            w.on_input(InputEvent::KeyUp(key(1)), at(t0, 110)),
            WakeAction::Drop
        );
        // The next press (2 s later) works normally.
        assert_eq!(
            w.on_input(InputEvent::KeyDown(key(1)), at(t0, 2_130)),
            WakeAction::Forward
        );
        assert_eq!(
            w.on_input(InputEvent::KeyUp(key(1)), at(t0, 2_430)),
            WakeAction::Forward
        );
    }

    #[test]
    fn a_long_wake_press_is_swallowed_until_its_release() {
        let t0 = tokio::time::Instant::now();
        let mut w = Wake {
            asleep: true,
            ..Wake::default()
        };
        assert_eq!(w.on_input(InputEvent::KeyDown(key(3)), t0), WakeAction::Wake);
        // Released after the grace period: still part of the wake press.
        assert_eq!(
            w.on_input(InputEvent::KeyUp(key(3)), at(t0, 900)),
            WakeAction::Drop
        );
        assert_eq!(
            w.on_input(InputEvent::KeyDown(key(3)), at(t0, 1_500)),
            WakeAction::Forward
        );
    }

    #[test]
    fn a_turn_wakes_and_later_turns_pass() {
        let t0 = tokio::time::Instant::now();
        let knob = Knob::new(1).unwrap();
        let mut w = Wake {
            asleep: true,
            ..Wake::default()
        };
        assert_eq!(
            w.on_input(InputEvent::KnobTurn { knob, delta: 1 }, t0),
            WakeAction::Wake
        );
        assert_eq!(
            w.on_input(InputEvent::KnobTurn { knob, delta: 1 }, at(t0, 100)),
            WakeAction::Drop
        );
        assert_eq!(
            w.on_input(InputEvent::KnobTurn { knob, delta: 1 }, at(t0, 600)),
            WakeAction::Forward
        );
    }

    #[test]
    fn awake_input_is_forwarded() {
        let t0 = tokio::time::Instant::now();
        let mut w = Wake::default();
        assert_eq!(w.on_input(InputEvent::KeyDown(key(1)), t0), WakeAction::Forward);
        assert_eq!(
            w.on_input(InputEvent::KeyUp(key(1)), at(t0, 10)),
            WakeAction::Forward
        );
    }

    #[test]
    fn lock_is_exclusive_per_serial() {
        let serial = format!("test-{}", std::process::id());
        let first = try_lock_dock(&serial).unwrap().expect("first lock");
        assert!(try_lock_dock(&serial).unwrap().is_none(), "second lock must fail");
        assert!(
            try_lock_dock(&format!("{serial}-other")).unwrap().is_some(),
            "other docks are independent"
        );
        drop(first);
        assert!(
            try_lock_dock(&serial).unwrap().is_some(),
            "free again after release"
        );
        let _ = std::fs::remove_file(lock_path(&serial));
        let _ = std::fs::remove_file(lock_path(&format!("{serial}-other")));
    }

    #[test]
    fn lock_path_sanitizes_the_serial() {
        assert_eq!(lock_path("../x y/0300").file_name().unwrap(), "___x_y_0300.lock");
    }

    #[test]
    fn a_new_image_replaces_the_old_one() {
        let mut r = Remembered::default();
        r.record(
            &Command::SetKeyImage {
                key: 1,
                image: vec![1],
            },
            Some(img()),
        );
        r.record(
            &Command::SetKeyImage {
                key: 1,
                image: vec![2],
            },
            Some(DynamicImage::new_rgb8(8, 8)),
        );
        assert_eq!(r.images.len(), 1);
        assert_eq!(r.images[&1].width(), 8);
    }

    /// H15-40: the reason names the missing hidraw, and whether a dock is on USB.
    #[test]
    fn explains_why_docks_cant_be_listed() {
        let sys = std::env::temp_dir().join(format!("streamdockd-sys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&sys);
        let dock = sys.join("bus/usb/devices/1-1");
        std::fs::create_dir_all(&dock).unwrap();
        let e = HidError::NotFound;
        assert!(dock_problem(&e, &sys).starts_with("this kernel has no hidraw"));
        std::fs::write(dock.join("idVendor"), "6603\n").unwrap();
        std::fs::write(dock.join("idProduct"), "1003\n").unwrap();
        assert!(dock_problem(&e, &sys).starts_with("a StreamDock is on USB, but"));
        std::fs::create_dir_all(sys.join("class/hidraw")).unwrap();
        assert!(dock_problem(&e, &sys).starts_with("cannot list docks:"));
        std::fs::remove_dir_all(&sys).unwrap();
    }
}

//! Custom keys (H15-23): pages of keys and knobs whose actions are written in the config, like
//! Stream Deck / OpenDeck actions. Each `[[modules.keys.page]]` is one mode.
//!
//! ```toml
//! [modules.keys]
//! allow_commands = false          # true allows `command` actions (they run as the daemon's user)
//!
//! [[modules.keys.page]]
//! title = "Office"                # mode name; id defaults to "keys", "keys-2", …
//!
//! [[modules.keys.page.key]]
//! key = 1                         # 1-6 LCD keys (with a face), 7-9 buttons
//! label = "Webhook"
//! detail = "n8n"                  # optional second line
//! action = { type = "http", url = "https://example.org/hook", method = "POST", body = "{}" }
//!
//! [[modules.keys.page.key]]
//! key = 2
//! label = "Spots"
//! action = { type = "toggle", on_label = "Spots on", off_label = "Spots off",
//!            on = { type = "homeassistant", service = "light.turn_on", data = { entity_id = "light.x" } },
//!            off = { type = "homeassistant", service = "light.turn_off", data = { entity_id = "light.x" } } }
//!
//! [[modules.keys.page.knob]]
//! knob = 1
//! label = "Brightness"
//! turn = { type = "brightness", step = 5 }      # per detent; `{delta}` in strings = signed detents
//! press = { type = "brightness", set = 80 }
//! ```
//!
//! Action types are documented in [`action::Action::parse`]: `http`, `command`, `mode`,
//! `brightness`, `homeassistant` (uses `[modules.homeassistant]` url and token), `lyrion` (uses
//! `[modules.lyrion]` url and player), `multi` (steps, with `{ type = "delay", ms = … }`) and
//! `toggle`. Actions run in the background; a key shows "ok" or "failed" for a moment after.
//! `assist` (H15-43, only directly on a key) listens while the key is held and shows
//! "listening…", "thinking…", then Home Assistant's answer ([`crate::assist`]).

pub mod action;
pub mod editor;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::{
    Activity, Context, FaceState, Input, KeyFace, Module, ModuleEvent, ModuleIo, ModuleTask, Page, face, knob,
};
use crate::assist::{Progress, Update};
use action::{Action, Env, HomeAssistantTarget, LyrionTarget};

/// How long a key shows the outcome of its action.
const FLASH: Duration = Duration::from_millis(1500);
/// How long an assist key shows the answer (or the error).
const ASSIST_FLASH: Duration = Duration::from_secs(6);
const LCD_KEYS: std::ops::RangeInclusive<u8> = 1..=6;
/// How often a toggle that follows a Home Assistant entity reads its state while the page shows.
const STATE_EVERY: Duration = Duration::from_secs(5);
/// After a press, polled states are ignored this long, so the key doesn't flip back.
const STATE_GRACE: Duration = Duration::from_secs(2);

/// Builds one module per `[[modules.keys.page]]`.
pub fn from_table(table: &toml::Table, context: &Context) -> anyhow::Result<Vec<Box<dyn Module>>> {
    for key in table.keys() {
        if !["enabled", "allow_commands", "page"].contains(&key.as_str()) {
            bail!("[modules.keys] unknown setting {key:?} (known: allow_commands, page)");
        }
    }
    let allow_commands = match table.get("allow_commands") {
        None => false,
        Some(v) => v
            .as_bool()
            .context("[modules.keys] allow_commands must be true or false")?,
    };
    let pages = table
        .get("page")
        .and_then(toml::Value::as_array)
        .context("[modules.keys] needs at least one [[modules.keys.page]]")?;
    anyhow::ensure!(
        !pages.is_empty(),
        "[modules.keys] needs at least one [[modules.keys.page]]"
    );

    let mut parsed = Vec::new();
    let mut ids = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let default_id = if i == 0 {
            "keys".to_owned()
        } else {
            format!("keys-{}", i + 1)
        };
        let page = PageDef::parse(page, &default_id, allow_commands)
            .with_context(|| format!("[[modules.keys.page]] {}", i + 1))?;
        anyhow::ensure!(
            !ids.contains(&page.id),
            "[[modules.keys.page]] id {:?} is used twice",
            page.id
        );
        ids.push(page.id.clone());
        parsed.push(page);
    }

    let (needs_ha, needs_lyrion) =
        parsed
            .iter()
            .flat_map(|p| p.slots.iter())
            .fold((false, false), |(h, l), s| {
                let (h2, l2) = s.action.needs();
                (h || h2, l || l2)
            });
    let homeassistant = needs_ha
        .then(|| -> anyhow::Result<HomeAssistantTarget> {
            let table = context.tables.get("homeassistant").context(
                "[modules.keys] homeassistant actions need [modules.homeassistant] (url, token_file)",
            )?;
            let config = super::homeassistant::Config::from_table(table)?;
            Ok(HomeAssistantTarget {
                url: config.url,
                token_file: config.token_file,
            })
        })
        .transpose()?;
    let lyrion = needs_lyrion
        .then(|| -> anyhow::Result<LyrionTarget> {
            let table = context
                .tables
                .get("lyrion")
                .context("[modules.keys] lyrion actions need [modules.lyrion] (url, player)")?;
            let text = |key: &str| {
                table
                    .get(key)
                    .and_then(toml::Value::as_str)
                    .with_context(|| format!("[modules.lyrion] {key} is required"))
            };
            Ok(LyrionTarget {
                client: super::lyrion::client::Client::new(text("url")?)?,
                player: text("player")?.to_owned(),
                player_ids: Arc::default(),
            })
        })
        .transpose()?;

    let http = reqwest::Client::builder().build()?;
    let assist = assist_service(&parsed, context)?;
    let env = Env {
        hub: Arc::clone(&context.hub),
        modes: Arc::clone(&context.modes),
        http,
        allow_commands,
        homeassistant,
        lyrion,
        assist,
    };
    Ok(parsed
        .into_iter()
        .map(|def| {
            Box::new(Keys {
                def,
                env: env.clone(),
            }) as Box<dyn Module>
        })
        .collect())
}

/// The push-to-talk service when a key has an `assist` action (H15-43).
fn assist_service(
    pages: &[PageDef],
    context: &Context,
) -> anyhow::Result<Option<Arc<crate::assist::Assist>>> {
    let wants = pages
        .iter()
        .flat_map(|p| p.slots.iter())
        .any(|s| matches!(s.action, Action::Assist(_)));
    if !wants {
        return Ok(None);
    }
    crate::assist::from_context(context)
}

/// Where a slot sits on the dock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Key(u8),
    KnobTurn(u8),
    KnobPress(u8),
}

/// One configured key, knob turn or knob press.
#[derive(Debug, Clone, PartialEq)]
struct SlotDef {
    place: Place,
    label: String,
    detail: String,
    /// Uploaded key image (H15-28), by name.
    image: String,
    action: Action,
}

#[derive(Debug, Clone, PartialEq)]
struct PageDef {
    id: String,
    title: String,
    slots: Vec<SlotDef>,
    /// Knob roles shown in the UI: (knob, label).
    knobs: Vec<(u8, String)>,
}

fn text(t: &toml::Table, key: &str) -> anyhow::Result<Option<String>> {
    t.get(key)
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .with_context(|| format!("{key} must be a string"))
        })
        .transpose()
}

/// A key's `image`: the name of an uploaded image (H15-28); empty for none.
fn image(t: &toml::Table) -> anyhow::Result<String> {
    let name = text(t, "image")?.unwrap_or_default();
    anyhow::ensure!(
        name.is_empty() || crate::assets::valid_name(&name),
        "image {name:?} is not an image name (lowercase letters, digits, - and _)"
    );
    Ok(name)
}

fn number(t: &toml::Table, key: &str, range: std::ops::RangeInclusive<u8>) -> anyhow::Result<u8> {
    t.get(key)
        .and_then(toml::Value::as_integer)
        .and_then(|n| u8::try_from(n).ok())
        .filter(|n| range.contains(n))
        .with_context(|| format!("{key} must be {}-{}", range.start(), range.end()))
}

impl PageDef {
    fn parse(value: &toml::Value, default_id: &str, allow_commands: bool) -> anyhow::Result<Self> {
        let table = value.as_table().context("a page must be a table")?;
        for key in table.keys() {
            if !["id", "title", "key", "knob"].contains(&key.as_str()) {
                bail!("unknown setting {key:?} (known: id, title, key, knob)");
            }
        }
        let list = |key: &str| -> anyhow::Result<Vec<&toml::Table>> {
            match table.get(key) {
                None => Ok(Vec::new()),
                Some(v) => v
                    .as_array()
                    .with_context(|| format!("{key} must be a list of [[…{key}]] tables"))?
                    .iter()
                    .map(|t| {
                        t.as_table()
                            .with_context(|| format!("{key} must be a list of tables"))
                    })
                    .collect(),
            }
        };

        let id = text(table, "id")?.unwrap_or_else(|| default_id.to_owned());
        anyhow::ensure!(!id.is_empty(), "id must not be empty");
        let title = text(table, "title")?.context("title is required")?;
        let mut slots = Vec::new();
        for key in list("key")? {
            for k in key.keys() {
                if !["key", "label", "detail", "image", "action"].contains(&k.as_str()) {
                    bail!("key: unknown setting {k:?} (known: key, label, detail, image, action)");
                }
            }
            let n = number(key, "key", 1..=9)?;
            anyhow::ensure!(
                !slots.iter().any(|s: &SlotDef| s.place == Place::Key(n)),
                "key {n} is configured twice"
            );
            let action = key
                .get("action")
                .with_context(|| format!("key {n}: action is required"))?;
            slots.push(SlotDef {
                place: Place::Key(n),
                label: text(key, "label")?.unwrap_or_default(),
                detail: text(key, "detail")?.unwrap_or_default(),
                image: image(key).with_context(|| format!("key {n}"))?,
                action: Action::parse(action, allow_commands).with_context(|| format!("key {n}"))?,
            });
        }
        let mut knobs = Vec::new();
        for k in list("knob")? {
            for key in k.keys() {
                if !["knob", "label", "turn", "press"].contains(&key.as_str()) {
                    bail!("knob: unknown setting {key:?} (known: knob, label, turn, press)");
                }
            }
            // Knob 3 switches modes.
            let n = number(k, "knob", 1..=2)?;
            anyhow::ensure!(
                !knobs.iter().any(|(m, _)| *m == n),
                "knob {n} is configured twice"
            );
            let label = text(k, "label")?.unwrap_or_default();
            anyhow::ensure!(
                k.contains_key("turn") || k.contains_key("press"),
                "knob {n}: give turn, press or both"
            );
            for (key, place) in [("turn", Place::KnobTurn(n)), ("press", Place::KnobPress(n))] {
                if let Some(action) = k.get(key) {
                    slots.push(SlotDef {
                        place,
                        label: label.clone(),
                        detail: String::new(),
                        image: String::new(),
                        action: Action::parse(action, allow_commands)
                            .and_then(|a| {
                                anyhow::ensure!(
                                    !matches!(a, Action::Assist(_)),
                                    "assist works only on keys (held = listening)"
                                );
                                Ok(a)
                            })
                            .with_context(|| format!("knob {n} {key}"))?,
                    });
                }
            }
            knobs.push((n, label));
        }
        // An empty page is fine (blank keys): the editor creates pages before their keys (H15-29).
        Ok(Self {
            id,
            title,
            slots,
            knobs,
        })
    }
}

/// A page of custom keys, as a mode.
#[derive(Debug)]
pub struct Keys {
    def: PageDef,
    env: Env,
}

/// What a slot is doing.
#[derive(Debug, Default)]
struct SlotState {
    /// Toggles: which side runs next press is `!on`.
    on: bool,
    running: bool,
    /// Knob detents that arrived while the turn action was running.
    pending: i32,
    /// Outcome shown until the instant passes: (ok, until).
    flash: Option<(bool, Instant)>,
    /// Toggles that follow a Home Assistant entity: a state request is on its way.
    polling: bool,
    /// When the last press of this slot finished; a toggle ignores polled states for a moment
    /// after it (Home Assistant may not have applied the change yet).
    pressed: Option<Instant>,
    /// Assist keys: the running request.
    assist: Option<AssistRun>,
    /// Assist keys: what the second line shows while a request runs ("listening…", the text).
    status: Option<String>,
    /// Shown instead of "ok" / "failed" while the flash lasts (the assist answer or error).
    flash_text: Option<String>,
}

/// An assist key's running request (H15-43, H15-69).
#[derive(Debug)]
struct AssistRun {
    request: crate::assist::Request,
    /// Still recording (a tap stops it); after that a tap cancels.
    listening: bool,
}

/// A polled toggle state: slot index, whether the entity is on.
type Polled = (usize, anyhow::Result<bool>);

/// A finished action: slot index, toggle side it switched to (if any), outcome.
type Done = (usize, Option<bool>, Result<(), String>);

/// Where actions report: the module's own queue of finished actions, and the activity feed.
struct Io {
    done: mpsc::UnboundedSender<Done>,
    activity: Activity,
    /// Updates of running assist requests, by slot.
    assist: mpsc::UnboundedSender<(usize, Update)>,
}

impl SlotDef {
    /// How the activity feed names the slot: its label, else its place.
    fn name(&self) -> String {
        match (self.label.is_empty(), self.place) {
            (false, Place::KnobTurn(_)) => format!("{} (turn)", self.label),
            (false, Place::KnobPress(_)) => format!("{} (press)", self.label),
            (false, Place::Key(_)) => self.label.clone(),
            (true, Place::Key(n)) if n >= 7 => format!("button {n}"),
            (true, Place::Key(n)) => format!("key {n}"),
            (true, Place::KnobTurn(n)) => format!("knob {n} turn"),
            (true, Place::KnobPress(n)) => format!("knob {n} press"),
        }
    }
}

impl Keys {
    fn slot(&self, place: Place) -> Option<usize> {
        self.def.slots.iter().position(|s| s.place == place)
    }

    /// Starts slot `i`'s action with `detents`.
    fn start(&self, i: usize, detents: i32, states: &mut [SlotState], io: &Io) {
        let state = &mut states[i];
        let def = &self.def.slots[i];
        let (action, side) = match &def.action {
            Action::Toggle { on, off, .. } => {
                // Optimistic: show the new side now, revert if it fails.
                state.on = !state.on;
                (
                    if state.on { (**on).clone() } else { (**off).clone() },
                    Some(state.on),
                )
            }
            other => (other.clone(), None),
        };
        state.running = true;
        let env = self.env.clone();
        let (done, activity) = (io.done.clone(), io.activity.clone());
        let (page, place) = (self.def.id.clone(), def.place);
        let what = format!("{}: {}", def.name(), action.describe(detents));
        tokio::spawn(async move {
            let result = action.run(&env, detents).await.map_err(|e| format!("{e:#}"));
            match &result {
                Ok(()) => {
                    tracing::info!(page, ?place, "keys: action done");
                    activity.ok(what);
                }
                Err(e) => {
                    tracing::warn!(page, ?place, error = %e, "keys: action failed");
                    activity.error(format!("{what} failed: {e}"));
                }
            }
            let _ = done.send((i, side, result));
        });
    }

    /// The assist key went down: start a request; its updates come back through `io.assist`.
    fn start_assist(&self, i: usize, mode: crate::assist::Mode, states: &mut [SlotState], io: &Io) {
        let state = &mut states[i];
        let Some(assist) = &self.env.assist else {
            state.flash = Some((false, Instant::now() + FLASH));
            return;
        };
        let mut request = assist.request(mode);
        let (tx, mut updates) = (io.assist.clone(), request.take_updates());
        state.running = true;
        state.assist = Some(AssistRun {
            request,
            listening: true,
        });
        state.status = Some("…".into());
        state.flash = None;
        tokio::spawn(async move {
            while let Some(update) = updates.recv().await {
                if tx.send((i, update)).is_err() {
                    break;
                }
            }
        });
    }

    /// A press or release of an assist key (H15-43, H15-69).
    fn assist_input(
        &self,
        i: usize,
        mode: crate::assist::Mode,
        pressed: bool,
        states: &mut [SlotState],
        io: &Io,
    ) {
        use crate::assist::Mode;
        if pressed && states[i].assist.is_none() {
            self.start_assist(i, mode, states, io);
            return;
        }
        let Some(run) = states[i].assist.as_mut() else {
            return;
        };
        match (mode, pressed) {
            // Hold: the release ends the recording.
            (Mode::Hold, false) => run.request.stop(),
            // Tap: a tap while recording ends it now; a tap while answering cancels.
            (Mode::Tap, true) if run.listening => run.request.stop(),
            (Mode::Tap, true) => run.request.cancel(),
            _ => {}
        }
    }

    /// A running assist request moved on: the key shows it, the activity feed records it.
    fn on_assist(&self, (i, update): (usize, Update), states: &mut [SlotState], io: &Io) {
        let state = &mut states[i];
        let name = self.def.slots[i].name();
        match update {
            Update::Progress(Progress::Listening) => {
                state.status = Some("listening…".into());
                io.activity.info(format!("{name}: listening"));
            }
            Update::Progress(Progress::Thinking) => {
                if let Some(run) = state.assist.as_mut() {
                    run.listening = false;
                }
                state.status = Some("thinking…".into());
            }
            Update::Progress(Progress::Heard(text)) => {
                tracing::info!(page = %self.def.id, heard = %text, "keys: assist");
                io.activity.info(format!("{name}: heard “{text}”"));
                state.status = Some(text);
            }
            Update::Progress(Progress::Answer(text)) => {
                tracing::info!(page = %self.def.id, answer = %text, "keys: assist");
                io.activity.ok(format!("{name}: “{text}”"));
                state.status = Some(text);
            }
            Update::Speaking => {}
            Update::Done(outcome) => {
                let text = if outcome.answer.is_empty() {
                    "ok".to_owned()
                } else {
                    outcome.answer
                };
                self.end_assist(state, true, text);
            }
            Update::Cancelled(reason) => {
                io.activity.info(format!("{name}: {reason}"));
                self.end_assist(state, true, reason);
            }
            Update::Failed(e) => {
                tracing::warn!(page = %self.def.id, error = %e, "keys: assist failed");
                io.activity.error(format!("{name} failed: {e}"));
                self.end_assist(state, false, short_error(&e));
            }
        }
    }

    fn end_assist(&self, state: &mut SlotState, ok: bool, text: String) {
        let _ = self;
        state.running = false;
        state.assist = None;
        state.status = None;
        state.flash = Some((ok, Instant::now() + ASSIST_FLASH));
        state.flash_text = Some(text);
    }

    fn finish(&self, (i, side, result): Done, states: &mut [SlotState], io: &Io) {
        let state = &mut states[i];
        state.running = false;
        state.pressed = Some(Instant::now());
        if let (Some(side), Err(_)) = (side, &result) {
            state.on = !side;
        }
        // Turns are merged; presses only flash.
        let flash = !matches!(self.def.slots[i].place, Place::KnobTurn(_)) || result.is_err();
        if flash {
            state.flash = Some((result.is_ok(), Instant::now() + FLASH));
        }
        let pending = std::mem::take(&mut state.pending);
        if pending != 0 && result.is_ok() {
            self.start(i, pending, states, io);
        }
    }

    fn on_input(&self, input: Input, states: &mut [SlotState], io: &Io) {
        let (place, detents) = match input {
            Input::Key { key, pressed: true } => (Place::Key(key), 1),
            Input::KnobPress { knob, pressed: true } => (Place::KnobPress(knob), 1),
            Input::KnobTurn { knob, delta } => (Place::KnobTurn(knob), delta),
            Input::Key { key, pressed: false } => {
                if let Some(i) = self.slot(Place::Key(key))
                    && let Action::Assist(mode) = self.def.slots[i].action
                {
                    self.assist_input(i, mode, false, states, io);
                }
                return;
            }
            Input::KnobPress { .. } => return,
        };
        let Some(i) = self.slot(place) else { return };
        if let Action::Assist(mode) = self.def.slots[i].action {
            self.assist_input(i, mode, true, states, io);
            return;
        }
        if states[i].running {
            // A press while running is dropped; turns add up and run after.
            if matches!(place, Place::KnobTurn(_)) {
                states[i].pending = states[i].pending.saturating_add(detents);
            }
            return;
        }
        self.start(i, detents, states, io);
    }

    fn page_view(&self, states: &[SlotState], now: Instant) -> Page {
        let faces = self
            .def
            .slots
            .iter()
            .zip(states)
            .filter_map(|(def, state)| {
                let Place::Key(key) = def.place else { return None };
                if !LCD_KEYS.contains(&key) {
                    return None;
                }
                let label = match &def.action {
                    Action::Toggle {
                        on_label: Some(l), ..
                    } if state.on => l.clone(),
                    Action::Toggle {
                        off_label: Some(l), ..
                    } if !state.on => l.clone(),
                    _ => def.label.clone(),
                };
                let flash = state.flash.filter(|(_, until)| *until > now);
                let flash_text =
                    |fallback: &str| state.flash_text.clone().unwrap_or_else(|| fallback.to_owned());
                let (detail, face_state) = match (flash, state.running) {
                    (_, true) => (
                        state.status.clone().unwrap_or_else(|| "…".to_owned()),
                        FaceState::FACE_STATE_ACTIVE,
                    ),
                    (Some((true, _)), _) => (flash_text("ok"), FaceState::FACE_STATE_ACTIVE),
                    (Some((false, _)), _) => (flash_text("failed"), FaceState::FACE_STATE_UNAVAILABLE),
                    (None, false) if state.on => (def.detail.clone(), FaceState::FACE_STATE_ON),
                    (None, false) => (def.detail.clone(), FaceState::FACE_STATE_IDLE),
                };
                let image = match &def.action {
                    Action::Toggle {
                        on_image: Some(i), ..
                    } if state.on => i.clone(),
                    Action::Toggle {
                        off_image: Some(i), ..
                    } if !state.on => i.clone(),
                    _ => def.image.clone(),
                };
                Some(KeyFace {
                    image,
                    ..face(key, label, detail, face_state)
                })
            })
            .collect();
        let knobs = self
            .def
            .knobs
            .iter()
            .map(|(n, label)| knob(*n, label.clone()))
            .collect();
        Page {
            faces,
            knobs,
            now_playing: None,
        }
    }
}

impl Keys {
    /// Asks Home Assistant for the state of every toggle that follows an entity.
    fn poll_states(&self, states: &mut [SlotState], tx: &mpsc::UnboundedSender<Polled>) {
        for (i, def) in self.def.slots.iter().enumerate() {
            let Some(entity) = def.action.followed_entity() else {
                continue;
            };
            if states[i].polling {
                continue;
            }
            states[i].polling = true;
            let (env, tx, entity) = (self.env.clone(), tx.clone(), entity.to_owned());
            tokio::spawn(async move {
                let _ = tx.send((i, action::homeassistant_state(&env, &entity).await));
            });
        }
    }

    fn apply_state(&self, (i, polled): Polled, states: &mut [SlotState]) {
        let state = &mut states[i];
        state.polling = false;
        match polled {
            Ok(on) => {
                let recent = state.pressed.is_some_and(|at| at.elapsed() < STATE_GRACE);
                if !state.running && !recent {
                    state.on = on;
                }
            }
            Err(e) => {
                tracing::debug!(page = %self.def.id, error = %format!("{e:#}"), "keys: toggle state unknown");
            }
        }
    }
}

impl Module for Keys {
    fn id(&self) -> &str {
        &self.def.id
    }

    fn title(&self) -> &str {
        &self.def.title
    }

    fn kind(&self) -> &'static str {
        "keys"
    }

    fn run(self: Box<Self>, mut io: ModuleIo) -> ModuleTask {
        Box::pin(async move {
            let mut states: Vec<SlotState> =
                (0..self.def.slots.len()).map(|_| SlotState::default()).collect();
            let (done, mut done_rx) = mpsc::unbounded_channel::<Done>();
            let (polled_tx, mut polled_rx) = mpsc::unbounded_channel::<Polled>();
            let (assist_tx, mut assist_rx) = mpsc::unbounded_channel::<(usize, Update)>();
            let mut state_tick = tokio::time::interval(STATE_EVERY);
            state_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut active = false;
            let out = Io {
                done,
                activity: io.activity.clone(),
                assist: assist_tx,
            };
            let mut shown = Page::default();
            let publish = |states: &[SlotState], shown: &mut Page, io: &ModuleIo| {
                let page = self.page_view(states, Instant::now());
                if page != *shown {
                    shown.clone_from(&page);
                    io.page.send_replace(page);
                }
            };
            publish(&states, &mut shown, &io);
            loop {
                let next_flash_end = states
                    .iter()
                    .filter_map(|s| s.flash.map(|(_, until)| until))
                    .filter(|until| *until > Instant::now())
                    .min();
                tokio::select! {
                    event = io.events.recv() => match event {
                        None => return,
                        Some(ModuleEvent::Input(input)) => self.on_input(input, &mut states, &out),
                        Some(ModuleEvent::Activated) => {
                            active = true;
                            self.poll_states(&mut states, &polled_tx);
                        }
                        Some(ModuleEvent::Deactivated) => active = false,
                    },
                    Some(done) = done_rx.recv() => self.finish(done, &mut states, &out),
                    Some(polled) = polled_rx.recv() => self.apply_state(polled, &mut states),
                    Some(update) = assist_rx.recv() => self.on_assist(update, &mut states, &out),
                    _ = state_tick.tick(), if active => self.poll_states(&mut states, &polled_tx),
                    () = sleep_until(next_flash_end) => {
                        let now = Instant::now();
                        for s in &mut states {
                            if s.flash.is_some_and(|(_, until)| until <= now) {
                                s.flash = None;
                                s.flash_text = None;
                            }
                        }
                    }
                }
                publish(&states, &mut shown, &io);
            }
        })
    }
}

/// An error short enough for a key's second line: its last cause, at most 40 characters.
fn short_error(e: &str) -> String {
    let last = e.rsplit(": ").next().unwrap_or(e);
    if last.chars().count() > 40 {
        format!("{}…", last.chars().take(39).collect::<String>())
    } else {
        last.to_owned()
    }
}

/// Sleeps until `when`, or forever without one.
async fn sleep_until(when: Option<Instant>) {
    match when {
        Some(when) => tokio::time::sleep_until(when).await,
        None => std::future::pending().await,
    }
}

/// For tests: the parsed pages by id.
#[cfg(test)]
fn parse_pages(src: &str) -> anyhow::Result<std::collections::BTreeMap<String, PageDef>> {
    let table: toml::Table = toml::from_str(src)?;
    let pages = table["page"].as_array().unwrap();
    pages
        .iter()
        .enumerate()
        .map(|(i, p)| PageDef::parse(p, &format!("keys-{}", i + 1), false).map(|d| (d.id.clone(), d)))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::routing::post;
    use tokio::sync::watch;

    use super::*;

    const PAGE: &str = r#"
        [[page]]
        id = "office"
        title = "Office"

        [[page.key]]
        key = 1
        label = "Hook"
        image = "hook"
        action = { type = "http", url = "URL/hook", body = "{\"from\":\"dock\"}" }

        [[page.key]]
        key = 2
        label = "Lamp"
        action = { type = "toggle", on_label = "Lamp on", off_label = "Lamp off", on_image = "lamp-on", off_image = "lamp-off",
                   on = { type = "http", url = "URL/on" }, off = { type = "http", url = "URL/off" } }

        [[page.key]]
        key = 3
        label = "Both"
        action = { type = "multi", steps = [
            { type = "http", url = "URL/hook" }, { type = "delay", ms = 10 }, { type = "http", url = "URL/hook" } ] }

        [[page.key]]
        key = 7
        action = { type = "http", url = "URL/missing" }

        [[page.knob]]
        knob = 1
        label = "Turn"
        turn = { type = "http", url = "URL/turn?d={delta}" }
    "#;

    #[test]
    fn parses_a_page() {
        let pages = parse_pages(&PAGE.replace("URL", "http://x")).unwrap();
        let page = &pages["office"];
        assert_eq!(page.title, "Office");
        assert_eq!(page.slots.len(), 5);
        assert_eq!(page.knobs, vec![(1, "Turn".to_owned())]);
        assert_eq!(page.slots[4].place, Place::KnobTurn(1));
    }

    #[test]
    fn rejects_bad_pages() {
        for (src, why) in [
            (
                "[[page]]\n[[page.key]]\nkey = 1\naction = { type = \"mode\", id = \"a\" }",
                "title is required",
            ),
            (
                "[[page]]\ntitle = \"x\"\n[[page.key]]\nkey = 10\naction = { type = \"mode\", id = \"a\" }",
                "key must be 1-9",
            ),
            (
                "[[page]]\ntitle = \"x\"\n[[page.knob]]\nknob = 3\nturn = { type = \"mode\", id = \"a\" }",
                "knob must be 1-2",
            ),
            (
                "[[page]]\ntitle = \"x\"\n[[page.key]]\nkey = 1",
                "action is required",
            ),
            (
                "[[page]]\ntitle = \"x\"\n[[page.key]]\nkey = 1\nimage = \"../x\"\naction = { type = \"mode\", id = \"a\" }",
                "not an image name",
            ),
            (
                "[[page]]\ntitle = \"x\"\n[[page.key]]\nkey = 1\naction = { type = \"mode\", id = \"a\" }\n[[page.key]]\nkey = 1\naction = { type = \"mode\", id = \"a\" }",
                "twice",
            ),
        ] {
            let err = format!("{:#}", parse_pages(src).unwrap_err());
            assert!(err.contains(why), "{src}: {err}");
        }
    }

    #[test]
    fn integrations_must_be_configured() {
        let hub = crate::hub::Hub::new();
        let table: toml::Table = toml::from_str(
            "[[page]]\ntitle = \"x\"\n[[page.key]]\nkey = 1\naction = { type = \"homeassistant\", service = \"light.toggle\" }",
        )
        .unwrap();
        let err = from_table(&table, &Context::new(Arc::clone(&hub), &BTreeMap::new()))
            .err()
            .unwrap();
        assert!(format!("{err:#}").contains("[modules.homeassistant]"), "{err:#}");

        let mut tables = BTreeMap::new();
        tables.insert(
            "homeassistant".to_owned(),
            toml::from_str("url = \"https://ha.example\"\narea = \"Office\"").unwrap(),
        );
        let modules = from_table(&table, &Context::new(hub, &tables)).unwrap();
        assert_eq!(modules[0].id(), "keys");
    }

    /// A local HTTP server counting hits per path; `/missing` answers 404.
    async fn server() -> (String, Arc<BTreeMap<&'static str, AtomicUsize>>) {
        let hits: Arc<BTreeMap<&'static str, AtomicUsize>> = Arc::new(
            ["/hook", "/on", "/off", "/turn"]
                .into_iter()
                .map(|p| (p, AtomicUsize::new(0)))
                .collect(),
        );
        let counter = |path: &'static str, hits: Arc<BTreeMap<&'static str, AtomicUsize>>| {
            post(move || async move {
                hits[path].fetch_add(1, Ordering::SeqCst);
                "ok"
            })
        };
        let app = axum::Router::new()
            .route("/hook", counter("/hook", Arc::clone(&hits)))
            .route("/on", counter("/on", Arc::clone(&hits)))
            .route("/off", counter("/off", Arc::clone(&hits)))
            .route("/turn", counter("/turn", Arc::clone(&hits)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, hits)
    }

    /// Waits until the server counted `n` hits on `path`.
    async fn wait_hits(hits: &BTreeMap<&'static str, AtomicUsize>, path: &str, n: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while hits[path].load(Ordering::SeqCst) < n {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{path} never got {n} hits: {}", hits[path].load(Ordering::SeqCst)));
    }

    /// Waits until key `key`'s face matches `want`.
    async fn wait_face(
        page: &mut watch::Receiver<Page>,
        key: u32,
        want: impl Fn(&super::super::KeyFace) -> bool,
    ) -> super::super::KeyFace {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(f) = page
                    .borrow_and_update()
                    .faces
                    .iter()
                    .find(|f| f.key == key && want(f))
                {
                    return f.clone();
                }
                page.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("key {key} never matched: {:?}", page.borrow().faces))
    }

    async fn wait_for(page: &mut watch::Receiver<Page>, key: u32, detail: &str) -> super::super::KeyFace {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(f) = page
                    .borrow_and_update()
                    .faces
                    .iter()
                    .find(|f| f.key == key && f.detail == detail)
                {
                    return f.clone();
                }
                page.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("key {key} never showed {detail:?}: {:?}", page.borrow().faces))
    }

    #[tokio::test]
    async fn runs_http_toggle_and_multi_actions() {
        let (url, hits) = server().await;
        let table: toml::Table = toml::from_str(&PAGE.replace("URL", &url)).unwrap();
        let module = from_table(&table, &Context::new(crate::hub::Hub::new(), &BTreeMap::new()))
            .unwrap()
            .remove(0);
        let (events, rx) = mpsc::channel(16);
        let (page_tx, mut page) = watch::channel(Page::default());
        tokio::spawn(module.run(ModuleIo {
            events: rx,
            page: page_tx,
            activity: Activity::disabled(),
        }));
        let press = |key| ModuleEvent::Input(Input::Key { key, pressed: true });

        // Waits on what lasts (server hits, labels, the settled face), not on the 1.5 s "ok" flash,
        // which a busy machine can miss.
        events.send(press(1)).await.unwrap();
        wait_hits(&hits, "/hook", 1).await;
        let hook = wait_face(&mut page, 1, |f| f.detail.is_empty()).await;
        assert_eq!(hook.image, "hook");

        // Toggle: on, then off; the label and image follow.
        events.send(press(2)).await.unwrap();
        wait_hits(&hits, "/on", 1).await;
        let f = wait_face(&mut page, 2, |f| f.label == "Lamp on" && f.detail.is_empty()).await;
        assert_eq!(f.image, "lamp-on");
        assert_eq!(f.state, face(0, "", "", FaceState::FACE_STATE_ON).state);
        events.send(press(2)).await.unwrap();
        wait_hits(&hits, "/off", 1).await;
        let f = wait_face(&mut page, 2, |f| f.label == "Lamp off" && f.detail.is_empty()).await;
        assert_eq!(f.image, "lamp-off");
        assert_eq!(hits["/on"].load(Ordering::SeqCst), 1);

        // Multi: both steps.
        events.send(press(3)).await.unwrap();
        wait_hits(&hits, "/hook", 3).await;

        // Knob turns reach the server.
        events
            .send(ModuleEvent::Input(Input::KnobTurn { knob: 1, delta: 2 }))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while hits["/turn"].load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    /// A Home Assistant stand-in with one light: `GET /api/states/light.a`, `turn_on` / `turn_off`.
    async fn home_assistant(on: Arc<std::sync::atomic::AtomicBool>) -> String {
        let (get, set_on, set_off) = (Arc::clone(&on), Arc::clone(&on), on);
        let app = axum::Router::new()
            .route(
                "/api/states/light.a",
                axum::routing::get(move || async move {
                    let state = if get.load(Ordering::SeqCst) { "on" } else { "off" };
                    axum::Json(serde_json::json!({ "entity_id": "light.a", "state": state }))
                }),
            )
            .route(
                "/api/services/light/turn_on",
                post(move || async move {
                    set_on.store(true, Ordering::SeqCst);
                    "[]"
                }),
            )
            .route(
                "/api/services/light/turn_off",
                post(move || async move {
                    set_off.store(false, Ordering::SeqCst);
                    "[]"
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    #[tokio::test]
    async fn a_home_assistant_toggle_follows_the_entity() {
        let light = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let url = home_assistant(Arc::clone(&light)).await;
        let dir = std::env::temp_dir().join(format!("streamdockd-keys-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let token = dir.join("token");
        std::fs::write(&token, "test-token\n").unwrap();
        let mut tables = BTreeMap::new();
        tables.insert(
            "homeassistant".to_owned(),
            toml::from_str(&format!(
                "url = \"{url}\"\narea = \"Office\"\ntoken_file = \"{}\"",
                token.display()
            ))
            .unwrap(),
        );
        let table: toml::Table = toml::from_str(
            r#"
            [[page]]
            title = "x"
            [[page.key]]
            key = 2
            label = "Lamp"
            action = { type = "toggle", on_label = "Lamp on", off_label = "Lamp off",
                       on = { type = "homeassistant", service = "light.turn_on", data = { entity_id = "light.a" } },
                       off = { type = "homeassistant", service = "light.turn_off", data = { entity_id = "light.a" } } }
            "#,
        )
        .unwrap();
        let module = from_table(&table, &Context::new(crate::hub::Hub::new(), &tables))
            .unwrap()
            .remove(0);
        let (events, rx) = mpsc::channel(16);
        let (page_tx, mut page) = watch::channel(Page::default());
        tokio::spawn(module.run(ModuleIo {
            events: rx,
            page: page_tx,
            activity: Activity::disabled(),
        }));
        let label = |page: &watch::Receiver<Page>| {
            page.borrow()
                .faces
                .iter()
                .find(|f| f.key == 2)
                .map(|f| f.label.clone())
        };
        let wait_label = |mut page: watch::Receiver<Page>, want: &'static str| async move {
            tokio::time::timeout(Duration::from_secs(5), async {
                while label(&page).as_deref() != Some(want) {
                    page.changed().await.unwrap();
                }
            })
            .await
            .unwrap_or_else(|_| panic!("never showed {want:?}, got {:?}", label(&page)));
        };

        // The light is already on: shown as on once the page is active.
        events.send(ModuleEvent::Activated).await.unwrap();
        wait_label(page.clone(), "Lamp on").await;

        // A press turns it off (not on, as a blind toggle would).
        events
            .send(ModuleEvent::Input(Input::Key {
                key: 2,
                pressed: true,
            }))
            .await
            .unwrap();
        wait_label(page.clone(), "Lamp off").await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while light.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        // Switched on elsewhere: the key follows on the next poll (here: shown again).
        light.store(true, Ordering::SeqCst);
        tokio::time::sleep(STATE_GRACE).await;
        events.send(ModuleEvent::Deactivated).await.unwrap();
        events.send(ModuleEvent::Activated).await.unwrap();
        wait_label(page.clone(), "Lamp on").await;
        page.borrow_and_update();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_failed_toggle_reverts() {
        let (url, _hits) = server().await;
        let src = format!(
            "[[page]]\ntitle = \"x\"\n[[page.key]]\nkey = 4\nlabel = \"L\"\naction = {{ type = \"toggle\", on = {{ type = \"http\", url = \"{url}/missing\" }}, off = {{ type = \"http\", url = \"{url}/off\" }} }}"
        );
        let table: toml::Table = toml::from_str(&src).unwrap();
        let module = from_table(&table, &Context::new(crate::hub::Hub::new(), &BTreeMap::new()))
            .unwrap()
            .remove(0);
        let (events, rx) = mpsc::channel(16);
        let (page_tx, mut page) = watch::channel(Page::default());
        tokio::spawn(module.run(ModuleIo {
            events: rx,
            page: page_tx,
            activity: Activity::disabled(),
        }));
        events
            .send(ModuleEvent::Input(Input::Key {
                key: 4,
                pressed: true,
            }))
            .await
            .unwrap();
        let f = wait_for(&mut page, 4, "failed").await;
        assert_eq!(f.state, face(0, "", "", FaceState::FACE_STATE_UNAVAILABLE).state);
        let f = wait_for(&mut page, 4, "").await;
        assert_eq!(
            f.state,
            face(0, "", "", FaceState::FACE_STATE_IDLE).state,
            "back to off"
        );
    }

    #[tokio::test]
    async fn an_assist_key_listens_while_held_and_shows_the_answer() {
        let (url, seen) = crate::assist::pipeline::tests::fake_ha(false).await;
        let dir = std::env::temp_dir().join(format!("streamdockd-assist-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let token = dir.join("token");
        std::fs::write(&token, "secret\n").unwrap();
        // 0.1 s of 16 kHz mono silence as the "microphone".
        let wav = dir.join("in.wav");
        let mut file =
            b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data\x80\x0c\0\0"
                .to_vec();
        file.extend_from_slice(&[0; 3200]);
        std::fs::write(&wav, file).unwrap();
        let mut tables = BTreeMap::new();
        tables.insert(
            "homeassistant".to_owned(),
            toml::from_str(&format!(
                "url = \"{url}\"\ntoken_file = \"{}\"\n[assist]\ntest_input = \"{}\"\nspeak = false",
                token.display(),
                wav.display()
            ))
            .unwrap(),
        );
        let table: toml::Table = toml::from_str(
            r#"
            [[page]]
            title = "x"
            [[page.key]]
            key = 3
            label = "Assist"
            action = { type = "assist" }
            "#,
        )
        .unwrap();
        let module = from_table(&table, &Context::new(crate::hub::Hub::new(), &tables))
            .unwrap()
            .remove(0);
        let (events, rx) = mpsc::channel(16);
        let (page_tx, mut page) = watch::channel(Page::default());
        tokio::spawn(module.run(ModuleIo {
            events: rx,
            page: page_tx,
            activity: Activity::disabled(),
        }));
        events
            .send(ModuleEvent::Input(Input::Key {
                key: 3,
                pressed: true,
            }))
            .await
            .unwrap();
        wait_for(&mut page, 3, "listening…").await;
        // A short press: the test file still plays to its end.
        events
            .send(ModuleEvent::Input(Input::Key {
                key: 3,
                pressed: false,
            }))
            .await
            .unwrap();
        let face = wait_face(&mut page, 3, |f| f.detail == "Es ist 12:00").await;
        assert_eq!(face.label, "Assist");
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.audio_bytes, 3200);
            assert!(seen.ended);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn assist_needs_home_assistant() {
        let table: toml::Table = toml::from_str(
            r#"
            [[page]]
            title = "x"
            [[page.key]]
            key = 3
            action = { type = "assist" }
            "#,
        )
        .unwrap();
        let Err(err) = from_table(&table, &Context::new(crate::hub::Hub::new(), &BTreeMap::new())) else {
            panic!("assist without Home Assistant was accepted");
        };
        assert!(format!("{err:#}").contains("[modules.homeassistant]"), "{err:#}");
        assert_eq!(short_error("connecting: a: io error"), "io error");
        assert_eq!(short_error(&"x".repeat(50)).chars().count(), 40);
    }

    /// A 16 kHz mono 16-bit WAV of `samples`.
    fn wav_bytes(samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut v =
            b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data".to_vec();
        v.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        v.extend_from_slice(&data);
        v
    }

    struct AssistKey {
        events: mpsc::Sender<ModuleEvent>,
        page: watch::Receiver<Page>,
        seen: Arc<std::sync::Mutex<crate::assist::pipeline::tests::Seen>>,
        dir: std::path::PathBuf,
    }

    impl AssistKey {
        async fn tap(&self) {
            for pressed in [true, false] {
                self.events
                    .send(ModuleEvent::Input(Input::Key { key: 3, pressed }))
                    .await
                    .unwrap();
            }
        }
    }

    impl Drop for AssistKey {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Key 3 = `{ type = "assist", mode = "tap" }` against a fake Home Assistant, `input` as the mic.
    async fn tap_key(
        name: &str,
        behavior: crate::assist::pipeline::tests::Behavior,
        input: &[i16],
        extra: &str,
    ) -> AssistKey {
        let (url, seen) = crate::assist::pipeline::tests::fake_ha_with(behavior).await;
        let dir = std::env::temp_dir().join(format!("streamdockd-tap-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let token = dir.join("token");
        std::fs::write(&token, "secret\n").unwrap();
        let wav = dir.join("in.wav");
        std::fs::write(&wav, wav_bytes(input)).unwrap();
        let mut tables = BTreeMap::new();
        tables.insert(
            "homeassistant".to_owned(),
            toml::from_str(&format!(
                "url = \"{url}\"\ntoken_file = \"{}\"\n[assist]\ntest_input = \"{}\"\nspeak = false\n{extra}",
                token.display(),
                wav.display()
            ))
            .unwrap(),
        );
        let table: toml::Table = toml::from_str(
            r#"
            [[page]]
            title = "x"
            [[page.key]]
            key = 3
            label = "Ask"
            action = { type = "assist", mode = "tap" }
            "#,
        )
        .unwrap();
        let module = from_table(&table, &Context::new(crate::hub::Hub::new(), &tables))
            .unwrap()
            .remove(0);
        let (events, rx) = mpsc::channel(16);
        let (page_tx, page) = watch::channel(Page::default());
        tokio::spawn(module.run(ModuleIo {
            events: rx,
            page: page_tx,
            activity: Activity::disabled(),
        }));
        AssistKey {
            events,
            page,
            seen,
            dir,
        }
    }

    fn speech_then_silence() -> Vec<i16> {
        let mut s =
            crate::assist::capture::wav_to_16k_mono(include_bytes!("../../assist/fixtures/speech-1.wav"))
                .unwrap();
        s.extend(vec![0i16; 16_000 * 3]);
        s
    }

    #[tokio::test]
    async fn a_tap_key_ends_at_the_end_of_speech() {
        use crate::assist::pipeline::tests::Behavior;
        let input = speech_then_silence();
        let mut key = tap_key("end", Behavior::Answer, &input, "").await;
        key.tap().await;
        wait_for(&mut key.page, 3, "listening…").await;
        wait_face(&mut key.page, 3, |f| f.detail == "Es ist 12:00").await;
        let seen = key.seen.lock().unwrap();
        assert!(seen.ended, "end marker sent");
        // Speech (1.23 s) + 0.7 s silence, not the whole 4.2 s file.
        let secs = f64::from(u32::try_from(seen.audio_bytes).unwrap()) / 32_000.0;
        assert!((1.8..2.2).contains(&secs), "sent {secs:.2} s");
    }

    #[tokio::test]
    async fn a_second_tap_stops_early() {
        use crate::assist::pipeline::tests::Behavior;
        let input = speech_then_silence();
        let mut key = tap_key("stop", Behavior::Answer, &input, "").await;
        key.tap().await;
        wait_for(&mut key.page, 3, "listening…").await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        key.tap().await;
        wait_face(&mut key.page, 3, |f| f.detail == "Es ist 12:00").await;
        let seen = key.seen.lock().unwrap();
        assert!(seen.ended);
        let secs = f64::from(u32::try_from(seen.audio_bytes).unwrap()) / 32_000.0;
        assert!(secs < 1.0, "stopped after {secs:.2} s");
    }

    #[tokio::test]
    async fn silence_is_nothing_heard_and_sends_no_end() {
        use crate::assist::pipeline::tests::Behavior;
        let mut key = tap_key(
            "silence",
            Behavior::Answer,
            &vec![0i16; 16_000 * 3],
            "no_speech_ms = 1000",
        )
        .await;
        key.tap().await;
        wait_face(&mut key.page, 3, |f| f.detail == "nothing heard").await;
        assert!(!key.seen.lock().unwrap().ended, "nothing heard is not a request");
    }

    #[tokio::test]
    async fn a_tap_while_thinking_cancels() {
        use crate::assist::pipeline::tests::Behavior;
        let input = speech_then_silence();
        let mut key = tap_key("cancel", Behavior::Stall, &input, "").await;
        key.tap().await;
        wait_for(&mut key.page, 3, "thinking…").await;
        key.tap().await;
        wait_face(&mut key.page, 3, |f| f.detail == "cancelled").await;
    }
}

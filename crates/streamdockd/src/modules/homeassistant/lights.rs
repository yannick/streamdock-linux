//! The Home Assistant mode's logic, without I/O: which entities are on the keys (an area's lights,
//! or entities pinned to keys, H15-32), what the keys show, and which service calls an input turns
//! into. `session.rs` feeds it Home Assistant data and sends the calls.

use serde_json::{Value, json};

use crate::modules::{FaceState, Input, KeyFace, LevelStyle, Page, face, knob};

/// LCD keys available for lights.
pub const MAX_LIGHTS: usize = 6;
/// Brightness change per knob detent, in percent.
pub const BRIGHTNESS_STEP_PCT: i32 = 5;
/// Color temperature change per knob detent, in kelvin.
pub const KELVIN_STEP: i32 = 100;

/// What a knob does on a lights tab (`knob1` / `knob2` in the config).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnobFn {
    /// Turn: brightness of the lights that are on. Press: all on/off.
    Brightness,
    /// Turn: color temperature of the lights that are on. Press: all on/off.
    ColorTemp,
    None,
}

impl KnobFn {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "brightness" => Ok(Self::Brightness),
            "color_temp" => Ok(Self::ColorTemp),
            "none" => Ok(Self::None),
            other => Err(format!(
                "unknown knob function {other:?} (brightness, color_temp, none)"
            )),
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Brightness => "brightness",
            Self::ColorTemp => "color_temp",
            Self::None => "none",
        }
    }

    fn label(self) -> Option<&'static str> {
        match self {
            Self::Brightness => Some("Brightness"),
            Self::ColorTemp => Some("Color temp"),
            Self::None => None,
        }
    }
}

/// What a level key shows (H15-97).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelShow {
    Brightness,
    ColorTemp,
}

/// A key that shows the brightness or colour temperature of the lights that are on, as a bar
/// (`level_keys = [{ key, show, label }]`, H15-97).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelKey {
    pub key: u8,
    pub show: LevelShow,
    pub label: String,
}

impl LevelShow {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "brightness" => Ok(Self::Brightness),
            "color_temp" => Ok(Self::ColorTemp),
            other => Err(format!("unknown level {other:?} (brightness, color_temp)")),
        }
    }

    #[must_use]
    pub fn default_label(self) -> &'static str {
        match self {
            Self::Brightness => "Brightness",
            Self::ColorTemp => "Color temp",
        }
    }
}

/// How long a knob turn's own value is shown before Home Assistant's state takes over again: long
/// enough to bridge the batched call and its state change (H15-97).
pub const PREVIEW_HOLD: std::time::Duration = std::time::Duration::from_millis(1500);

/// The values a knob turn asked for, shown at once (optimistic) until [`PREVIEW_HOLD`] after the
/// last turn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preview {
    pub brightness: Option<i32>,
    pub kelvin: Option<i32>,
    pub until: std::time::Instant,
}

/// An entity pinned to a key (`keys = [{ key, entity, label }]`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pin {
    pub key: u8,
    pub entity: String,
    /// Shown instead of the entity's name.
    pub label: Option<String>,
    /// Cover shorthand, or empty for the existing domain behaviour.
    pub action: String,
    /// Explicit domain.service, targeting this entity only.
    pub service: String,
    /// Uploaded key image or built-in icon; empty selects the domain default.
    pub icon: String,
}

impl Pin {
    pub fn validate(&self) -> Result<(), String> {
        if !self.icon.is_empty() && !crate::assets::valid_name(&self.icon) {
            return Err(format!("invalid icon name {:?}", self.icon));
        }
        if !self.action.is_empty() {
            if !self.service.is_empty() {
                return Err("action and service are mutually exclusive".into());
            }
            if !self.entity.starts_with("cover.")
                || !matches!(self.action.as_str(), "open" | "close" | "stop")
            {
                return Err("action must be open, close or stop on a cover entity".into());
            }
        }
        if !self.service.is_empty() {
            let parts: Vec<_> = self.service.split('.').collect();
            if parts.len() != 2
                || parts.iter().any(|p| {
                    p.is_empty()
                        || !p
                            .bytes()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
                })
            {
                return Err("service must be domain.service (lowercase letters, digits, underscores)".into());
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn explicit_service(&self) -> Option<&str> {
        match self.action.as_str() {
            "open" => Some("cover.open_cover"),
            "close" => Some("cover.close_cover"),
            "stop" => Some("cover.stop_cover"),
            _ => (!self.service.is_empty()).then_some(self.service.as_str()),
        }
    }
}

/// One entity on a key (usually a light), with what the keys and knobs need from its state.
#[derive(Debug, Clone, PartialEq)]
pub struct Light {
    /// LCD key 1-6.
    pub key: u8,
    pub entity_id: String,
    pub name: String,
    pub available: bool,
    pub on: bool,
    /// 0-100, when on and dimmable.
    pub brightness_pct: Option<u8>,
    pub kelvin: Option<i32>,
    /// Supported color temperature range, if the light has one.
    pub kelvin_range: Option<(i32, i32)>,
    pub action: String,
    pub service: String,
    pub icon: String,
    pub cover_state: String,
    pub position: Option<i32>,
}

impl Light {
    /// Builds a light from a Home Assistant state object (`get_states` / `state_changed`).
    #[must_use]
    // brightness is 0-255 from HA; the percentage is clamped to 0-100 before the cast.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn from_state(key: u8, entity_id: &str, name: &str, state: &Value) -> Self {
        let attr = &state["attributes"];
        // Some installations send these as floats (e.g. `2700.0`).
        let int = |key: &str| {
            attr[key]
                .as_f64()
                .filter(|v| v.is_finite())
                .map(|v| v.round() as i32)
        };
        let s = state["state"].as_str().unwrap_or("unavailable");
        let on = s == "on";
        Self {
            key,
            entity_id: entity_id.to_owned(),
            name: name.to_owned(),
            available: s != "unavailable" && s != "unknown",
            on,
            brightness_pct: attr["brightness"]
                .as_f64()
                .filter(|_| on)
                // A light that is on shows at least 1 % (HA's minimum brightness 1/255 rounds to 0).
                .map(|b| (b / 255.0 * 100.0).round().clamp(1.0, 100.0) as u8),
            kelvin: int("color_temp_kelvin").filter(|_| on),
            kelvin_range: int("min_color_temp_kelvin").zip(int("max_color_temp_kelvin")),
            action: String::new(),
            service: String::new(),
            icon: String::new(),
            cover_state: s.to_owned(),
            position: int("current_position").map(|p| p.clamp(0, 100)),
        }
    }

    /// The entity's domain, e.g. `light`, `switch`, `scene`.
    #[must_use]
    pub fn domain(&self) -> &str {
        self.entity_id.split('.').next().unwrap_or("")
    }

    fn icon_name(&self) -> &str {
        if !self.icon.is_empty() {
            return &self.icon;
        }
        match (self.domain(), self.action.as_str()) {
            ("cover", "open") => "blinds-up",
            ("cover", "close") => "blinds-down",
            ("cover", "stop") => "blinds-stop",
            ("cover", _) => "blinds",
            ("light", _) if self.on => "light-on",
            ("light", _) => "light-off",
            ("switch" | "input_boolean", _) => "plug",
            ("scene", _) => "scene",
            _ => "",
        }
    }

    fn is_light(&self) -> bool {
        self.domain() == "light"
    }

    /// Scenes, scripts and buttons are triggered, not switched: they have no on/off state.
    fn is_momentary(&self) -> bool {
        matches!(self.domain(), "scene" | "script" | "button" | "input_button")
    }

    /// The call a key press makes.
    fn press(&self) -> Call {
        let id = vec![self.entity_id.clone()];
        let explicit = if self.domain() == "cover"
            && matches!(self.cover_state.as_str(), "opening" | "closing")
            && matches!(self.action.as_str(), "open" | "close")
        {
            Some("cover.stop_cover")
        } else {
            match self.action.as_str() {
                "open" => Some("cover.open_cover"),
                "close" => Some("cover.close_cover"),
                "stop" => Some("cover.stop_cover"),
                _ => (!self.service.is_empty()).then_some(self.service.as_str()),
            }
        };
        if let Some((domain, service)) = explicit.and_then(|s| s.split_once('.')) {
            return Call {
                domain: domain.to_owned(),
                service: service.to_owned(),
                entities: id,
                data: json!({}),
            };
        }
        match self.domain() {
            "light" => call("toggle", id, json!({})),
            "scene" | "script" => Call {
                domain: self.domain().to_owned(),
                service: "turn_on".into(),
                entities: id,
                data: json!({}),
            },
            "button" | "input_button" => Call {
                domain: self.domain().to_owned(),
                service: "press".into(),
                entities: id,
                data: json!({}),
            },
            _ => Call {
                domain: "homeassistant".to_owned(),
                service: "toggle".into(),
                entities: id,
                data: json!({}),
            },
        }
    }
}

/// A Home Assistant service call: `<domain>.<service>` on `entities` with `data`.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub domain: String,
    pub service: String,
    pub entities: Vec<String>,
    pub data: Value,
}

/// The entities shown on the keys, and what the knobs do.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::struct_field_names,
    reason = "H15-97: `lights` is the list every caller uses; the new fields tipped the lint"
)]
pub struct Lights {
    pub lights: Vec<Light>,
    pub knobs: [KnobFn; 2],
    /// Keys showing brightness / colour temperature bars (H15-97).
    pub levels: Vec<LevelKey>,
    pub preview: Option<Preview>,
}

impl Default for Lights {
    fn default() -> Self {
        Self {
            lights: Vec::new(),
            knobs: [KnobFn::Brightness, KnobFn::ColorTemp],
            levels: Vec::new(),
            preview: None,
        }
    }
}

impl Lights {
    /// Updates every pin for an entity from `state_changed`; returns whether any matched.
    pub fn update(&mut self, entity_id: &str, state: &Value) -> bool {
        let mut found = false;
        for light in self.lights.iter_mut().filter(|l| l.entity_id == entity_id) {
            let mut updated = Light::from_state(light.key, entity_id, &light.name, state);
            updated.action.clone_from(&light.action);
            updated.service.clone_from(&light.service);
            updated.icon.clone_from(&light.icon);
            *light = updated;
            found = true;
        }
        found
    }

    /// The lights that count for the levels: lights that are on.
    fn lit(&self) -> impl Iterator<Item = &Light> {
        self.lights.iter().filter(|l| l.is_light() && l.available && l.on)
    }

    /// Brightness of the lights that are on (their mean), 0 when all are off; a knob turn's own
    /// value while it is held (H15-97).
    #[must_use]
    pub fn brightness(&self, now: std::time::Instant) -> i32 {
        if let Some(b) = self.active_preview(now).and_then(|p| p.brightness) {
            return b;
        }
        let values: Vec<i32> = self
            .lit()
            .filter_map(|l| l.brightness_pct.map(i32::from))
            .collect();
        if values.is_empty() {
            return if self.lit().next().is_some() { 100 } else { 0 };
        }
        values.iter().sum::<i32>() / i32::try_from(values.len()).unwrap_or(1)
    }

    /// Colour temperature (kelvin) and the range it moves in: of the lights that are on, else of
    /// any light that has one; `None` without colour temperature.
    #[must_use]
    pub fn kelvin(&self, now: std::time::Instant) -> Option<(i32, (i32, i32))> {
        let source = self
            .lit()
            .find(|l| l.kelvin_range.is_some())
            .or_else(|| self.lights.iter().find(|l| l.kelvin_range.is_some()))?;
        let range = source.kelvin_range?;
        let value = self
            .active_preview(now)
            .and_then(|p| p.kelvin)
            .or(source.kelvin)
            .unwrap_or(i32::midpoint(range.0, range.1));
        Some((value.clamp(range.0, range.1), range))
    }

    fn active_preview(&self, now: std::time::Instant) -> Option<&Preview> {
        self.preview.as_ref().filter(|p| p.until > now)
    }

    /// Shows what a knob turn asks for at once (optimistic, H15-97): the batched call follows, and
    /// Home Assistant's state takes over again [`PREVIEW_HOLD`] after the last turn. Colour
    /// temperature only while a light is on; brightness also from off (see [`Lights::dim`]).
    pub fn preview_turn(&mut self, knob: KnobFn, detents: i32, now: std::time::Instant) {
        let lit = self.lit().next().is_some();
        if detents == 0 || (!lit && (knob != KnobFn::Brightness || detents < 0)) {
            return;
        }
        let mut preview = self.active_preview(now).copied().unwrap_or(Preview {
            brightness: None,
            kelvin: None,
            until: now,
        });
        match knob {
            KnobFn::Brightness => {
                let current = self.brightness(now);
                preview.brightness = Some(if current == 0 {
                    from_off(detents)
                } else {
                    (current + detents * BRIGHTNESS_STEP_PCT).clamp(0, 100)
                });
            }
            KnobFn::ColorTemp => {
                let Some((current, (lo, hi))) = self.kelvin(now) else {
                    return;
                };
                preview.kelvin = Some((current + detents * KELVIN_STEP).clamp(lo, hi));
            }
            KnobFn::None => return,
        }
        preview.until = now + PREVIEW_HOLD;
        self.preview = Some(preview);
    }

    /// When the page has to be drawn again because a preview ends.
    #[must_use]
    pub fn preview_ends(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.active_preview(now).map(|p| p.until)
    }

    /// The keys and knob roles for the current state.
    #[must_use]
    pub fn page(&self) -> Page {
        self.page_at(std::time::Instant::now())
    }

    /// [`Lights::page`] as of `now` (a knob turn's preview may be showing).
    #[must_use]
    pub fn page_at(&self, now: std::time::Instant) -> Page {
        let mut faces: Vec<KeyFace> = self
            .lights
            .iter()
            .filter(|light| !self.levels.iter().any(|l| l.key == light.key))
            .map(|light| {
                let (detail, state) = match (light.available, light.on) {
                    (false, _) => ("unavailable".to_owned(), FaceState::FACE_STATE_UNAVAILABLE),
                    (true, _) if light.domain() == "cover" => {
                        let detail = light.position.map_or_else(
                            || light.cover_state.clone(),
                            |p| format!("{p} % · {}", light.cover_state),
                        );
                        let moving = matches!(
                            (light.action.as_str(), light.cover_state.as_str()),
                            ("open", "opening") | ("close", "closing")
                        ) || (light.action.is_empty()
                            && matches!(light.cover_state.as_str(), "open" | "opening" | "closing"));
                        (
                            detail,
                            if moving {
                                FaceState::FACE_STATE_ON
                            } else {
                                FaceState::FACE_STATE_IDLE
                            },
                        )
                    }
                    (true, _) if light.is_momentary() => {
                        (light.domain().to_owned(), FaceState::FACE_STATE_IDLE)
                    }
                    (true, true) => (
                        light
                            .brightness_pct
                            .map_or_else(|| "on".to_owned(), |p| format!("{p} %")),
                        FaceState::FACE_STATE_ON,
                    ),
                    (true, false) => ("off".to_owned(), FaceState::FACE_STATE_IDLE),
                };
                let mut face = face(light.key, light.name.clone(), detail, state);
                light.icon_name().clone_into(&mut face.image);
                self.brightness_strip(light, &mut face, now);
                face
            })
            .collect();
        faces.extend(self.levels.iter().map(|level| self.level_face(level, now)));
        Page {
            now_playing: None,
            faces,
            knobs: (1u8..)
                .zip(self.knobs)
                .filter_map(|(n, f)| f.label().map(|l| knob(n, l)))
                .collect(),
        }
    }

    /// On a tab with level keys, a light that is on shows its brightness as a strip along the
    /// bottom of its key, following a knob turn at once (H15-97).
    fn brightness_strip(&self, light: &Light, face: &mut KeyFace, now: std::time::Instant) {
        if self.levels.is_empty() || !(light.available && light.on && light.is_light()) {
            return;
        }
        let preview = self.active_preview(now).and_then(|p| p.brightness);
        let Some(pct) = preview.or_else(|| light.brightness_pct.map(i32::from)) else {
            return;
        };
        if preview.is_some() {
            face.detail = format!("{pct} %");
        }
        face.level = u32::try_from(pct).ok();
        face.level_style = LevelStyle::LEVEL_STYLE_STRIP.into();
    }

    /// A level key: the brightness or colour temperature of the lights that are on, as a bar
    /// (H15-97). With every light off: brightness 0 %, colour temperature "off".
    fn level_face(&self, level: &LevelKey, now: std::time::Instant) -> KeyFace {
        let lit = self.lit().next().is_some() || self.active_preview(now).is_some();
        let state = if lit {
            FaceState::FACE_STATE_ON
        } else {
            FaceState::FACE_STATE_IDLE
        };
        let mut out = face(level.key, level.label.clone(), String::new(), state);
        match level.show {
            LevelShow::Brightness => out.level = u32::try_from(self.brightness(now)).ok(),
            LevelShow::ColorTemp => match self.kelvin(now).filter(|_| lit) {
                Some((kelvin, (lo, hi))) => {
                    let span = (hi - lo).max(1);
                    out.level = u32::try_from((kelvin - lo) * 100 / span).ok();
                    out.detail = format!("{kelvin} K");
                    out.level_style = LevelStyle::LEVEL_STYLE_TEMPERATURE.into();
                }
                None => "off".clone_into(&mut out.detail),
            },
        }
        out
    }

    /// What knob `n` (1 or 2) does.
    #[must_use]
    pub fn knob(&self, n: u8) -> KnobFn {
        match n {
            1 => self.knobs[0],
            2 => self.knobs[1],
            _ => KnobFn::None,
        }
    }

    /// The service calls for a key press or knob press. Knob turns go through [`Lights::dim`] and
    /// [`Lights::warm`] after batching.
    #[must_use]
    pub fn on_input(&self, input: Input) -> Vec<Call> {
        match input {
            Input::Key { key, pressed: true } => self
                .lights
                .iter()
                .find(|l| l.key == key)
                .filter(|l| l.available)
                .map(|l| vec![l.press()])
                .unwrap_or_default(),
            Input::KnobPress { knob, pressed: true } if self.knob(knob) != KnobFn::None => {
                let all: Vec<_> = self
                    .lights
                    .iter()
                    .filter(|l| l.available && l.is_light())
                    .map(|l| l.entity_id.clone())
                    .collect();
                if all.is_empty() {
                    return Vec::new();
                }
                let service = if self.lights.iter().any(|l| l.on && l.is_light()) {
                    "turn_off"
                } else {
                    "turn_on"
                };
                vec![call(service, all, json!({}))]
            }
            _ => Vec::new(),
        }
    }

    /// Brightness change for `detents` of the brightness knob, applied to the lights that are on.
    /// With every light off, turning right switches them on from the bottom (1 % for the first
    /// detent, H15-97); turning down to 0 switches them off. The state is updated at once, so the
    /// next batch builds on it before Home Assistant reports back. Colour temperature turns never
    /// switch lights on.
    pub fn dim(&mut self, detents: i32) -> Option<Call> {
        if detents == 0 {
            return None;
        }
        let on = self.on_lights(|_| true);
        if on.is_empty() {
            let all = self.on_lights_or_off();
            if detents < 0 || all.is_empty() {
                return None;
            }
            let target = from_off(detents);
            self.assume(&all, Some(target));
            return Some(call("turn_on", all, json!({ "brightness_pct": target })));
        }
        let brightest = self
            .lights
            .iter()
            .filter(|l| on.contains(&l.entity_id))
            .map(|l| l.brightness_pct.map_or(100, i32::from))
            .max()
            .unwrap_or(100);
        if brightest + detents * BRIGHTNESS_STEP_PCT <= 0 {
            self.assume(&on, None);
            return Some(call("turn_off", on, json!({})));
        }
        Some(call(
            "turn_on",
            on,
            json!({ "brightness_step_pct": detents * BRIGHTNESS_STEP_PCT }),
        ))
    }

    /// The available lights (not covers), on or off.
    fn on_lights_or_off(&self) -> Vec<String> {
        self.lights
            .iter()
            .filter(|l| l.available && l.is_light())
            .map(|l| l.entity_id.clone())
            .collect()
    }

    /// Sets the state a call asks for before Home Assistant confirms it: on at `brightness`, or off.
    fn assume(&mut self, entities: &[String], brightness: Option<i32>) {
        for light in self.lights.iter_mut().filter(|l| entities.contains(&l.entity_id)) {
            light.on = brightness.is_some();
            light.brightness_pct = brightness.and_then(|b| u8::try_from(b).ok());
        }
    }

    /// Color temperature change for `detents` of knob 2 (positive = cooler), one call per light
    /// that is on and supports it, kept within each light's range.
    #[must_use]
    pub fn warm(&self, detents: i32) -> Vec<Call> {
        if detents == 0 {
            return Vec::new();
        }
        self.lights
            .iter()
            .filter(|l| l.available && l.on && l.is_light())
            .filter_map(|l| {
                let (min, max) = l.kelvin_range?;
                let current = l.kelvin.unwrap_or(i32::midpoint(min, max));
                let target = (current + detents * KELVIN_STEP).clamp(min, max);
                (target != current).then(|| {
                    call(
                        "turn_on",
                        vec![l.entity_id.clone()],
                        json!({ "color_temp_kelvin": target }),
                    )
                })
            })
            .collect()
    }

    fn on_lights(&self, filter: impl Fn(&Light) -> bool) -> Vec<String> {
        self.lights
            .iter()
            .filter(|l| l.available && l.on && l.is_light() && filter(l))
            .map(|l| l.entity_id.clone())
            .collect()
    }
}

/// Brightness after `detents` (> 0) from off: 1 % (Home Assistant's minimum), then the usual steps.
fn from_off(detents: i32) -> i32 {
    (1 + (detents - 1) * BRIGHTNESS_STEP_PCT).clamp(1, 100)
}

/// Drops the leading words a light's name shares with its area ("OG Buero Deckenspots" in area
/// "OG Buero Nord" → "Deckenspots"); keys are small. Keeps the full name if nothing would remain.
#[must_use]
pub fn short_name(name: &str, area: &str) -> String {
    let area_words: Vec<_> = area.split_whitespace().collect();
    let words: Vec<_> = name.split_whitespace().collect();
    let shared = words
        .iter()
        .zip(&area_words)
        .take_while(|(w, a)| same_name(w, a))
        .count();
    if shared == 0 || shared >= words.len() {
        name.to_owned()
    } else {
        words[shared..].join(" ")
    }
}

/// A `light.<service>` call.
fn call(service: &'static str, entities: Vec<String>, data: Value) -> Call {
    Call {
        domain: "light".to_owned(),
        service: service.to_owned(),
        entities,
        data,
    }
}

/// Compares area names loosely: case, umlauts (ü = ue), spaces, `_` and `-` don't matter, so
/// `gaeste buero` matches "Gäste Büro".
#[must_use]
pub fn same_name(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        s.to_lowercase()
            .replace('ä', "ae")
            .replace('ö', "oe")
            .replace('ü', "ue")
            .replace('ß', "ss")
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect()
    }
    norm(a) == norm(b)
}

/// Picks the lights of an area from the registries and states (`config/area_registry/list`,
/// `config/device_registry/list`, `config/entity_registry/list`, `get_states`).
///
/// A light belongs to the area if its entity is assigned to it, or if the entity has no area and
/// its device is in the area. Disabled and hidden entities are skipped. Sorted by name, at most
/// [`MAX_LIGHTS`].
pub fn lights_in_area(
    area: &str,
    areas: &Value,
    devices: &Value,
    entities: &Value,
    states: &Value,
) -> Result<Lights, String> {
    let list = |v: &Value| v.as_array().cloned().unwrap_or_default();
    let area_entry = list(areas)
        .into_iter()
        .find(|a| same_name(a["name"].as_str().unwrap_or(""), area) || a["area_id"].as_str() == Some(area));
    let area_name = area_entry
        .as_ref()
        .and_then(|a| a["name"].as_str())
        .unwrap_or(area)
        .to_owned();
    let area_id = area_entry
        .as_ref()
        .and_then(|a| a["area_id"].as_str().map(str::to_owned))
        .ok_or_else(|| {
            let names: Vec<_> = list(areas)
                .iter()
                .filter_map(|a| a["name"].as_str().map(str::to_owned))
                .collect();
            format!("no area {area:?} in Home Assistant (areas: {})", names.join(", "))
        })?;
    // Maps, not searches: a large installation has thousands of devices and states.
    let device_areas: std::collections::HashMap<&str, &str> = devices
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| Some((d["id"].as_str()?, d["area_id"].as_str()?)))
        .collect();
    let state_of: std::collections::HashMap<&str, &Value> = states
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| Some((s["entity_id"].as_str()?, s)))
        .collect();
    let device_area = |device_id: &str| device_areas.get(device_id).map(|a| (*a).to_owned());
    let mut lights: Vec<Light> = entities
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["entity_id"].as_str().is_some_and(|id| id.starts_with("light.")))
        .filter(|e| e["disabled_by"].is_null() && e["hidden_by"].is_null())
        .filter(|e| {
            let area = e["area_id"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| e["device_id"].as_str().and_then(device_area));
            area.as_deref() == Some(area_id.as_str())
        })
        .filter_map(|e| {
            let id = e["entity_id"].as_str()?;
            let state = (*state_of.get(id)?).clone();
            let name = e["name"]
                .as_str()
                .or_else(|| state["attributes"]["friendly_name"].as_str())
                .or_else(|| e["original_name"].as_str())
                .unwrap_or(id);
            Some(Light::from_state(0, id, &short_name(name, &area_name), &state))
        })
        .collect();
    lights.sort_by_key(|l| l.name.to_lowercase());
    lights.truncate(MAX_LIGHTS);
    for (light, key) in lights.iter_mut().zip(1u8..) {
        light.key = key;
    }
    Ok(Lights {
        lights,
        ..Lights::default()
    })
}

/// The entities pinned to keys, named by their label, the registry name or the friendly name.
/// An entity Home Assistant doesn't know is shown as unavailable.
#[must_use]
pub fn pinned(pins: &[Pin], entities: &Value, states: &Value) -> Lights {
    let find = |list: &Value, id: &str| {
        list.as_array()
            .and_then(|l| l.iter().find(|e| e["entity_id"].as_str() == Some(id)))
            .cloned()
    };
    let lights = pins
        .iter()
        .map(|pin| {
            let state = find(states, &pin.entity).unwrap_or_else(|| json!({ "state": "unavailable" }));
            let entry = find(entities, &pin.entity).unwrap_or(Value::Null);
            let name = pin
                .label
                .clone()
                .or_else(|| entry["name"].as_str().map(str::to_owned))
                .or_else(|| state["attributes"]["friendly_name"].as_str().map(str::to_owned))
                .or_else(|| entry["original_name"].as_str().map(str::to_owned))
                .unwrap_or_else(|| pin.entity.clone());
            let mut light = Light::from_state(pin.key, &pin.entity, &name, &state);
            light.action.clone_from(&pin.action);
            light.service.clone_from(&pin.service);
            light.icon.clone_from(&pin.icon);
            light
        })
        .collect();
    Lights {
        lights,
        ..Lights::default()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Registries and states of a small Home Assistant with two areas.
    pub fn fixture() -> (Value, Value, Value, Value) {
        let areas = json!([{ "area_id": "buero", "name": "Gäste Büro" }, { "area_id": "kitchen", "name": "Kitchen" }]);
        let devices =
            json!([{ "id": "dev-strip", "area_id": "buero" }, { "id": "dev-k", "area_id": "kitchen" }]);
        let entities = json!([
            { "entity_id": "light.desk", "area_id": "buero", "device_id": null, "disabled_by": null, "hidden_by": null, "name": "Desk" },
            { "entity_id": "light.strip", "area_id": null, "device_id": "dev-strip", "disabled_by": null, "hidden_by": null, "name": null, "original_name": "Strip" },
            { "entity_id": "light.ceiling", "area_id": "buero", "device_id": null, "disabled_by": null, "hidden_by": null, "name": "Ceiling" },
            { "entity_id": "light.old", "area_id": "buero", "device_id": null, "disabled_by": "user", "hidden_by": null, "name": "Old" },
            { "entity_id": "light.kitchen", "area_id": "kitchen", "device_id": null, "disabled_by": null, "hidden_by": null, "name": "Kitchen" },
            { "entity_id": "switch.fan", "area_id": "buero", "device_id": null, "disabled_by": null, "hidden_by": null, "name": "Fan" }
        ]);
        let states = json!([
            { "entity_id": "light.desk", "state": "on", "attributes": { "friendly_name": "Büro Desk", "brightness": 153, "color_temp_kelvin": 3000, "min_color_temp_kelvin": 2200, "max_color_temp_kelvin": 6500 } },
            { "entity_id": "light.strip", "state": "off", "attributes": { "friendly_name": "Strip" } },
            { "entity_id": "light.ceiling", "state": "unavailable", "attributes": { "friendly_name": "Ceiling" } },
            { "entity_id": "light.old", "state": "off", "attributes": {} },
            { "entity_id": "light.kitchen", "state": "on", "attributes": {} },
            { "entity_id": "switch.fan", "state": "on", "attributes": {} }
        ]);
        (areas, devices, entities, states)
    }

    fn lights() -> Lights {
        let (a, d, e, s) = fixture();
        lights_in_area("gaeste buero", &a, &d, &e, &s).unwrap()
    }

    #[test]
    fn default_icons_follow_entity_domain_and_live_state() {
        let mut lights = lights();
        assert_eq!(lights.page().faces[1].image, "light-on");
        assert_eq!(lights.page().faces[2].image, "light-off");
        lights.update("light.desk", &json!({"state":"off"}));
        assert_eq!(lights.page().faces[1].image, "light-off");
        assert_eq!(lights.page().faces[1].state, FaceState::FACE_STATE_IDLE);
        let other = Lights {
            lights: vec![
                Light::from_state(
                    1,
                    "cover.blind",
                    "Blind",
                    &json!({"state":"open","attributes":{"current_position":65}}),
                ),
                Light::from_state(2, "switch.socket", "Socket", &json!({"state":"on"})),
                Light::from_state(3, "scene.movie", "Movie", &json!({"state":"2026-10-19"})),
            ],
            ..Lights::default()
        };
        let page = other.page();
        assert_eq!(
            page.faces.iter().map(|f| f.image.as_str()).collect::<Vec<_>>(),
            ["blinds", "plug", "scene"]
        );
        assert_eq!(page.faces[0].detail, "65 % · open");
        assert_eq!(page.faces[0].state, FaceState::FACE_STATE_ON);
    }

    #[test]
    fn explicit_pin_icon_wins_and_survives_state_updates() {
        let table = toml::from_str(
            r#"url="https://ha"
            keys=[{key=4,entity="cover.test",action="open",icon="home"}]"#,
        )
        .unwrap();
        let config = super::super::Config::from_table(&table).unwrap();
        let mut lights = pinned(
            &config.pins,
            &json!([]),
            &json!([
                {"entity_id":"cover.test","state":"closed","attributes":{"current_position":0}}
            ]),
        );
        assert_eq!(lights.page().faces[0].image, "home");
        lights.update(
            "cover.test",
            &json!({"state":"opening","attributes":{"current_position":42}}),
        );
        assert_eq!(lights.page().faces[0].image, "home");
        assert_eq!(lights.page().faces[0].state, FaceState::FACE_STATE_ON);
    }

    #[test]
    fn finds_the_area_lights_by_entity_or_device_area() {
        let names: Vec<_> = lights().lights.iter().map(|l| l.name.clone()).collect();
        assert_eq!(names, ["Ceiling", "Desk", "Strip"]);
        let (a, d, e, s) = fixture();
        assert!(
            lights_in_area("Garage", &a, &d, &e, &s)
                .unwrap_err()
                .contains("Gäste Büro")
        );
    }

    #[test]
    fn levels_follow_the_lights_that_are_on_and_knob_turns_preview() {
        let mut l = lights();
        let now = std::time::Instant::now();
        assert_eq!(l.brightness(now), 60, "only Desk is on");
        assert_eq!(l.kelvin(now), Some((3000, (2200, 6500))));
        l.preview_turn(KnobFn::Brightness, 3, now);
        l.preview_turn(KnobFn::ColorTemp, -10, now);
        assert_eq!(l.brightness(now), 75);
        assert_eq!(l.kelvin(now), Some((2200, (2200, 6500))), "clamped to the range");
        l.preview_turn(KnobFn::Brightness, 20, now);
        assert_eq!(l.brightness(now), 100);
        assert_eq!(l.preview_ends(now), Some(now + PREVIEW_HOLD));
        let later = now + PREVIEW_HOLD;
        assert_eq!(l.brightness(later), 60, "Home Assistant's state again");
        assert_eq!(l.preview_ends(later), None);
        for light in &mut l.lights {
            light.on = false;
        }
        assert_eq!(l.brightness(later), 0);
        l.preview_turn(KnobFn::ColorTemp, 2, later);
        l.preview_turn(KnobFn::Brightness, -2, later);
        assert_eq!(l.preview_ends(later), None, "off: colour and down change nothing");
        l.preview_turn(KnobFn::Brightness, 1, later);
        assert_eq!(l.brightness(later), 1, "the dimmer starts at the bottom");
        l.preview_turn(KnobFn::Brightness, 2, later);
        assert_eq!(l.brightness(later), 11);
    }

    #[test]
    fn level_keys_show_brightness_and_colour_temperature_bars() {
        let mut l = lights();
        l.levels = vec![
            LevelKey {
                key: 1,
                show: LevelShow::Brightness,
                label: "Hell".into(),
            },
            LevelKey {
                key: 3,
                show: LevelShow::ColorTemp,
                label: "Farbe".into(),
            },
        ];
        let now = std::time::Instant::now();
        let by_key = |page: &Page, key: u32| page.faces.iter().find(|f| f.key == key).cloned().unwrap();
        let page = l.page_at(now);
        assert_eq!(page.faces.len(), 3, "the level keys replace Ceiling and Strip");
        let bright = by_key(&page, 1);
        assert_eq!((bright.label.as_str(), bright.level), ("Hell", Some(60)));
        let temp = by_key(&page, 3);
        assert_eq!(
            (temp.detail.as_str(), temp.level),
            ("3000 K", Some(18)),
            "(3000-2200)/4300"
        );
        assert_eq!(
            temp.level_style.as_known(),
            Some(LevelStyle::LEVEL_STYLE_TEMPERATURE)
        );
        let desk = by_key(&page, 2);
        assert_eq!(
            (desk.level, desk.level_style.as_known()),
            (Some(60), Some(LevelStyle::LEVEL_STYLE_STRIP))
        );

        // A knob turn shows at once, on the bar and on the light's own key.
        l.preview_turn(KnobFn::Brightness, 2, now);
        let page = l.page_at(now);
        assert_eq!(by_key(&page, 1).level, Some(70));
        assert_eq!(
            (by_key(&page, 2).level, by_key(&page, 2).detail.as_str()),
            (Some(70), "70 %")
        );

        for light in &mut l.lights {
            light.on = false;
        }
        let page = l.page_at(now + PREVIEW_HOLD);
        assert_eq!(by_key(&page, 1).level, Some(0), "off: the bar starts at 0 %");
        assert_eq!(
            (by_key(&page, 3).level, by_key(&page, 3).detail.as_str()),
            (None, "off")
        );
        assert_eq!(by_key(&page, 2).level, None, "no strip while off");
    }

    #[test]
    fn dimmer_switches_on_from_the_bottom_and_off_at_zero() {
        let mut l = lights();
        for light in &mut l.lights {
            light.on = false;
        }
        assert_eq!(l.dim(-1), None);
        let on = l.dim(1).unwrap();
        assert_eq!(
            (on.service.as_str(), on.entities.clone()),
            ("turn_on", vec!["light.desk".to_owned(), "light.strip".to_owned()])
        );
        assert_eq!(on.data, json!({ "brightness_pct": 1 }));
        let up = l.dim(2).unwrap();
        assert_eq!(
            up.data,
            json!({ "brightness_step_pct": 10 }),
            "builds on the assumed state"
        );
        assert_eq!(l.dim(-3).unwrap().service, "turn_off", "1 % + 10 - 15 reaches 0");
        assert_eq!(l.dim(-1), None, "already off");
        assert!(l.warm(5).is_empty(), "colour temperature never switches on");
    }

    #[test]
    fn faces_show_state_and_brightness() {
        let page = lights().page();
        let f: Vec<_> = page
            .faces
            .iter()
            .map(|f| (f.label.as_str(), f.detail.as_str(), f.state.as_known().unwrap()))
            .collect();
        assert_eq!(
            f,
            [
                ("Ceiling", "unavailable", FaceState::FACE_STATE_UNAVAILABLE),
                ("Desk", "60 %", FaceState::FACE_STATE_ON),
                ("Strip", "off", FaceState::FACE_STATE_IDLE),
            ]
        );
        assert_eq!(page.knobs.len(), 2);
    }

    #[test]
    fn key_press_toggles_and_knob_2_press_switches_all() {
        let l = lights();
        assert_eq!(
            l.on_input(Input::Key {
                key: 3,
                pressed: true
            }),
            [call("toggle", vec!["light.strip".into()], json!({}))]
        );
        assert!(
            l.on_input(Input::Key {
                key: 1,
                pressed: true
            })
            .is_empty(),
            "unavailable light"
        );
        assert!(
            l.on_input(Input::Key {
                key: 3,
                pressed: false
            })
            .is_empty(),
            "release does nothing"
        );
        // One light is on: knob 2 press turns all off.
        let calls = l.on_input(Input::KnobPress {
            knob: 2,
            pressed: true,
        });
        assert_eq!(calls[0].service, "turn_off");
        assert_eq!(calls[0].entities, ["light.desk", "light.strip"]);
    }

    #[test]
    fn knobs_change_the_lights_that_are_on() {
        let mut l = lights();
        assert_eq!(
            l.dim(-3),
            Some(call(
                "turn_on",
                vec!["light.desk".into()],
                json!({ "brightness_step_pct": -15 })
            ))
        );
        assert_eq!(
            l.warm(2),
            [call(
                "turn_on",
                vec!["light.desk".into()],
                json!({ "color_temp_kelvin": 3200 })
            )]
        );
        assert_eq!(
            l.warm(-100)[0].data,
            json!({ "color_temp_kelvin": 2200 }),
            "clamped to the light's range"
        );
    }

    #[test]
    fn float_attributes_from_home_assistant() {
        let light = Light::from_state(
            1,
            "light.spots",
            "Spots",
            &json!({ "state": "on", "attributes": { "brightness": 127.5, "color_temp_kelvin": 3000.0, "min_color_temp_kelvin": 2700.0, "max_color_temp_kelvin": 6000.0 } }),
        );
        assert_eq!(light.kelvin, Some(3000));
        assert_eq!(light.kelvin_range, Some((2700, 6000)));
        assert_eq!(light.brightness_pct, Some(50));
        let l = Lights {
            lights: vec![light],
            ..Lights::default()
        };
        assert_eq!(l.warm(1)[0].data, json!({ "color_temp_kelvin": 3100 }));
    }

    #[test]
    fn a_light_that_is_on_shows_at_least_one_percent() {
        let light = Light::from_state(
            1,
            "light.x",
            "X",
            &json!({ "state": "on", "attributes": { "brightness": 1 } }),
        );
        assert_eq!(light.brightness_pct, Some(1));
    }

    #[test]
    fn state_changed_updates_one_light() {
        let mut l = lights();
        let changed = l.update(
            "light.strip",
            &json!({ "state": "on", "attributes": { "brightness": 255 } }),
        );
        assert!(changed);
        assert_eq!(l.lights[2].brightness_pct, Some(100));
        assert_eq!(l.lights[2].name, "Strip", "keeps the registry name");
        assert!(!l.update("light.kitchen", &json!({ "state": "off" })));
    }

    #[test]
    fn light_names_drop_the_area_prefix() {
        assert_eq!(short_name("OG Buero Deckenspots", "OG Buero Nord"), "Deckenspots");
        assert_eq!(
            short_name("Büro Desk", "Gäste Büro"),
            "Büro Desk",
            "only a leading prefix"
        );
        assert_eq!(short_name("OG Buero", "OG Buero Nord"), "OG Buero", "never empty");
        assert_eq!(short_name("Desk", "Gäste Büro"), "Desk");
    }

    #[test]
    fn pinned_entities_of_any_domain_on_chosen_keys() {
        let (_, _, e, mut s) = fixture();
        s.as_array_mut()
            .unwrap()
            .push(json!({ "entity_id": "scene.movie", "state": "2026-10-18T10:00:00", "attributes": { "friendly_name": "Movie" } }));
        let pins = [
            Pin {
                key: 2,
                entity: "switch.fan".into(),
                label: None,
                ..Pin::default()
            },
            Pin {
                key: 4,
                entity: "scene.movie".into(),
                label: Some("Kino".into()),
                ..Pin::default()
            },
            Pin {
                key: 6,
                entity: "light.desk".into(),
                label: None,
                ..Pin::default()
            },
            Pin {
                key: 5,
                entity: "light.gone".into(),
                label: None,
                ..Pin::default()
            },
        ];
        let mut l = pinned(&pins, &e, &s);
        l.knobs = [KnobFn::None, KnobFn::Brightness];
        let page = l.page();
        let faces: Vec<_> = page
            .faces
            .iter()
            .map(|f| (f.key, f.label.as_str(), f.detail.as_str()))
            .collect();
        assert_eq!(
            faces,
            [
                (2, "Fan", "on"),
                (4, "Kino", "scene"),
                (6, "Desk", "60 %"),
                (5, "light.gone", "unavailable")
            ]
        );
        assert_eq!(page.knobs.iter().map(|k| k.knob).collect::<Vec<_>>(), [2]);
        let press = |key| l.on_input(Input::Key { key, pressed: true });
        assert_eq!(
            (press(2)[0].domain.as_str(), press(2)[0].service.as_str()),
            ("homeassistant", "toggle")
        );
        assert_eq!(
            (press(4)[0].domain.as_str(), press(4)[0].service.as_str()),
            ("scene", "turn_on")
        );
        assert_eq!(
            (press(6)[0].domain.as_str(), press(6)[0].service.as_str()),
            ("light", "toggle")
        );
        assert!(press(1).is_empty() && press(5).is_empty());
        // Knob 1 does nothing; knob 2 presses switch the lights only (not the fan).
        assert_eq!(
            l.on_input(Input::KnobPress {
                knob: 1,
                pressed: true
            }),
            []
        );
        let all = l.on_input(Input::KnobPress {
            knob: 2,
            pressed: true,
        });
        assert_eq!(all[0].entities, ["light.desk"]);
        assert_eq!(l.dim(1).unwrap().entities, ["light.desk"]);
    }

    #[test]
    fn area_names_match_loosely() {
        assert!(same_name("gaeste buero", "Gäste Büro"));
        assert!(same_name("gaeste_buero", "Gäste-Büro"));
        assert!(!same_name("gaeste", "Gäste Büro"));
    }
}

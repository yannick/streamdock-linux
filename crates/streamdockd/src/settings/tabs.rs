//! Tabs on the dashboard (H15-32): add, rename, delete and reorder modes, and choose what the keys
//! and knobs of a lights tab do. Every change edits `config.toml` with `toml_edit` (comments
//! stay), is checked like a startup and then applied by hot reload.
//!
//! Where a mode lives in the config:
//!
//! | kind   | first tab                         | more tabs                        |
//! |--------|-----------------------------------|----------------------------------|
//! | lights | `[modules.homeassistant]` (area/keys) | `[[modules.homeassistant.tab]]` |
//! | music  | `[modules.lyrion]` (player)       | `[[modules.lyrion.tab]]`         |
//! | keys   | `[[modules.keys.page]]`           | (same)                           |
//!
//! Tabs that rely on a default id ("homeassistant-2", "keys-3" …) get it written out before a tab
//! is added or removed, so ids, and with them profiles and `mode_order`, never shift.

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use super::{SettingsError, Store, write_atomically};
use crate::modules::homeassistant::{KnobFn, Pin};
use crate::{config, modules};

/// Where a mode's settings are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// The top-level table of `[modules.<module>]`.
    Top(&'static str),
    /// `[[modules.<module>.<list>]]` entry `i`.
    Entry(&'static str, &'static str, usize),
}

fn invalid(m: impl Into<String>) -> SettingsError {
    SettingsError::Invalid(m.into())
}

fn module_of(kind: &str) -> Result<(&'static str, &'static str), SettingsError> {
    match kind {
        "lights" => Ok(("homeassistant", "tab")),
        "music" => Ok(("lyrion", "tab")),
        "keys" => Ok(("keys", "page")),
        other => Err(invalid(format!(
            "unknown tab kind {other:?} (lights, music, keys)"
        ))),
    }
}

/// The default id of entry `i` of a list (as the modules assign them).
fn default_entry_id(module: &str, list: &str, i: usize) -> String {
    match (module, list) {
        ("keys", _) if i == 0 => "keys".into(),
        ("keys", _) => format!("keys-{}", i + 1),
        (m, _) => format!("{m}-{}", i + 2),
    }
}

// Lookups go through `Table::get{,_mut}`: `Item::get_mut` would create missing tables.

fn top<'a>(doc: &'a DocumentMut, module: &str) -> Option<&'a Table> {
    doc.as_table().get("modules")?.as_table()?.get(module)?.as_table()
}

fn top_mut<'a>(doc: &'a mut DocumentMut, module: &str) -> Option<&'a mut Table> {
    doc.as_table_mut()
        .get_mut("modules")?
        .as_table_mut()?
        .get_mut(module)?
        .as_table_mut()
}

fn entry_list<'a>(doc: &'a DocumentMut, module: &str, list: &str) -> Option<&'a ArrayOfTables> {
    top(doc, module)?.get(list)?.as_array_of_tables()
}

fn entry_list_mut<'a>(doc: &'a mut DocumentMut, module: &str, list: &str) -> Option<&'a mut ArrayOfTables> {
    top_mut(doc, module)?.get_mut(list)?.as_array_of_tables_mut()
}

fn str_of(t: &Table, key: &str) -> Option<String> {
    t.get(key).and_then(Item::as_str).map(str::to_owned)
}

/// Every editable mode: `(id, place)`, in config order.
fn modes(doc: &DocumentMut) -> Vec<(String, Place)> {
    let mut out = Vec::new();
    if let Some(t) = top(doc, "homeassistant")
        && (t.contains_key("area") || t.contains_key("keys"))
    {
        out.push((
            str_of(t, "id").unwrap_or_else(|| "homeassistant".into()),
            Place::Top("homeassistant"),
        ));
    }
    if let Some(t) = top(doc, "lyrion")
        && t.contains_key("player")
        && t.get("show").and_then(Item::as_bool) != Some(false)
    {
        out.push((
            str_of(t, "id").unwrap_or_else(|| "lyrion".into()),
            Place::Top("lyrion"),
        ));
    }
    for (module, list) in [("homeassistant", "tab"), ("lyrion", "tab"), ("keys", "page")] {
        if let Some(entries) = entry_list(doc, module, list) {
            for (i, t) in entries.iter().enumerate() {
                let id = str_of(t, "id").unwrap_or_else(|| default_entry_id(module, list, i));
                out.push((id, Place::Entry(module, list, i)));
            }
        }
    }
    out
}

fn locate(doc: &DocumentMut, mode: &str) -> Result<Place, SettingsError> {
    modes(doc)
        .into_iter()
        .find(|(id, _)| id == mode)
        .map(|(_, p)| p)
        .ok_or_else(|| {
            if mode.starts_with("demo") {
                invalid("demo tabs can't be edited; change [modules.demo] in the settings")
            } else {
                SettingsError::NotFound(format!("no tab {mode:?}"))
            }
        })
}

fn table_mut(doc: &mut DocumentMut, place: Place) -> Option<&mut Table> {
    match place {
        Place::Top(module) => top_mut(doc, module),
        Place::Entry(module, list, i) => entry_list_mut(doc, module, list)?.get_mut(i),
    }
}

/// Writes out default ids of list entries, so removing or adding one doesn't renumber the others.
fn pin_ids(doc: &mut DocumentMut) {
    for (module, list) in [("homeassistant", "tab"), ("lyrion", "tab"), ("keys", "page")] {
        if let Some(entries) = entry_list_mut(doc, module, list) {
            for (i, t) in entries.iter_mut().enumerate() {
                if !t.contains_key("id") {
                    t.insert("id", value(default_entry_id(module, list, i)));
                }
            }
        }
    }
}

/// A new id from the title ("Living room" → "living-room"), unique among `taken`.
fn new_id(title: &str, kind: &str, taken: &[String]) -> String {
    let mut base: String = title
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'ä' => 'a',
            'ö' => 'o',
            'ü' => 'u',
            c if c.is_ascii_alphanumeric() => c,
            _ => '-',
        })
        .collect();
    base = base
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if base.is_empty() {
        kind.clone_into(&mut base);
    }
    let mut id = base.clone();
    let mut n = 2;
    while taken.contains(&id) || id.starts_with("demo") {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// Every mode id of the config, demo modes included (for unique new ids).
fn all_ids(doc: &DocumentMut) -> Vec<String> {
    let mut ids: Vec<String> = modes(doc).into_iter().map(|(id, _)| id).collect();
    if let Some(pages) = top(doc, "demo")
        .and_then(|t| t.get("pages"))
        .and_then(Item::as_integer)
    {
        ids.extend((1..=pages).map(|n| format!("demo-{n}")));
    }
    ids
}

/// The profile's mode list, if `profile` names one.
fn profile_modes<'a>(doc: &'a mut DocumentMut, profile: &str) -> Option<&'a mut toml_edit::Array> {
    if profile.is_empty() {
        return None;
    }
    doc.as_table_mut()
        .get_mut("profiles")?
        .as_array_of_tables_mut()?
        .iter_mut()
        .find(|p| p.get("name").and_then(Item::as_str) == Some(profile))?
        .get_mut("modes")?
        .as_array_mut()
}

/// Replaces `old` by `new` (or removes it, `new` = None) in `mode_order` and every profile.
fn rename_in_lists(doc: &mut DocumentMut, old: &str, new: Option<&str>) {
    let fix = |list: &mut toml_edit::Array| {
        let at = list.iter().position(|v| v.as_str() == Some(old));
        if let Some(i) = at {
            list.remove(i);
            if let Some(new) = new {
                list.insert(i, new);
            }
        }
    };
    if let Some(order) = doc
        .as_table_mut()
        .get_mut("mode_order")
        .and_then(Item::as_array_mut)
    {
        fix(order);
    }
    if let Some(profiles) = doc
        .as_table_mut()
        .get_mut("profiles")
        .and_then(Item::as_array_of_tables_mut)
    {
        for p in profiles.iter_mut() {
            if let Some(modes) = p.get_mut("modes").and_then(Item::as_array_mut) {
                fix(modes);
            }
        }
    }
}

fn ensure_table<'a>(doc: &'a mut DocumentMut, module: &str) -> &'a mut Table {
    let modules = doc
        .entry("modules")
        .or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        })
        .as_table_mut()
        .expect("modules is a table");
    modules
        .entry(module)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .expect("module is a table")
}

/// A lights tab as the dashboard shows it for editing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LightsTab {
    pub area: String,
    pub pinned: bool,
    pub keys: Vec<Pin>,
    pub knobs: [String; 2],
}

impl Store {
    /// Checks a changed config like a startup, then writes it; hot reload applies it.
    fn commit(&self, doc: &DocumentMut) -> Result<(), SettingsError> {
        let text = doc.to_string();
        let settings = config::Settings::parse(&text).map_err(|e| invalid(format!("{e:#}")))?;
        modules::from_config(&settings.modules, &self.module_context(&settings.modules))
            .map_err(|e| invalid(format!("{e:#}")))?;
        write_atomically(&self.path, text.as_bytes(), None)
    }

    /// Adds a tab; returns its mode id. `area` (lights) or `player` (music) may be empty: a lights
    /// tab then starts without keys (pin entities later), a music tab uses the default player.
    /// With `profile`, the tab joins that profile.
    pub fn add_tab(
        &self,
        kind: &str,
        title: &str,
        area: &str,
        player: &str,
        profile: &str,
    ) -> Result<String, SettingsError> {
        let (module, list) = module_of(kind)?;
        let title = title.trim();
        if title.is_empty() {
            return Err(invalid("a tab needs a name"));
        }
        let mut doc = self.read()?;
        match kind {
            "lights" if top(&doc, "homeassistant").is_none_or(|t| !t.contains_key("url")) => {
                return Err(invalid(
                    "set up Home Assistant on the settings page first (URL and token)",
                ));
            }
            "music" if top(&doc, "lyrion").is_none_or(|t| !t.contains_key("url")) => {
                return Err(invalid(
                    "set up Lyrion on the settings page first (URL and player)",
                ));
            }
            _ => {}
        }
        pin_ids(&mut doc);
        let id = new_id(title, kind, &all_ids(&doc));
        let mut entry = Table::new();
        entry.insert("id", value(&id));
        entry.insert("title", value(title));
        match kind {
            "lights" if area.trim().is_empty() => {
                entry.insert("keys", value(toml_edit::Array::new()));
            }
            "lights" => {
                entry.insert("area", value(area.trim()));
            }
            "music" if !player.trim().is_empty() => {
                entry.insert("player", value(player.trim()));
            }
            _ => {}
        }
        let module_table = ensure_table(&mut doc, module);
        module_table.insert("enabled", value(true));
        module_table
            .entry(list)
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .ok_or_else(|| invalid(format!("modules.{module}.{list} is not a list of tables")))?
            .push(entry);
        if let Some(modes) = profile_modes(&mut doc, profile) {
            modes.push(id.as_str());
        }
        if let Some(order) = doc
            .as_table_mut()
            .get_mut("mode_order")
            .and_then(Item::as_array_mut)
        {
            order.push(id.as_str());
        }
        self.commit(&doc)?;
        Ok(id)
    }

    pub fn rename_tab(&self, mode: &str, title: &str) -> Result<(), SettingsError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(invalid("a tab needs a name"));
        }
        let mut doc = self.read()?;
        let place = locate(&doc, mode)?;
        let table = table_mut(&mut doc, place).ok_or_else(|| invalid("tab not found"))?;
        // Keep the id: profiles and mode_order refer to it.
        if !table.contains_key("id") {
            table.insert("id", value(mode));
        }
        table.insert("title", value(title));
        self.commit(&doc)
    }

    /// A music tab's player: as written for it (empty if inherited), and the one that applies.
    pub fn music_tab_player(&self, mode: &str) -> Result<(String, String), SettingsError> {
        let mut doc = self.read()?;
        let place = locate(&doc, mode)?;
        let top = top(&doc, "lyrion").and_then(|t| str_of(t, "player"));
        let own = match place {
            Place::Top("lyrion") => top.clone(),
            Place::Entry("lyrion", _, _) => table_mut(&mut doc, place).and_then(|t| str_of(t, "player")),
            _ => return Err(invalid(format!("{mode:?} is not a music tab"))),
        };
        let effective = own
            .clone()
            .or(top)
            .unwrap_or_else(|| modules::lyrion::DEFAULT_PLAYER.to_owned());
        Ok((own.unwrap_or_default(), effective))
    }

    /// Makes a music tab control another Lyrion player (H15-87). On the top-level tab this is
    /// `[modules.lyrion] player`, which tabs without their own player inherit.
    pub fn set_music_tab_player(&self, mode: &str, player: &str) -> Result<(), SettingsError> {
        let player = player.trim();
        if player.is_empty() {
            return Err(invalid("choose a player"));
        }
        let mut doc = self.read()?;
        let place = locate(&doc, mode)?;
        if !matches!(place, Place::Top("lyrion") | Place::Entry("lyrion", _, _)) {
            return Err(invalid(format!("{mode:?} is not a music tab")));
        }
        let table = table_mut(&mut doc, place).ok_or_else(|| invalid("tab not found"))?;
        table.insert("player", value(player));
        self.commit(&doc)
    }

    pub fn delete_tab(&self, mode: &str) -> Result<(), SettingsError> {
        let mut doc = self.read()?;
        pin_ids(&mut doc);
        let place = locate(&doc, mode)?;
        Self::remove_tab_keep_lists(&mut doc, place);
        rename_in_lists(&mut doc, mode, None);
        self.commit(&doc)
    }

    /// Sets the tab order: the active profile's modes when `profile` names one, else `mode_order`.
    pub fn move_tabs(&self, order: &[String], profile: &str) -> Result<(), SettingsError> {
        let mut doc = self.read()?;
        pin_ids(&mut doc);
        let list: toml_edit::Array = order.iter().map(String::as_str).collect();
        match profile_modes(&mut doc, profile) {
            Some(modes) => *modes = list,
            None => {
                doc.insert("mode_order", value(list));
            }
        }
        self.commit(&doc)
    }

    /// The now-playing band of the music tab `mode` (H15-88): its own `now_playing`, else the top
    /// level's (which `[[tab]]`s inherit), else the defaults.
    pub fn now_playing(&self, mode: &str) -> Result<modules::lyrion::now_playing::Config, SettingsError> {
        let doc = self.read()?;
        let place = locate(&doc, mode)?;
        let own = match place {
            Place::Top("lyrion") => top(&doc, "lyrion"),
            Place::Entry("lyrion", list, i) => entry_list(&doc, "lyrion", list).and_then(|l| l.get(i)),
            _ => return Err(invalid(format!("{mode:?} is not a music tab"))),
        };
        let item = own
            .and_then(|t| t.get("now_playing"))
            .or_else(|| top(&doc, "lyrion").and_then(|t| t.get("now_playing")));
        let inline = item
            .and_then(|i| i.as_value())
            .and_then(toml_edit::Value::as_inline_table);
        match inline {
            Some(t) => {
                modules::lyrion::now_playing::Config::from_inline(t).map_err(|e| invalid(format!("{e:#}")))
            }
            None => Ok(modules::lyrion::now_playing::Config::default()),
        }
    }

    /// Sets the now-playing band of the music tab `mode`. Changing the top-level tab first copies
    /// its old setting into the `[[tab]]`s that inherited it, so they stay as they are.
    pub fn set_now_playing(
        &self,
        mode: &str,
        config: &modules::lyrion::now_playing::Config,
    ) -> Result<(), SettingsError> {
        let mut doc = self.read()?;
        let place = locate(&doc, mode)?;
        if !matches!(place, Place::Top("lyrion") | Place::Entry("lyrion", _, _)) {
            return Err(invalid(format!("{mode:?} is not a music tab")));
        }
        if place == Place::Top("lyrion") {
            let inherited = top(&doc, "lyrion").and_then(|t| t.get("now_playing")).cloned();
            if let (Some(old), Some(entries)) = (inherited, entry_list_mut(&mut doc, "lyrion", "tab")) {
                for t in entries.iter_mut() {
                    if !t.contains_key("now_playing") {
                        t.insert("now_playing", old.clone());
                    }
                }
            }
        }
        let table = table_mut(&mut doc, place).ok_or_else(|| invalid("tab not found"))?;
        table.insert("now_playing", value(config.to_inline_table()));
        self.commit(&doc)
    }

    /// The lights tab `mode`: its area, pinned keys and knob functions.
    pub fn lights_tab(&self, mode: &str) -> Result<LightsTab, SettingsError> {
        let config = self.lights_config(mode)?;
        Ok(LightsTab {
            area: config.area.clone(),
            pinned: config.pinned,
            keys: config.pins.clone(),
            knobs: config.knobs.map(|k| k.name().to_owned()),
        })
    }

    /// The parsed config of the lights tab `mode` (with the connection).
    pub fn lights_config(&self, mode: &str) -> Result<modules::homeassistant::Config, SettingsError> {
        let doc = self.read()?;
        let table: toml::Table = toml::from_str(&doc.to_string()).map_err(|e| invalid(e.to_string()))?;
        let ha = table
            .get("modules")
            .and_then(|m| m.get("homeassistant"))
            .and_then(toml::Value::as_table)
            .ok_or_else(|| SettingsError::NotFound(format!("no lights tab {mode:?}")))?;
        modules::homeassistant::checked_tabs(ha, Some(&crate::assets::Assets::beside(&self.path)))
            .map_err(|e| invalid(format!("{e:#}")))?
            .into_iter()
            .find(|t| t.id == mode)
            .ok_or_else(|| SettingsError::NotFound(format!("no lights tab {mode:?}")))
    }

    /// Pins `keys` (including each action/service) to the lights tab `mode`, replacing its area's lights.
    pub fn set_lights_keys(&self, mode: &str, keys: &[Pin]) -> Result<(), SettingsError> {
        let mut doc = self.read()?;
        let place = locate(&doc, mode)?;
        if !matches!(
            place,
            Place::Top("homeassistant") | Place::Entry("homeassistant", _, _)
        ) {
            return Err(invalid(format!("{mode:?} is not a lights tab")));
        }
        let mut list = toml_edit::Array::new();
        let mut sorted = keys.to_vec();
        sorted.sort_by_key(|k| k.key);
        for pin in sorted {
            let mut t = toml_edit::InlineTable::new();
            t.insert("key", i64::from(pin.key).into());
            t.insert("entity", pin.entity.as_str().into());
            for (name, text) in [
                ("label", pin.label.as_deref().unwrap_or_default()),
                ("action", pin.action.as_str()),
                ("service", pin.service.as_str()),
                ("icon", pin.icon.as_str()),
            ] {
                if !text.trim().is_empty() {
                    t.insert(name, text.trim().into());
                }
            }
            list.push(t);
        }
        list.set_trailing_comma(false);
        let table = table_mut(&mut doc, place).expect("located");
        table.insert("keys", value(list));
        self.commit(&doc)
    }

    /// Sets what knob 1 or 2 does on the lights tab `mode`.
    pub fn set_lights_knob(&self, mode: &str, knob: u32, function: &str) -> Result<(), SettingsError> {
        KnobFn::parse(function).map_err(invalid)?;
        let key = match knob {
            1 => "knob1",
            2 => "knob2",
            _ => return Err(invalid("lights tabs have knobs 1 and 2 (knob 3 switches tabs)")),
        };
        let mut doc = self.read()?;
        let place = locate(&doc, mode)?;
        if !matches!(
            place,
            Place::Top("homeassistant") | Place::Entry("homeassistant", _, _)
        ) {
            return Err(invalid(format!("{mode:?} is not a lights tab")));
        }
        table_mut(&mut doc, place)
            .expect("located")
            .insert(key, value(function));
        self.commit(&doc)
    }

    /// Replaces the tab `mode` by a custom keys page with `keys` (each a `[[page.key]]` table),
    /// at the same place in `mode_order` and the profiles. Returns the new page's id.
    pub fn replace_with_keys_page(
        &self,
        mode: &str,
        title: &str,
        keys: Vec<Table>,
    ) -> Result<String, SettingsError> {
        let mut doc = self.read()?;
        pin_ids(&mut doc);
        let place = locate(&doc, mode)?;
        let mut taken = all_ids(&doc);
        taken.retain(|id| id != mode);
        let id = new_id(&format!("{title} keys"), "keys", &taken);
        let mut page = Table::new();
        page.insert("id", value(&id));
        page.insert("title", value(title));
        let mut list = ArrayOfTables::new();
        for k in keys {
            list.push(k);
        }
        page.insert("key", Item::ArrayOfTables(list));
        // The new page takes the old tab's place in mode_order and the profiles.
        rename_in_lists(&mut doc, mode, Some(&id));
        Self::remove_tab_keep_lists(&mut doc, place);
        let keys = ensure_table(&mut doc, "keys");
        keys.insert("enabled", value(true));
        keys.entry("page")
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .ok_or_else(|| invalid("modules.keys.page is not a list of tables"))?
            .push(page);
        self.commit(&doc)?;
        Ok(id)
    }

    /// Removes a tab's settings (not its id from the lists).
    fn remove_tab_keep_lists(doc: &mut DocumentMut, place: Place) {
        match place {
            Place::Top("homeassistant") => {
                let t = top_mut(doc, "homeassistant").expect("located");
                for key in ["area", "keys", "knob1", "knob2", "level_keys", "title", "id"] {
                    t.remove(key);
                }
            }
            Place::Top(module) => {
                // The player stays: it is the default of the other music tabs and of key actions.
                let t = top_mut(doc, module).expect("located");
                for key in ["title", "id"] {
                    t.remove(key);
                }
                t.insert("show", value(false));
            }
            Place::Entry(module, list, i) => {
                let entries = entry_list_mut(doc, module, list).expect("located");
                entries.remove(i);
                if entries.is_empty() {
                    let t = top_mut(doc, module).expect("located");
                    t.remove(list);
                    // The keys module needs at least one page while enabled.
                    if module == "keys" {
                        t.insert("enabled", value(false));
                    }
                }
            }
        }
    }
}

/// An action as an inline TOML table.
fn action(pairs: &[(&str, toml_edit::Value)]) -> toml_edit::Value {
    let mut t = toml_edit::InlineTable::new();
    for (k, v) in pairs {
        t.insert(*k, v.clone());
    }
    toml_edit::Value::InlineTable(t)
}

fn ha_service(service: &str, entity: &str) -> toml_edit::Value {
    let mut data = toml_edit::InlineTable::new();
    data.insert("entity_id", entity.into());
    action(&[
        ("type", "homeassistant".into()),
        ("service", service.into()),
        ("data", toml_edit::Value::InlineTable(data)),
    ])
}

fn lyrion(command: &[&str], player: Option<&str>) -> toml_edit::Value {
    let command: toml_edit::Array = command.iter().copied().collect();
    let mut pairs = vec![
        ("type", "lyrion".into()),
        ("command", toml_edit::Value::Array(command)),
    ];
    if let Some(p) = player {
        pairs.push(("player", p.into()));
    }
    action(&pairs)
}

/// A `[[page.key]]` table.
fn key_table(key: u8, label: &str, action: toml_edit::Value) -> Table {
    let mut t = Table::new();
    t.insert("key", value(i64::from(key)));
    t.insert("label", value(label));
    t.insert("action", value(action));
    t
}

/// The custom key that does what a lights tab's key does: a toggle that follows the entity, or
/// activating a scene, script or button.
fn ha_key(key: u8, entity: &str, label: &str) -> Table {
    let domain = entity.split('.').next().unwrap_or("");
    let act = match domain {
        "scene" | "script" => ha_service(&format!("{domain}.turn_on"), entity),
        "button" | "input_button" => ha_service(&format!("{domain}.press"), entity),
        "light" | "switch" | "fan" | "input_boolean" => action(&[
            ("type", "toggle".into()),
            ("on", ha_service(&format!("{domain}.turn_on"), entity)),
            ("off", ha_service(&format!("{domain}.turn_off"), entity)),
        ]),
        _ => action(&[
            ("type", "toggle".into()),
            ("on", ha_service("homeassistant.turn_on", entity)),
            ("off", ha_service("homeassistant.turn_off", entity)),
        ]),
    };
    key_table(key, label, act)
}

impl Store {
    /// The Home Assistant connection (of tab `mode`, or the top-level one) and its token.
    fn ha(&self, mode: Option<&str>) -> Result<(modules::homeassistant::Config, String), SettingsError> {
        let config = if let Some(mode) = mode {
            self.lights_config(mode)?
        } else {
            let doc = self.read()?;
            let table: toml::Table = toml::from_str(&doc.to_string()).map_err(|e| invalid(e.to_string()))?;
            let ha = table
                .get("modules")
                .and_then(|m| m.get("homeassistant"))
                .and_then(toml::Value::as_table)
                .ok_or_else(|| invalid("set up Home Assistant on the settings page first"))?;
            modules::homeassistant::Config::from_table(ha).map_err(|e| invalid(format!("{e:#}")))?
        };
        let token = modules::homeassistant::read_token(&config.token_file)
            .map_err(|e| invalid(format!("no Home Assistant token: {e}")))?;
        Ok((config, token))
    }

    /// [`Store::lights_tab`], with the keys an area tab shows right now (from Home Assistant).
    pub async fn lights_tab_live(&self, mode: &str) -> Result<LightsTab, SettingsError> {
        let mut tab = self.lights_tab(mode)?;
        if !tab.pinned
            && let Ok((config, token)) = self.ha(Some(mode))
            && let Ok(keys) = modules::homeassistant::tab_entities(&config, &token).await
        {
            tab.keys = keys
                .into_iter()
                .map(|(key, entity)| Pin {
                    key,
                    entity,
                    ..Pin::default()
                })
                .collect();
        }
        Ok(tab)
    }

    /// Pins `entity` to `key` of the lights tab `mode` (empty `entity`: the key shows nothing).
    /// An area tab first gets its current lights pinned, so the other keys stay as they are.
    pub async fn set_lights_key(&self, mode: &str, mut pin: Pin) -> Result<(), SettingsError> {
        let key = pin.key;
        if !(1..=6).contains(&key) {
            return Err(invalid("lights tabs use keys 1-6"));
        }
        let tab = self.lights_tab(mode)?;
        let mut keys = if tab.pinned {
            tab.keys
        } else {
            let (config, token) = self.ha(Some(mode))?;
            modules::homeassistant::tab_entities(&config, &token)
                .await
                .map_err(|e| invalid(format!("Home Assistant: {e:#}")))?
                .into_iter()
                .map(|(key, entity)| Pin {
                    key,
                    entity,
                    ..Pin::default()
                })
                .collect()
        };
        keys.retain(|p| p.key != key);
        pin.entity = pin.entity.trim().to_owned();
        if !pin.entity.is_empty() {
            keys.push(pin);
        }
        self.set_lights_keys(mode, &keys)
    }

    /// Entities that can go on a key, for the entity picker.
    pub async fn ha_entities(&self) -> Result<Vec<modules::homeassistant::EntityInfo>, SettingsError> {
        let (config, token) = self.ha(None)?;
        modules::homeassistant::key_entities(&config, &token)
            .await
            .map_err(|e| invalid(format!("Home Assistant: {e:#}")))
    }

    /// Replaces the lights or music tab `mode` by a keys tab doing the same; returns its id.
    pub async fn convert_tab(&self, mode: &str) -> Result<String, SettingsError> {
        let doc = self.read()?;
        let place = locate(&doc, mode)?;
        let table: toml::Table = toml::from_str(&doc.to_string()).map_err(|e| invalid(e.to_string()))?;
        let module_table = |m: &str| {
            table
                .get("modules")
                .and_then(|t| t.get(m))
                .and_then(toml::Value::as_table)
                .cloned()
                .unwrap_or_default()
        };
        match place {
            Place::Top("homeassistant") | Place::Entry("homeassistant", _, _) => {
                let (config, token) = self.ha(Some(mode))?;
                let keys = modules::homeassistant::tab_entities(&config, &token)
                    .await
                    .map_err(|e| invalid(format!("Home Assistant: {e:#}")))?;
                let names = modules::homeassistant::key_entities(&config, &token)
                    .await
                    .unwrap_or_default();
                let label = |key: u8, entity: &str| {
                    config
                        .pins
                        .iter()
                        .find(|p| p.key == key && p.entity == entity)
                        .and_then(|p| p.label.clone())
                        .or_else(|| {
                            names
                                .iter()
                                .find(|n| n.entity_id == entity)
                                .map(|n| n.name.clone())
                        })
                        .unwrap_or_else(|| entity.to_owned())
                };
                let tables = keys
                    .iter()
                    .map(|(k, e)| {
                        let label = label(*k, e);
                        let mut table = match config
                            .pins
                            .iter()
                            .find(|p| p.key == *k)
                            .and_then(Pin::explicit_service)
                        {
                            Some(service) => key_table(*k, &label, ha_service(service, e)),
                            None => ha_key(*k, e, &label),
                        };
                        if let Some(pin) = config.pins.iter().find(|p| p.key == *k && !p.icon.is_empty()) {
                            table.insert("image", value(pin.icon.as_str()));
                        }
                        table
                    })
                    .collect();
                self.replace_with_keys_page(mode, &config.title, tables)
            }
            Place::Top("lyrion") | Place::Entry("lyrion", _, _) => {
                let (url, tabs) =
                    modules::lyrion::tabs(&module_table("lyrion")).map_err(|e| invalid(format!("{e:#}")))?;
                let tab = tabs
                    .into_iter()
                    .find(|t| t.id == mode)
                    .ok_or_else(|| SettingsError::NotFound(format!("no music tab {mode:?}")))?;
                let default_player = module_table("lyrion")
                    .get("player")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned);
                let player =
                    (default_player.as_deref() != Some(tab.player.as_str())).then_some(tab.player.as_str());
                let playlists = modules::lyrion::tab_playlists(&url, &tab.prefix)
                    .await
                    .map_err(|e| invalid(format!("Lyrion: {e:#}")))?;
                let mut tables: Vec<Table> = playlists
                    .iter()
                    .take(6)
                    .zip(1u8..)
                    .map(|((id, name), key)| {
                        let load = format!("playlist_id:{id}");
                        key_table(key, name, lyrion(&["playlistcontrol", "cmd:load", &load], player))
                    })
                    .collect();
                for (key, label, command) in [
                    (7, "Previous", &["playlist", "index", "-1"][..]),
                    (8, "Play/Pause", &["pause"][..]),
                    (9, "Next", &["playlist", "index", "+1"][..]),
                ] {
                    tables.push(key_table(key, label, lyrion(command, player)));
                }
                self.replace_with_keys_page(mode, &tab.title, tables)
            }
            _ => Err(invalid(format!("{mode:?} is already a keys tab"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str, text: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("streamdockd-tabs-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, text).unwrap();
        (Store::new(path.clone(), Vec::new()), dir)
    }

    const BASE: &str = "# my dock\n\
        [modules.homeassistant]\nurl = \"https://ha\"\narea = \"Office\"   # the office\n\n\
        [modules.lyrion]\nurl = \"http://lms:9000\"\nplayer = \"Office\"\n\n\
        [[profiles]]\nname = \"Day\"\nmodes = [\"homeassistant\", \"lyrion\"]\n";

    fn read(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("config.toml")).unwrap()
    }

    #[test]
    fn music_tabs_switch_player() {
        let text = format!("{BASE}\n[[modules.lyrion.tab]]\nid = \"kitchen\"\ntitle = \"Kitchen\"\n");
        let (s, dir) = store("music-player", &text);
        assert_eq!(
            s.music_tab_player("lyrion").unwrap(),
            ("Office".into(), "Office".into())
        );
        assert_eq!(
            s.music_tab_player("kitchen").unwrap(),
            (String::new(), "Office".into()),
            "inherits the top-level player"
        );
        s.set_music_tab_player("kitchen", "aa:aa:00:00:00:06").unwrap();
        assert_eq!(s.music_tab_player("kitchen").unwrap().0, "aa:aa:00:00:00:06");
        assert_eq!(
            s.music_tab_player("lyrion").unwrap().0,
            "Office",
            "the main tab keeps its player"
        );
        assert!(read(&dir).contains("# my dock"), "comments kept");
        assert!(
            s.set_music_tab_player("homeassistant", "x").is_err(),
            "not a music tab"
        );
        assert!(s.set_music_tab_player("kitchen", " ").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn now_playing_per_music_tab() {
        use crate::modules::lyrion::now_playing::Config;
        let base = "[modules.lyrion]\nurl = \"http://lms:9000\"\nplayer = \"Office\"\nnow_playing = { keys = [1] }\n\n\
            [[modules.lyrion.tab]]\nid = \"kitchen\"\nplayer = \"Kitchen\"\n";
        let (s, dir) = store("nowplaying", base);
        assert_eq!(
            s.now_playing("kitchen").unwrap().keys,
            [1],
            "inherits the top level"
        );
        let off = Config {
            keys: Vec::new(),
            ..Config::default()
        };
        s.set_now_playing("lyrion", &off).unwrap();
        assert_eq!(s.now_playing("lyrion").unwrap(), off);
        assert_eq!(
            s.now_playing("kitchen").unwrap().keys,
            [1],
            "kept what it inherited"
        );
        let row = Config {
            keys: vec![4, 5, 6],
            ..Config::default()
        };
        s.set_now_playing("kitchen", &row).unwrap();
        assert_eq!(s.now_playing("kitchen").unwrap(), row);
        // The file still parses as a config with both tabs.
        let config = crate::config::Settings::parse(&read(&dir)).unwrap();
        let (_, tabs) = crate::modules::lyrion::tabs(&config.modules["lyrion"]).unwrap();
        assert_eq!(tabs[1].now_playing, row);
        assert!(
            s.set_now_playing("homeassistant", &row).is_err(),
            "not a music tab"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn add_rename_move_delete() {
        let (s, dir) = store("crud", BASE);
        let id = s
            .add_tab("lights", "Living room", "Wohnzimmer", "", "Day")
            .unwrap();
        assert_eq!(id, "living-room");
        let music = s.add_tab("music", "Kitchen", "", "Kitchen Radio", "").unwrap();
        let keys = s.add_tab("keys", "Office", "", "", "Day").unwrap();
        assert_eq!(keys, "office");
        let text = read(&dir);
        assert!(text.contains("# the office"), "comments stay: {text}");
        assert!(
            text.contains("modes = [\"homeassistant\", \"lyrion\", \"living-room\", \"office\"]"),
            "{text}"
        );
        assert!(text.contains("player = \"Kitchen Radio\""));

        s.rename_tab("homeassistant", "Büro").unwrap();
        s.rename_tab(&music, "Küche").unwrap();
        let text = read(&dir);
        assert!(text.contains("title = \"Büro\"") && text.contains("title = \"Küche\""));
        assert!(
            text.contains("id = \"homeassistant\""),
            "the id is kept on rename"
        );

        s.move_tabs(
            &[
                "office".into(),
                "lyrion".into(),
                "homeassistant".into(),
                "living-room".into(),
            ],
            "Day",
        )
        .unwrap();
        assert!(read(&dir).contains("modes = [\"office\", \"lyrion\", \"homeassistant\", \"living-room\"]"));
        s.move_tabs(&["lyrion".into()], "").unwrap();
        assert!(read(&dir).contains("mode_order = [\"lyrion\"]"));

        // Deleting the first music tab keeps its player as the default (show = false).
        s.delete_tab("lyrion").unwrap();
        s.delete_tab("living-room").unwrap();
        s.delete_tab("office").unwrap();
        let text = read(&dir);
        assert!(
            !text.contains("living-room") && !text.contains("\"lyrion\""),
            "{text}"
        );
        assert!(text.contains("[modules.keys]") && text.contains("enabled = false"));
        assert!(
            text.contains("show = false") && text.contains("player = \"Office\""),
            "{text}"
        );
        assert!(matches!(s.delete_tab("nope"), Err(SettingsError::NotFound(_))));
        assert!(s.rename_tab("demo-1", "x").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn deleting_keeps_the_ids_of_later_tabs() {
        let (s, dir) = store(
            "ids",
            "[modules.keys]\n[[modules.keys.page]]\ntitle = \"A\"\n[[modules.keys.page]]\ntitle = \"B\"\n\
             [[modules.keys.page]]\ntitle = \"C\"\n\n[[profiles]]\nname = \"P\"\nmodes = [\"keys-3\"]\n",
        );
        s.delete_tab("keys").unwrap();
        let text = read(&dir);
        assert!(text.contains("id = \"keys-3\""), "C keeps its id: {text}");
        assert!(text.contains("modes = [\"keys-3\"]"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lights_keys_and_knobs() {
        let (s, dir) = store("lights", BASE);
        assert_eq!(s.lights_tab("homeassistant").unwrap().area, "Office");
        s.set_lights_keys(
            "homeassistant",
            &[
                Pin {
                    key: 3,
                    entity: "scene.movie".into(),
                    label: Some("Movie".into()),
                    ..Pin::default()
                },
                Pin {
                    key: 1,
                    entity: "light.desk".into(),
                    ..Pin::default()
                },
            ],
        )
        .unwrap();
        s.set_lights_knob("homeassistant", 2, "none").unwrap();
        let tab = s.lights_tab("homeassistant").unwrap();
        assert!(tab.pinned);
        assert_eq!(
            tab.keys[0],
            Pin {
                key: 1,
                entity: "light.desk".into(),
                ..Pin::default()
            }
        );
        assert_eq!(
            tab.keys[1],
            Pin {
                key: 3,
                entity: "scene.movie".into(),
                label: Some("Movie".into()),
                ..Pin::default()
            }
        );
        assert_eq!(tab.knobs, ["brightness".to_owned(), "none".to_owned()]);
        assert!(s.set_lights_knob("homeassistant", 1, "volume").is_err());
        assert!(s.set_lights_keys("lyrion", &[]).is_err());
        assert!(
            s.set_lights_keys(
                "homeassistant",
                &[Pin {
                    key: 7,
                    entity: "light.x".into(),
                    ..Pin::default()
                }]
            )
            .is_err()
        );

        // An empty lights tab (pin entities later) is valid.
        let id = s.add_tab("lights", "Spare", "", "", "").unwrap();
        assert!(s.lights_tab(&id).unwrap().pinned);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn editing_a_pin_preserves_other_cover_actions() {
        let src = BASE.replace(
            "area = \"Office\"",
            r#"keys = [
            {key=1, entity="light.desk"},
            {key=4, entity="cover.test", label="Up", action="open"},
            {key=5, entity="cover.test", label="Stop", action="stop"},
            {key=6, entity="cover.test", label="Down", service="cover.close_cover"}
        ]"#,
        );
        let (s, dir) = store("cover-edit", &src);
        s.set_lights_key(
            "homeassistant",
            Pin {
                key: 1,
                entity: "light.other".into(),
                label: Some("Ceiling".into()),
                ..Pin::default()
            },
        )
        .await
        .unwrap();
        let config = s.lights_config("homeassistant").unwrap();
        assert_eq!(config.pins.len(), 4);
        assert_eq!(config.pins[0].entity, "light.other");
        assert_eq!(config.pins[1].action, "open");
        assert_eq!(config.pins[2].action, "stop");
        assert_eq!(config.pins[3].service, "cover.close_cover");
        assert_eq!(config.pins[1].label.as_deref(), Some("Up"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn settings_edits_preserve_uploaded_pin_icons_and_reject_missing_ones() {
        let src = BASE.replace(
            "area = \"Office\"",
            r#"keys = [{key=1,entity="light.desk",icon="family-lamp"}]"#,
        );
        let (s, dir) = store("pin-icons", &src);
        let assets = crate::assets::Assets::beside(&dir.join("config.toml"));
        assets
            .put("family-lamp", &crate::icons::preview("home").unwrap())
            .unwrap();
        s.set_lights_key(
            "homeassistant",
            Pin {
                key: 4,
                entity: "cover.test".into(),
                action: "open".into(),
                ..Pin::default()
            },
        )
        .await
        .unwrap();
        let written: toml::Value = toml::from_str(&read(&dir)).unwrap();
        assert_eq!(
            written["modules"]["homeassistant"]["keys"][0]["icon"].as_str(),
            Some("family-lamp")
        );
        // Saving must fail if the referenced upload has vanished, keeping the last config intact.
        assets.delete("family-lamp").unwrap();
        let before = read(&dir);
        assert!(
            s.set_lights_key(
                "homeassistant",
                Pin {
                    key: 2,
                    entity: "light.other".into(),
                    ..Pin::default()
                }
            )
            .await
            .is_err()
        );
        assert_eq!(read(&dir), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn adding_needs_the_connection_first() {
        let (s, dir) = store("noconn", "");
        assert!(
            s.add_tab("lights", "x", "a", "", "")
                .unwrap_err()
                .to_string()
                .contains("settings page")
        );
        assert!(s.add_tab("music", "x", "", "p", "").is_err());
        assert_eq!(s.add_tab("keys", "Keys", "", "", "").unwrap(), "keys");
        assert!(s.add_tab("disco", "x", "", "", "").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replacing_a_tab_with_a_keys_page_keeps_its_place() {
        let (s, dir) = store("convert", BASE);
        let mut key = Table::new();
        key.insert("key", value(1));
        key.insert("label", value("Desk"));
        let mut action = toml_edit::InlineTable::new();
        action.insert("type", "homeassistant".into());
        action.insert("service", "light.toggle".into());
        key.insert("action", value(action));
        let id = s
            .replace_with_keys_page("homeassistant", "Lights", vec![key])
            .unwrap();
        assert_eq!(id, "lights-keys");
        let text = read(&dir);
        assert!(text.contains("modes = [\"lights-keys\", \"lyrion\"]"), "{text}");
        assert!(!text.contains("area = "), "{text}");
        assert!(text.contains("[[modules.keys.page.key]]"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

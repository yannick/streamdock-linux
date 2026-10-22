//! The keys editor's side of the settings store (H15-29): `[modules.keys]` read for the editor and
//! written back with `toml_edit`, so the rest of the config file and its comments are kept.

use std::collections::BTreeMap;

use streamdock_proto::streamdock::v1 as pb;

use super::{SettingsError, Store, write_atomically};
use crate::modules::{self, keys::editor};

/// Keys of a key or knob that hold an action; written as inline tables, like people type them.
const ACTION_KEYS: [&str; 3] = ["action", "turn", "press"];

impl Store {
    /// Every `[modules.<id>]` table of the config file.
    fn module_tables(&self) -> Result<BTreeMap<String, toml::Table>, SettingsError> {
        let doc: toml::Table =
            toml::from_str(&self.read()?.to_string()).map_err(|e| SettingsError::Invalid(e.to_string()))?;
        Ok(doc
            .get("modules")
            .and_then(toml::Value::as_table)
            .map(|m| {
                m.iter()
                    .filter_map(|(id, t)| Some((id.clone(), t.as_table()?.clone())))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// `[modules.keys]` for the editor, and what its pickers offer (images are added by the API).
    pub fn key_pages(&self) -> Result<pb::GetKeyPagesResponse, SettingsError> {
        let tables = self.module_tables()?;
        let pages = editor::pages_from_table(tables.get("keys"))
            .map_err(|e| SettingsError::Invalid(format!("[modules.keys]: {e:#}")))?;
        let enabled = |t: &toml::Table| t.get("enabled").and_then(toml::Value::as_bool) != Some(false);
        let mut modes: Vec<String> = tables
            .iter()
            .filter(|(id, t)| id.as_str() != "keys" && enabled(t))
            .map(|(id, _)| id.clone())
            .collect();
        for (i, page) in pages.pages.iter().enumerate() {
            modes.push(match (page.id.is_empty(), i) {
                (false, _) => page.id.clone(),
                (true, 0) => "keys".to_owned(),
                (true, i) => format!("keys-{}", i + 1),
            });
        }
        let doc: toml::Table =
            toml::from_str(&self.read()?.to_string()).map_err(|e| SettingsError::Invalid(e.to_string()))?;
        let profiles = doc
            .get("profiles")
            .and_then(toml::Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|p| p.get("name")?.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Ok(pb::GetKeyPagesResponse {
            pages: buffa::MessageField::some(pages),
            modes,
            profiles,
            homeassistant: tables.contains_key("homeassistant"),
            lyrion: tables.contains_key("lyrion"),
            ..Default::default()
        })
    }

    /// Validates `pages` like the daemon does at startup and writes them as `[modules.keys]`.
    pub fn save_key_pages(&self, pages: &pb::KeyPages) -> Result<(), SettingsError> {
        let invalid = |e: anyhow::Error| SettingsError::Invalid(format!("{e:#}"));
        let table = editor::table_from_pages(pages).map_err(invalid)?;
        if pages.enabled {
            let mut tables = self.module_tables()?;
            tables.insert("keys".into(), table.clone());
            let context = modules::Context::new(crate::hub::Hub::new(), &tables);
            modules::keys::from_table(&table, &context).map_err(invalid)?;
        }

        let mut doc = self.read()?;
        let modules = doc
            .entry("modules")
            .or_insert_with(|| {
                let mut t = toml_edit::Table::new();
                t.set_implicit(true);
                toml_edit::Item::Table(t)
            })
            .as_table_mut()
            .ok_or_else(|| SettingsError::Invalid("`modules` in the config is not a table".into()))?;
        modules.insert("keys", keys_item(&table)?);
        write_atomically(&self.path, doc.to_string().as_bytes(), None)
    }
}

/// `table` as the `[modules.keys]` item: pages, keys and knobs as `[[…]]` tables, actions inline.
fn keys_item(table: &toml::Table) -> Result<toml_edit::Item, SettingsError> {
    let mut wrapper = toml::Table::new();
    let mut modules = toml::Table::new();
    modules.insert("keys".into(), toml::Value::Table(table.clone()));
    wrapper.insert("modules".into(), toml::Value::Table(modules));
    let text = toml::to_string(&wrapper).map_err(|e| SettingsError::Invalid(e.to_string()))?;
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| SettingsError::Invalid(format!("{e}")))?;
    let mut item = std::mem::take(&mut doc["modules"]["keys"]);
    if let Some(t) = item.as_table_mut() {
        t.decor_mut().set_prefix("\n");
    }
    let pages = item
        .get_mut("page")
        .and_then(toml_edit::Item::as_array_of_tables_mut);
    for page in pages.into_iter().flat_map(toml_edit::ArrayOfTables::iter_mut) {
        for list in ["key", "knob"] {
            let slots = page
                .get_mut(list)
                .and_then(toml_edit::Item::as_array_of_tables_mut);
            for slot in slots.into_iter().flat_map(toml_edit::ArrayOfTables::iter_mut) {
                inline_actions(slot);
            }
        }
    }
    Ok(item)
}

/// Writes a key's or knob's actions as inline tables.
fn inline_actions(slot: &mut toml_edit::Table) {
    for key in ACTION_KEYS {
        if let Some(action) = slot.remove(key) {
            slot.insert(key, toml_edit::value(inline(action)));
        }
    }
}

/// A table item (and the tables in it) as one inline table, `type` first: `{ type = "mode", id = "x" }`.
fn inline(item: toml_edit::Item) -> toml_edit::Value {
    let mut value = match item {
        toml_edit::Item::Table(t) => toml_edit::Value::InlineTable(t.into_inline_table()),
        toml_edit::Item::ArrayOfTables(a) => toml_edit::Value::Array(a.into_array()),
        toml_edit::Item::Value(v) => v,
        toml_edit::Item::None => toml_edit::Value::InlineTable(toml_edit::InlineTable::new()),
    };
    type_first(&mut value);
    value.decor_mut().clear();
    value
}

fn type_first(value: &mut toml_edit::Value) {
    match value {
        toml_edit::Value::InlineTable(t) => {
            t.sort_values_by(|a, _, b, _| (a.get() != "type").cmp(&(b.get() != "type")));
            for (_, v) in t.iter_mut() {
                type_first(v);
            }
            t.fmt();
        }
        toml_edit::Value::Array(a) => {
            for v in a.iter_mut() {
                type_first(v);
            }
            a.fmt();
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str, config: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("streamdockd-keys-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, config).unwrap();
        (Store::new(path.clone(), Vec::new()), path)
    }

    const CONFIG: &str = r#"# my dock
brightness = 70 # bright enough

# Music
[modules.lyrion]
url = "http://lms:9000"
player = "Kitchen"

[modules.keys]
# old keys, replaced on save

[[modules.keys.page]]
title = "Old"

[[modules.keys.page.key]]
key = 1
action = { type = "mode", id = "lyrion" }

[[profiles]]
name = "Evening"
modes = ["lyrion"]
"#;

    #[test]
    fn reads_pages_and_picker_values() {
        let (store, path) = store("read", CONFIG);
        let got = store.key_pages().unwrap();
        let pages = got.pages.as_option().unwrap();
        assert_eq!(pages.pages[0].title, "Old");
        assert_eq!(got.modes, vec!["lyrion", "keys"]);
        assert_eq!(got.profiles, vec!["Evening"]);
        assert!(got.lyrion && !got.homeassistant);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn saves_keys_and_keeps_the_rest_of_the_file() {
        let (store, path) = store("save", CONFIG);
        let mut pages = store.key_pages().unwrap().pages.into_option().unwrap();
        pages.pages[0].title = "New".into();
        pages.pages[0].keys[0].label = "Music".into();
        store.save_key_pages(&pages).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        for kept in [
            "# my dock",
            "brightness = 70 # bright enough",
            "# Music",
            "player = \"Kitchen\"",
            "[[profiles]]",
        ] {
            assert!(text.contains(kept), "{kept:?} kept:\n{text}");
        }
        assert!(text.contains("[[modules.keys.page]]"), "{text}");
        assert!(
            text.contains("action = { type = \"mode\", id = \"lyrion\" }"),
            "actions inline:\n{text}"
        );
        assert!(text.contains("title = \"New\""));
        // And the daemon reads it back the same.
        let again = store.key_pages().unwrap().pages.into_option().unwrap();
        assert_eq!(again, pages);

        // A new, still empty page (the editor adds keys afterwards).
        pages.pages.push(pb::KeyPage {
            title: "Second".into(),
            ..Default::default()
        });
        store.save_key_pages(&pages).unwrap();
        assert_eq!(store.key_pages().unwrap().modes, vec!["lyrion", "keys", "keys-2"]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn refuses_what_the_daemon_would_refuse() {
        let (store, path) = store("invalid", CONFIG);
        let mut pages = store.key_pages().unwrap().pages.into_option().unwrap();
        pages.pages[0].keys[0].key = 12;
        let err = store.save_key_pages(&pages).unwrap_err();
        assert!(
            matches!(&err, SettingsError::Invalid(m) if m.contains("key must be 1-9")),
            "{err:?}"
        );
        // Home Assistant actions need [modules.homeassistant], which this config lacks.
        pages.pages[0].keys[0].key = 1;
        pages.pages[0].keys[0].action = buffa::MessageField::some(pb::KeyAction {
            kind: Some(
                pb::HomeAssistantAction {
                    service: "light.toggle".into(),
                    ..Default::default()
                }
                .into(),
            ),
            ..Default::default()
        });
        let err = store.save_key_pages(&pages).unwrap_err();
        assert!(
            matches!(&err, SettingsError::Invalid(m) if m.contains("[modules.homeassistant]")),
            "{err:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CONFIG, "nothing written");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

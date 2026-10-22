//! The keys editor's view of `[modules.keys]` (H15-29): protobuf messages to and from the TOML
//! table a person would type. Saving turns the messages into that table and validates it with
//! [`super::from_table`], so the editor gets exactly the errors the daemon gives at startup.

use anyhow::{Context as _, bail};
use serde_json::Value as Json;
use streamdock_proto::streamdock::v1 as pb;
use streamdock_proto::streamdock::v1::__buffa::oneof::audio_action::Change as AudioChange;
use streamdock_proto::streamdock::v1::__buffa::oneof::brightness_action::Change;
use streamdock_proto::streamdock::v1::__buffa::oneof::key_action::Kind;
use streamdock_proto::streamdock::v1::__buffa::oneof::multi_step::Step;
use toml::{Table, Value};

/// `[modules.keys]` as the editor shows it. A missing table is a disabled module without pages.
pub fn pages_from_table(table: Option<&Table>) -> anyhow::Result<pb::KeyPages> {
    let Some(table) = table else {
        return Ok(pb::KeyPages::default());
    };
    let pages = match table.get("page") {
        None => Vec::new(),
        Some(v) => v
            .as_array()
            .context("page must be a list")?
            .iter()
            .enumerate()
            .map(|(i, p)| page_from(p).with_context(|| format!("page {}", i + 1)))
            .collect::<anyhow::Result<_>>()?,
    };
    Ok(pb::KeyPages {
        enabled: table.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        allow_commands: table
            .get("allow_commands")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        pages,
        ..Default::default()
    })
}

/// The `[modules.keys]` table for `pages` (not validated yet).
pub fn table_from_pages(pages: &pb::KeyPages) -> anyhow::Result<Table> {
    let mut table = Table::new();
    table.insert("enabled".into(), pages.enabled.into());
    if pages.allow_commands {
        table.insert("allow_commands".into(), true.into());
    }
    let list = pages
        .pages
        .iter()
        .enumerate()
        .map(|(i, p)| {
            page_to(p)
                .map(Value::Table)
                .with_context(|| format!("page {}", i + 1))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if !list.is_empty() {
        table.insert("page".into(), Value::Array(list));
    }
    Ok(table)
}

fn text(t: &Table, key: &str) -> String {
    t.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn number(t: &Table, key: &str) -> u32 {
    t.get(key)
        .and_then(Value::as_integer)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

fn tables<'a>(t: &'a Table, key: &str) -> Vec<&'a Table> {
    t.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_table).collect())
        .unwrap_or_default()
}

fn strings(t: &Table, key: &str) -> Vec<String> {
    t.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Inserts `value` unless it is empty.
fn put(t: &mut Table, key: &str, value: &str) {
    if !value.is_empty() {
        t.insert(key.into(), value.into());
    }
}

fn page_from(v: &Value) -> anyhow::Result<pb::KeyPage> {
    let t = v.as_table().context("a page must be a table")?;
    let keys = tables(t, "key")
        .into_iter()
        .map(|k| {
            Ok(pb::KeySlot {
                key: number(k, "key"),
                label: text(k, "label"),
                detail: text(k, "detail"),
                image: text(k, "image"),
                action: optional_action(k.get("action"))
                    .with_context(|| format!("key {}", number(k, "key")))?,
                ..Default::default()
            })
        })
        .collect::<anyhow::Result<_>>()?;
    let knobs = tables(t, "knob")
        .into_iter()
        .map(|k| {
            let n = number(k, "knob");
            Ok(pb::KnobSlot {
                knob: n,
                label: text(k, "label"),
                turn: optional_action(k.get("turn")).with_context(|| format!("knob {n} turn"))?,
                press: optional_action(k.get("press")).with_context(|| format!("knob {n} press"))?,
                ..Default::default()
            })
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(pb::KeyPage {
        id: text(t, "id"),
        title: text(t, "title"),
        keys,
        knobs,
        ..Default::default()
    })
}

fn page_to(p: &pb::KeyPage) -> anyhow::Result<Table> {
    let mut t = Table::new();
    put(&mut t, "id", p.id.trim());
    t.insert("title".into(), p.title.trim().into());
    let keys = p
        .keys
        .iter()
        .map(|k| {
            let mut kt = Table::new();
            kt.insert("key".into(), i64::from(k.key).into());
            put(&mut kt, "label", &k.label);
            put(&mut kt, "detail", &k.detail);
            put(&mut kt, "image", &k.image);
            if let Some(a) = k.action.as_option() {
                kt.insert(
                    "action".into(),
                    Value::Table(action_to(a).with_context(|| format!("key {}", k.key))?),
                );
            }
            Ok(Value::Table(kt))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if !keys.is_empty() {
        t.insert("key".into(), Value::Array(keys));
    }
    let knobs = p
        .knobs
        .iter()
        .map(|k| {
            let mut kt = Table::new();
            kt.insert("knob".into(), i64::from(k.knob).into());
            put(&mut kt, "label", &k.label);
            for (key, action) in [("turn", &k.turn), ("press", &k.press)] {
                if let Some(a) = action.as_option() {
                    let a = action_to(a).with_context(|| format!("knob {} {key}", k.knob))?;
                    kt.insert(key.into(), Value::Table(a));
                }
            }
            Ok(Value::Table(kt))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if !knobs.is_empty() {
        t.insert("knob".into(), Value::Array(knobs));
    }
    Ok(t)
}

type Field = buffa::MessageField<pb::KeyAction, buffa::Inline<pb::KeyAction>>;

fn optional_action(v: Option<&Value>) -> anyhow::Result<Field> {
    match v {
        None => Ok(Field::default()),
        Some(v) => Ok(Field::some(action_from(v)?)),
    }
}

/// An action table (`{ type = "http", … }`) as a message.
fn action_from(v: &Value) -> anyhow::Result<pb::KeyAction> {
    let t = v.as_table().context("an action must be a table")?;
    let timeout = || number(t, "timeout_ms");
    let kind: Kind = match t.get("type").and_then(Value::as_str).unwrap_or_default() {
        "http" => pb::HttpAction {
            method: text(t, "method"),
            url: text(t, "url"),
            body: text(t, "body"),
            headers: t
                .get("headers")
                .and_then(Value::as_table)
                .map(|h| {
                    h.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                        .collect()
                })
                .unwrap_or_default(),
            timeout_ms: timeout(),
            ..Default::default()
        }
        .into(),
        "command" => pb::CommandAction {
            argv: strings(t, "argv"),
            timeout_ms: timeout(),
            ..Default::default()
        }
        .into(),
        "mode" => pb::ModeAction {
            id: text(t, "id"),
            ..Default::default()
        }
        .into(),
        "profile" => pb::ProfileAction {
            name: text(t, "name"),
            ..Default::default()
        }
        .into(),
        "brightness" => {
            let change = match (
                t.get("set").and_then(Value::as_integer),
                t.get("step").and_then(Value::as_integer),
            ) {
                (Some(set), _) => Some(Change::Set(u32::try_from(set).unwrap_or(0))),
                (None, Some(step)) => Some(Change::Step(i32::try_from(step).unwrap_or(0))),
                (None, None) => None,
            };
            pb::BrightnessAction {
                change,
                ..Default::default()
            }
            .into()
        }
        "audio" => {
            let change = match (t.get("step").and_then(Value::as_integer), t.get("mute")) {
                (Some(step), _) => Some(AudioChange::Step(i32::try_from(step).unwrap_or(0))),
                (None, Some(_)) => Some(AudioChange::ToggleMute(true)),
                (None, None) => None,
            };
            pb::AudioAction {
                change,
                ..Default::default()
            }
            .into()
        }
        "homeassistant" => pb::HomeAssistantAction {
            service: text(t, "service"),
            data_json: match t.get("data") {
                None => String::new(),
                Some(d) => serde_json::to_string(d).context("data")?,
            },
            ..Default::default()
        }
        .into(),
        "lyrion" => pb::LyrionAction {
            command: strings(t, "command"),
            player: text(t, "player"),
            ..Default::default()
        }
        .into(),
        "multi" => multi_from(t)?.into(),
        "toggle" => pb::ToggleAction {
            on: optional_action(t.get("on")).context("on")?,
            off: optional_action(t.get("off")).context("off")?,
            on_label: text(t, "on_label"),
            off_label: text(t, "off_label"),
            on_image: text(t, "on_image"),
            off_image: text(t, "off_image"),
            state: text(t, "state"),
            ..Default::default()
        }
        .into(),
        "assist" => assist_from(t).into(),
        other => bail!("unknown action type {other:?}"),
    };
    Ok(pb::KeyAction {
        kind: Some(kind),
        ..Default::default()
    })
}

fn assist_from(t: &Table) -> pb::AssistAction {
    pb::AssistAction {
        tap: t.get("mode").and_then(Value::as_str) == Some("tap"),
        ..Default::default()
    }
}

/// A message as the action table a person would type.
fn brightness_to(t: &mut Table, b: &pb::BrightnessAction) {
    t.insert("type".into(), "brightness".into());
    match b.change {
        Some(Change::Set(v)) => t.insert("set".into(), i64::from(v).into()),
        Some(Change::Step(v)) => t.insert("step".into(), i64::from(v).into()),
        None => None,
    };
}

fn audio_to(t: &mut Table, a: &pb::AudioAction) {
    t.insert("type".into(), "audio".into());
    match a.change {
        Some(AudioChange::Step(v)) => t.insert("step".into(), i64::from(v).into()),
        Some(AudioChange::ToggleMute(_)) => t.insert("mute".into(), "toggle".into()),
        None => None,
    };
}

fn action_to(a: &pb::KeyAction) -> anyhow::Result<Table> {
    let mut t = Table::new();
    let timeout = |t: &mut Table, ms: u32| {
        if ms > 0 {
            t.insert("timeout_ms".into(), i64::from(ms).into());
        }
    };
    match a.kind.as_ref().context("choose an action type")? {
        Kind::Http(h) => {
            t.insert("type".into(), "http".into());
            put(&mut t, "method", h.method.trim());
            t.insert("url".into(), h.url.trim().into());
            put(&mut t, "body", &h.body);
            if !h.headers.is_empty() {
                let mut headers: Vec<_> = h.headers.iter().collect();
                headers.sort();
                let headers: Table = headers
                    .into_iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect();
                t.insert("headers".into(), Value::Table(headers));
            }
            timeout(&mut t, h.timeout_ms);
        }
        Kind::Command(c) => {
            t.insert("type".into(), "command".into());
            t.insert(
                "argv".into(),
                c.argv
                    .iter()
                    .map(|a| Value::String(a.clone()))
                    .collect::<Vec<_>>()
                    .into(),
            );
            timeout(&mut t, c.timeout_ms);
        }
        Kind::Mode(m) => {
            t.insert("type".into(), "mode".into());
            t.insert("id".into(), m.id.trim().into());
        }
        Kind::Profile(p) => {
            t.insert("type".into(), "profile".into());
            t.insert("name".into(), p.name.trim().into());
        }
        Kind::Brightness(b) => brightness_to(&mut t, b),
        Kind::Audio(a) => audio_to(&mut t, a),
        Kind::Homeassistant(h) => {
            t.insert("type".into(), "homeassistant".into());
            t.insert("service".into(), h.service.trim().into());
            if !h.data_json.trim().is_empty() {
                let data: Json = serde_json::from_str(&h.data_json)
                    .map_err(|e| anyhow::anyhow!("homeassistant: data is not valid JSON ({e})"))?;
                anyhow::ensure!(
                    data.is_object(),
                    "homeassistant: data must be a JSON object, e.g. {{\"entity_id\": \"light.desk\"}}"
                );
                t.insert("data".into(), json_to_toml(&data)?);
            }
        }
        Kind::Lyrion(l) => {
            t.insert("type".into(), "lyrion".into());
            t.insert(
                "command".into(),
                l.command
                    .iter()
                    .map(|c| Value::String(c.clone()))
                    .collect::<Vec<_>>()
                    .into(),
            );
            put(&mut t, "player", l.player.trim());
        }
        Kind::Assist(a) => {
            t.insert("type".into(), "assist".into());
            if a.tap {
                t.insert("mode".into(), "tap".into());
            }
        }
        Kind::Multi(m) => {
            t.insert("type".into(), "multi".into());
            t.insert("steps".into(), Value::Array(steps_to(m)?));
        }
        Kind::Toggle(g) => {
            t.insert("type".into(), "toggle".into());
            for (key, side) in [("on", &g.on), ("off", &g.off)] {
                if let Some(a) = side.as_option() {
                    t.insert(
                        key.into(),
                        Value::Table(action_to(a).with_context(|| format!("toggle: {key}"))?),
                    );
                }
            }
            put(&mut t, "on_label", &g.on_label);
            put(&mut t, "off_label", &g.off_label);
            put(&mut t, "on_image", &g.on_image);
            put(&mut t, "off_image", &g.off_image);
            put(&mut t, "state", g.state.trim());
        }
    }
    Ok(t)
}

fn multi_from(t: &Table) -> anyhow::Result<pb::MultiAction> {
    let steps = t
        .get("steps")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|s| {
            let st = s.as_table().context("a step must be a table")?;
            let step = if st.get("type").and_then(Value::as_str) == Some("delay") {
                Step::DelayMs(number(st, "ms"))
            } else {
                Step::Action(Box::new(action_from(s)?))
            };
            Ok(pb::MultiStep {
                step: Some(step),
                ..Default::default()
            })
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(pb::MultiAction {
        steps,
        ..Default::default()
    })
}

fn steps_to(m: &pb::MultiAction) -> anyhow::Result<Vec<Value>> {
    m.steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let step = s
                .step
                .as_ref()
                .with_context(|| format!("multi: step {} is empty", i + 1))?;
            Ok(Value::Table(match step {
                Step::DelayMs(ms) => {
                    let mut d = Table::new();
                    d.insert("type".into(), "delay".into());
                    d.insert("ms".into(), i64::from(*ms).into());
                    d
                }
                Step::Action(a) => action_to(a).with_context(|| format!("multi: step {}", i + 1))?,
            }))
        })
        .collect()
}

fn json_to_toml(v: &Json) -> anyhow::Result<Value> {
    Ok(match v {
        Json::Null => bail!("homeassistant: data can't contain null"),
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Value::Integer(i),
            (None, Some(f)) => Value::Float(f),
            _ => bail!("homeassistant: number {n} is out of range"),
        },
        Json::String(s) => Value::String(s.clone()),
        Json::Array(a) => Value::Array(a.iter().map(json_to_toml).collect::<anyhow::Result<_>>()?),
        Json::Object(o) => Value::Table(
            o.iter()
                .map(|(k, v)| Ok((k.clone(), json_to_toml(v)?)))
                .collect::<anyhow::Result<_>>()?,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
        allow_commands = true

        [[page]]
        id = "office"
        title = "Office"

        [[page.key]]
        key = 1
        label = "Hook"
        image = "hook"
        action = { type = "http", url = "https://x/hook", method = "PUT", body = "{}", headers = { X-Token = "a" }, timeout_ms = 500 }

        [[page.key]]
        key = 2
        action = { type = "toggle", on_label = "On", off_label = "Off", on_image = "a", off_image = "b",
                   on = { type = "homeassistant", service = "light.turn_on", data = { entity_id = "light.a", brightness_pct = 40 } },
                   off = { type = "homeassistant", service = "light.turn_off", data = { entity_id = "light.a" } } }

        [[page.key]]
        key = 7
        action = { type = "multi", steps = [
            { type = "brightness", set = 100 }, { type = "delay", ms = 300 },
            { type = "lyrion", command = ["mixer", "volume", "{delta}"], player = "Kitchen" },
            { type = "command", argv = ["notify-send", "hi"] },
            { type = "mode", id = "lyrion" }, { type = "profile", name = "Evening" } ] }

        [[page.key]]
        key = 3
        label = "Ask"
        action = { type = "assist" }

        [[page.key]]
        key = 4
        action = { type = "assist", mode = "tap" }

        [[page.knob]]
        knob = 1
        label = "Brightness"
        turn = { type = "brightness", step = -5 }
        press = { type = "brightness", set = 80 }
    "#;

    #[test]
    fn round_trips_every_action_type() {
        let table: Table = toml::from_str(CONFIG).unwrap();
        let pages = pages_from_table(Some(&table)).unwrap();
        assert!(pages.enabled && pages.allow_commands);
        assert_eq!(pages.pages[0].keys.len(), 5);
        let back = table_from_pages(&pages).unwrap();
        let mut want = table.clone();
        want.insert("enabled".into(), true.into());
        assert_eq!(back, want, "messages and back give the same table");
        assert_eq!(pages_from_table(Some(&back)).unwrap(), pages);
    }

    #[test]
    fn empty_fields_are_left_out() {
        let pages = pb::KeyPages {
            enabled: true,
            pages: vec![pb::KeyPage {
                title: " Keys ".into(),
                keys: vec![pb::KeySlot {
                    key: 3,
                    action: Field::some(pb::KeyAction {
                        kind: Some(
                            pb::HttpAction {
                                url: "http://x".into(),
                                ..Default::default()
                            }
                            .into(),
                        ),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let table = table_from_pages(&pages).unwrap();
        let want: Table = toml::from_str(
            "enabled = true\n[[page]]\ntitle = \"Keys\"\n[[page.key]]\nkey = 3\naction = { type = \"http\", url = \"http://x\" }",
        )
        .unwrap();
        assert_eq!(table, want);
    }

    #[test]
    fn explains_bad_input() {
        let page = |action: pb::KeyAction| pb::KeyPages {
            pages: vec![pb::KeyPage {
                title: "x".into(),
                keys: vec![pb::KeySlot {
                    key: 1,
                    action: Field::some(action),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = |action| format!("{:#}", table_from_pages(&page(action)).unwrap_err());
        assert!(err(pb::KeyAction::default()).contains("choose an action type"));
        let ha = |data: &str| pb::KeyAction {
            kind: Some(
                pb::HomeAssistantAction {
                    service: "light.toggle".into(),
                    data_json: data.into(),
                    ..Default::default()
                }
                .into(),
            ),
            ..Default::default()
        };
        assert!(
            err(ha("{nope")).contains("not valid JSON"),
            "{}",
            err(ha("{nope"))
        );
        assert!(err(ha("[1]")).contains("JSON object"));
        assert!(err(ha("{\"a\": null}")).contains("null"));
    }
}

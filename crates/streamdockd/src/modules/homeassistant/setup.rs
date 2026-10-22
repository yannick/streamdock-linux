//! Compact setup replies: the full Home Assistant registries are transient, not application state.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

#[derive(Default)]
pub enum Selection {
    #[default]
    All,
    Lights,
    Pinned(Vec<String>),
}

impl Selection {
    fn includes(&self, id: &Value) -> bool {
        match self {
            Self::All => true,
            Self::Lights => id.as_str().is_some_and(|id| id.starts_with("light.")),
            Self::Pinned(ids) => id.as_str().is_some_and(|id| ids.iter().any(|pin| pin == id)),
        }
    }
}

// Values for consumed fields retain the old tolerance for nulls and unusual value types;
// serde skips everything else without allocating a JSON tree for it.
#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Area {
    area_id: Value,
    name: Value,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Device {
    id: Value,
    area_id: Value,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct RegistryEntry {
    entity_id: Value,
    name: Value,
    original_name: Value,
    area_id: Value,
    device_id: Value,
    disabled_by: Value,
    hidden_by: Value,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Attributes {
    friendly_name: Value,
    current_position: Value,
    brightness: Value,
    color_temp_kelvin: Value,
    min_color_temp_kelvin: Value,
    max_color_temp_kelvin: Value,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct LiveState {
    entity_id: Value,
    state: Value,
    attributes: Option<Attributes>,
}

#[derive(Deserialize, Serialize)]
struct Reply<T> {
    #[serde(rename = "type", default)]
    kind: Value,
    #[serde(default)]
    id: Value,
    #[serde(default)]
    success: Value,
    #[serde(default)]
    error: Value,
    #[serde(default)]
    result: Vec<T>,
}

fn compact<T: DeserializeOwned + Serialize + Default>(
    text: &str,
    keep: impl Fn(&T) -> bool,
) -> serde_json::Result<Value> {
    let mut reply: Reply<T> = serde_json::from_str(text)?;
    if reply.kind != "result" || reply.success != true {
        return serde_json::from_str(text);
    }
    reply.result.retain(keep);
    serde_json::to_value(reply)
}

pub fn parse(text: &str, request: &str, selection: &Selection) -> serde_json::Result<Value> {
    let parsed = match request {
        "config/area_registry/list" => compact::<Area>(text, |_| true),
        "config/device_registry/list" => compact::<Device>(text, |_| true),
        "config/entity_registry/list" => compact::<RegistryEntry>(text, |e| selection.includes(&e.entity_id)),
        "get_states" => compact::<LiveState>(text, |s| selection.includes(&s.entity_id)),
        _ => return serde_json::from_str(text),
    };
    // Preserve the existing handling of unexpected server shapes rather than rejecting a setup
    // that the dynamic parser accepted. Normal replies take the compact path.
    parsed.or_else(|_| serde_json::from_str(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reply(result: &Value) -> String {
        json!({"type":"result", "id":4, "success":true, "result":result}).to_string()
    }

    #[test]
    fn setup_keeps_only_selected_states_and_consumed_attributes() {
        let text = reply(&json!([
            {"entity_id":"sensor.noise", "state":"42", "attributes":{"history": "x".repeat(10000)}},
            {"entity_id":"light.desk", "state":"on", "attributes":{
                "friendly_name":"Desk", "brightness":128, "color_temp_kelvin":2700.0,
                "min_color_temp_kelvin":2000, "max_color_temp_kelvin":6500, "history":"unused"
            }}
        ]));
        let parsed = parse(&text, "get_states", &Selection::Lights).unwrap();
        assert_eq!(parsed["result"].as_array().unwrap().len(), 1);
        let state = &parsed["result"][0];
        assert_eq!(state["entity_id"], "light.desk");
        assert_eq!(state["attributes"]["brightness"], 128);
        assert_eq!(state["attributes"]["color_temp_kelvin"], 2700.0);
        assert!(state["attributes"].get("history").is_none());
    }

    #[test]
    fn pinned_cover_setup_keeps_position() {
        let text = reply(&json!([{"entity_id":"cover.test", "state":"opening",
            "attributes":{"current_position":42, "history":"unused"}}]));
        let parsed = parse(&text, "get_states", &Selection::Pinned(vec!["cover.test".into()])).unwrap();
        assert_eq!(parsed["result"][0]["attributes"]["current_position"], 42);
        assert!(parsed["result"][0]["attributes"].get("history").is_none());
    }

    #[test]
    fn pinned_tabs_keep_non_light_entities_and_picker_keeps_all() {
        let text = reply(&json!([
            {"entity_id":"sensor.custom", "state":"42", "attributes":null},
            {"entity_id":"light.desk", "state":"off", "attributes":{}}
        ]));
        let parsed = parse(
            &text,
            "get_states",
            &Selection::Pinned(vec!["sensor.custom".into()]),
        )
        .unwrap();
        assert_eq!(parsed["result"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["result"][0]["entity_id"], "sensor.custom");
        assert_eq!(
            parse(&text, "get_states", &Selection::All).unwrap()["result"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn entity_registry_preserves_visibility_names_and_area_links() {
        let row = json!({"entity_id":"light.desk", "name":null, "original_name":"Desk",
            "area_id":"office", "device_id":"bridge", "disabled_by":null, "hidden_by":"user"});
        let mut extra = row.clone();
        extra["aliases"] = json!(["unused"]);
        let parsed = parse(
            &reply(&json!([extra])),
            "config/entity_registry/list",
            &Selection::Lights,
        )
        .unwrap();
        assert_eq!(parsed["result"][0], row);
    }

    #[test]
    fn unexpected_shapes_and_protocol_errors_keep_existing_semantics() {
        for response in [
            json!({"type":"result", "id":4, "success":false, "error":{"message":"denied"}}),
            json!({"type":"result", "id":4, "success":true, "result":[42]}),
            json!({"type":"event", "event":{"data":{"value":12}}}),
        ] {
            assert_eq!(
                parse(&response.to_string(), "get_states", &Selection::Lights).unwrap(),
                response
            );
        }
    }
}

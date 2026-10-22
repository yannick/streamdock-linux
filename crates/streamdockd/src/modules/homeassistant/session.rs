//! One connection to the Home Assistant WebSocket API: authenticate, load the area's lights,
//! follow `state_changed`, send `light.*` service calls.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use super::Config;
use super::lights::{Call, KnobFn, Lights, lights_in_area, pinned};
use super::setup::{self, Selection};
use crate::modules::{Input, ModuleEvent, ModuleIo};

/// Knob turns within this window become one service call.
const BATCH: Duration = Duration::from_millis(150);
/// Application-level ping, which also keeps proxies from closing an idle connection.
const PING: Duration = Duration::from_secs(30);
/// How long the setup requests may take.
const SETUP_TIMEOUT: Duration = Duration::from_secs(15);

/// Why a session ended without an error.
#[derive(Debug, PartialEq, Eq)]
pub enum End {
    /// The daemon is stopping (the module's input channel closed).
    Stopping,
    /// The connection worked and was lost later; reconnect with a short backoff.
    Connected,
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Conn {
    ws: Ws,
    next_id: u64,
    selection: Selection,
}

impl Conn {
    async fn send(&mut self, mut msg: Value) -> anyhow::Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        msg["id"] = id.into();
        self.ws.send(Message::text(msg.to_string())).await?;
        Ok(id)
    }

    async fn recv_text(&mut self) -> anyhow::Result<tokio_tungstenite::tungstenite::Utf8Bytes> {
        loop {
            match self
                .ws
                .next()
                .await
                .ok_or_else(|| anyhow!("connection closed"))??
            {
                Message::Text(text) => return Ok(text),
                Message::Close(_) => bail!("connection closed by Home Assistant"),
                _ => {}
            }
        }
    }

    async fn recv(&mut self) -> anyhow::Result<Value> {
        Ok(serde_json::from_str(&self.recv_text().await?)?)
    }

    /// Sends a command and returns its `result`, skipping anything else (setup only).
    async fn request(&mut self, msg: Value) -> anyhow::Result<Value> {
        let what = msg["type"].as_str().unwrap_or("request").to_owned();
        let id = self.send(msg).await?;
        loop {
            let text = self.recv_text().await?;
            let mut reply = setup::parse(&text, &what, &self.selection)?;
            if reply["type"] == "result" && reply["id"] == id {
                if reply["success"] == true {
                    return Ok(reply["result"].take());
                }
                bail!("{what} failed: {}", reply["error"]);
            }
        }
    }

    /// Sends a service call; returns its request id (the result arrives later as a message).
    async fn call(&mut self, call: &Call) -> anyhow::Result<u64> {
        tracing::debug!(domain = %call.domain, service = call.service, entities = ?call.entities, data = %call.data, "Home Assistant call");
        self.send(json!({
            "type": "call_service",
            "domain": call.domain,
            "service": call.service,
            "service_data": call.data,
            "target": { "entity_id": call.entities },
        }))
        .await
    }
}

async fn connect(config: &Config, token: &str) -> anyhow::Result<Conn> {
    let (ws, _) = tokio_tungstenite::connect_async(config.websocket_url())
        .await
        .with_context(|| format!("connecting to {}", config.websocket_url()))?;
    let mut conn = Conn {
        ws,
        next_id: 1,
        selection: Selection::All,
    };
    let hello = conn.recv().await?;
    if hello["type"] != "auth_required" {
        bail!("unexpected greeting from Home Assistant: {}", hello["type"]);
    }
    conn.ws
        .send(Message::text(
            json!({ "type": "auth", "access_token": token }).to_string(),
        ))
        .await?;
    match conn.recv().await?["type"].as_str() {
        Some("auth_ok") => Ok(conn),
        Some("auth_invalid") => bail!("auth rejected: the token is invalid or expired"),
        other => bail!("unexpected auth reply {other:?}"),
    }
}

/// The names of all areas, for the settings screen's area suggestions.
pub async fn area_names(config: &Config, token: &str) -> anyhow::Result<Vec<String>> {
    tokio::time::timeout(SETUP_TIMEOUT, async {
        let mut conn = connect(config, token).await?;
        let areas = conn
            .request(json!({ "type": "config/area_registry/list" }))
            .await?;
        let mut names: Vec<String> = areas
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a["name"].as_str().map(str::to_owned))
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        let _ = conn.ws.close(None).await;
        Ok(names)
    })
    .await
    .context("Home Assistant did not answer in time")?
}

/// An entity that can go on a key, for the UI's entity picker (H15-32).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityInfo {
    pub entity_id: String,
    pub name: String,
    pub area: String,
}

/// Domains whose entities make sense on a key.
pub const KEY_DOMAINS: [&str; 9] = [
    "light",
    "switch",
    "fan",
    "input_boolean",
    "scene",
    "script",
    "button",
    "input_button",
    "cover",
];

/// Registries and states in one go: `(areas, devices, entities, states)`.
async fn snapshot(config: &Config, token: &str) -> anyhow::Result<(Value, Value, Value, Value)> {
    tokio::time::timeout(SETUP_TIMEOUT, async {
        let mut conn = connect(config, token).await?;
        let areas = conn
            .request(json!({ "type": "config/area_registry/list" }))
            .await?;
        let devices = conn
            .request(json!({ "type": "config/device_registry/list" }))
            .await?;
        let entities = conn
            .request(json!({ "type": "config/entity_registry/list" }))
            .await?;
        let states = conn.request(json!({ "type": "get_states" })).await?;
        let _ = conn.ws.close(None).await;
        Ok((areas, devices, entities, states))
    })
    .await
    .context("Home Assistant did not answer in time")?
}

/// What a tab shows right now, as `(key, entity id)`: the pinned entities, or the area's lights.
pub async fn tab_entities(config: &Config, token: &str) -> anyhow::Result<Vec<(u8, String)>> {
    let lights = if config.pinned {
        let (_, _, entities, states) = snapshot(config, token).await?;
        pinned(&config.pins, &entities, &states)
    } else {
        let (areas, devices, entities, states) = snapshot(config, token).await?;
        lights_in_area(&config.area, &areas, &devices, &entities, &states).map_err(|e| anyhow!(e))?
    };
    Ok(lights.lights.into_iter().map(|l| (l.key, l.entity_id)).collect())
}

/// Every enabled, visible entity of the [`KEY_DOMAINS`], sorted by area and name.
pub async fn key_entities(config: &Config, token: &str) -> anyhow::Result<Vec<EntityInfo>> {
    let (areas, devices, entities, states) = snapshot(config, token).await?;
    Ok(entity_infos(&areas, &devices, &entities, &states))
}

/// [`key_entities`] without I/O. Lookups go through maps: a large installation has thousands of
/// states and registry entries.
fn entity_infos(areas: &Value, devices: &Value, entities: &Value, states: &Value) -> Vec<EntityInfo> {
    use std::collections::HashMap;
    let area_names: HashMap<&str, &str> = areas
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some((a["area_id"].as_str()?, a["name"].as_str()?)))
        .collect();
    let device_areas: HashMap<&str, &str> = devices
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| Some((d["id"].as_str()?, d["area_id"].as_str()?)))
        .collect();
    let registry: HashMap<&str, &Value> = entities
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| Some((e["entity_id"].as_str()?, e)))
        .collect();
    let mut out: Vec<EntityInfo> = states
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let id = s["entity_id"].as_str()?;
            let domain = id.split('.').next()?;
            if !KEY_DOMAINS.contains(&domain) {
                return None;
            }
            let entry = registry.get(id).copied();
            if entry.is_some_and(|e| !e["disabled_by"].is_null() || !e["hidden_by"].is_null()) {
                return None;
            }
            let area = entry
                .and_then(|e| {
                    e["area_id"]
                        .as_str()
                        .or_else(|| e["device_id"].as_str().and_then(|d| device_areas.get(d).copied()))
                })
                .and_then(|a| area_names.get(a).copied())
                .unwrap_or_default()
                .to_owned();
            let name = entry
                .and_then(|e| e["name"].as_str())
                .or_else(|| s["attributes"]["friendly_name"].as_str())
                .unwrap_or(id)
                .to_owned();
            Some(EntityInfo {
                entity_id: id.to_owned(),
                name,
                area,
            })
        })
        .collect();
    // Entities with an area first (by area, then name); the rest after.
    out.sort_by_cached_key(|e| (e.area.is_empty(), e.area.to_lowercase(), e.name.to_lowercase()));
    out
}

/// Runs one session until the connection drops (`Ok(Connected)`) or the daemon stops.
pub async fn run(config: &Config, token: &str, io: &mut ModuleIo) -> anyhow::Result<End> {
    let (mut conn, mut lights) = tokio::time::timeout(SETUP_TIMEOUT, async {
        let mut conn = connect(config, token).await?;
        conn.selection = if config.pinned {
            Selection::Pinned(config.pins.iter().map(|pin| pin.entity.clone()).collect())
        } else {
            Selection::Lights
        };
        let areas = conn
            .request(json!({ "type": "config/area_registry/list" }))
            .await?;
        let devices = conn
            .request(json!({ "type": "config/device_registry/list" }))
            .await?;
        let entities = conn
            .request(json!({ "type": "config/entity_registry/list" }))
            .await?;
        let states = conn.request(json!({ "type": "get_states" })).await?;
        let mut lights = if config.pinned {
            pinned(&config.pins, &entities, &states)
        } else {
            lights_in_area(&config.area, &areas, &devices, &entities, &states).map_err(|e| anyhow!(e))?
        };
        lights.knobs = config.knobs;
        lights.levels.clone_from(&config.levels);
        conn.request(json!({ "type": "subscribe_events", "event_type": "state_changed" }))
            .await?;
        anyhow::Ok((conn, lights))
    })
    .await
    .context("Home Assistant setup timed out")??;

    let names: Vec<_> = lights.lights.iter().map(|l| l.name.as_str()).collect();
    tracing::info!(tab = %config.id, area = %config.area, lights = ?names, "Home Assistant connected");
    let place = if config.pinned {
        String::new()
    } else {
        format!(" in {}", config.area)
    };
    io.activity.ok(format!("connected: {}{place}", names.join(", ")));
    io.page.send_replace(lights.page());
    // Calls sent but not answered yet: request id → what the activity feed says about it.
    let mut pending: HashMap<u64, String> = HashMap::new();

    let mut dim = 0;
    let mut warm = 0;
    let mut flush_at: Option<Instant> = None;
    let mut ping = tokio::time::interval(PING);
    ping.tick().await;
    loop {
        tokio::select! {
            msg = conn.recv() => {
                let Ok(msg) = msg else { return Ok(End::Connected) };
                on_message(&msg, &mut lights, &mut pending, io);
            }
            event = io.events.recv() => match event {
                None => return Ok(End::Stopping),
                Some(ModuleEvent::Input(Input::KnobTurn { knob, delta })) => {
                    let function = lights.knob(knob);
                    match function {
                        KnobFn::Brightness => dim += delta,
                        KnobFn::ColorTemp => warm += delta,
                        KnobFn::None => continue,
                    }
                    flush_at.get_or_insert_with(|| Instant::now() + BATCH);
                    // Show the new value at once (H15-97); Home Assistant's state follows.
                    if !lights.levels.is_empty() {
                        lights.preview_turn(function, delta, std::time::Instant::now());
                        io.page.send_replace(lights.page());
                    }
                }
                Some(ModuleEvent::Input(input)) => {
                    for call in lights.on_input(input) {
                        let id = conn.call(&call).await?;
                        pending.insert(id, describe(&call, &lights));
                    }
                }
                Some(ModuleEvent::Activated) => {
                    io.page.send_replace(lights.page());
                }
                Some(ModuleEvent::Deactivated) => {}
            },
            () = sleep_until(flush_at) => {
                flush_at = None;
                for call in lights.dim(std::mem::take(&mut dim)).into_iter().chain(lights.warm(std::mem::take(&mut warm))) {
                    let id = conn.call(&call).await?;
                    pending.insert(id, describe(&call, &lights));
                }
            }
            _ = ping.tick() => {
                conn.send(json!({ "type": "ping" })).await?;
            }
            // A knob turn's own value ends: show Home Assistant's state again.
            () = sleep_until(lights.preview_ends(std::time::Instant::now()).map(Instant::from_std)) => {
                lights.preview = None;
                io.page.send_replace(lights.page());
            }
        }
    }
}

/// What the activity feed says about a call, e.g. "toggle Deckenspots", "brightness -15 % Desk".
fn describe(call: &Call, lights: &Lights) -> String {
    let names: Vec<_> = call
        .entities
        .iter()
        .map(|id| {
            lights
                .lights
                .iter()
                .find(|l| &l.entity_id == id)
                .map_or(id.as_str(), |l| l.name.as_str())
        })
        .collect();
    let what = if let Some(step) = call.data["brightness_step_pct"].as_i64() {
        format!("brightness {step:+} %")
    } else if let Some(pct) = call.data["brightness_pct"].as_i64() {
        format!("on at {pct} %")
    } else if let Some(kelvin) = call.data["color_temp_kelvin"].as_i64() {
        format!("color temp {kelvin} K")
    } else {
        match call.service.as_str() {
            "turn_on" => "on".to_owned(),
            "turn_off" => "off".to_owned(),
            other => other.to_owned(),
        }
    };
    format!("{what} {}", names.join(", "))
}

fn on_message(msg: &Value, lights: &mut Lights, pending: &mut HashMap<u64, String>, io: &ModuleIo) {
    match msg["type"].as_str() {
        Some("event") => {
            let data = &msg["event"]["data"];
            if let Some(id) = data["entity_id"].as_str()
                && !data["new_state"].is_null()
                && lights.update(id, &data["new_state"])
            {
                io.page.send_replace(lights.page());
            }
        }
        Some("result") => {
            let text = msg["id"].as_u64().and_then(|id| pending.remove(&id));
            if msg["success"] == false {
                let reason = msg["error"]["message"].as_str().unwrap_or("rejected");
                tracing::warn!(error = %msg["error"], "Home Assistant rejected a call");
                io.activity
                    .error(format!("{}: {reason}", text.as_deref().unwrap_or("call")));
            } else if let Some(text) = text {
                io.activity.ok(text);
            }
        }
        _ => {}
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
    use tokio::sync::{mpsc, watch};

    use super::super::lights::tests::fixture;
    use super::*;
    use crate::modules::{FaceState, Page};

    /// A fake Home Assistant: auth, registries and states from the fixture, records service
    /// calls, and pushes a `state_changed` for every toggle it receives.
    async fn fake_ha(calls: mpsc::UnboundedSender<Value>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let send = |v: Value| Message::text(v.to_string());
            ws.send(send(json!({ "type": "auth_required" }))).await.unwrap();
            let (areas, devices, entities, mut states) = fixture();
            states.as_array_mut().unwrap().push(
                json!({"entity_id":"cover.test", "state":"closed", "attributes":{"current_position":0}}),
            );
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let msg: Value = serde_json::from_str(&text).unwrap();
                let id = msg["id"].clone();
                let result =
                    |r: Value| send(json!({ "id": id, "type": "result", "success": true, "result": r }));
                let reply = match msg["type"].as_str().unwrap() {
                    "auth" if msg["access_token"] == "good-token" => send(json!({ "type": "auth_ok" })),
                    "auth" => send(json!({ "type": "auth_invalid" })),
                    "config/area_registry/list" => result(areas.clone()),
                    "config/device_registry/list" => result(devices.clone()),
                    "config/entity_registry/list" => result(entities.clone()),
                    "get_states" => result(states.clone()),
                    "subscribe_events" | "ping" => result(Value::Null),
                    "call_service" => {
                        let toggled = msg["target"]["entity_id"][0].as_str().unwrap_or("").to_owned();
                        calls.send(msg.clone()).unwrap();
                        ws.send(result(Value::Null)).await.unwrap();
                        if msg["service"] == "toggle" {
                            send(json!({ "type": "event", "event": { "data": {
                                "entity_id": toggled,
                                "new_state": { "state": "on", "attributes": { "brightness": 255 } }
                            } } }))
                        } else if msg["service"] == "turn_off"
                            || msg["service_data"]["brightness_pct"].is_i64()
                        {
                            // Switching off, or on at a brightness: every entity follows.
                            let mut follow = futures_util::stream::iter(
                                state_events(&msg).into_iter().map(|e| Ok(send(e))),
                            );
                            ws.send_all(&mut follow).await.unwrap();
                            continue;
                        } else if msg["service"] == "open_cover" {
                            send(json!({"type":"event", "event":{"data":{
                                "entity_id":toggled, "new_state":{"state":"opening", "attributes":{"current_position":42}}
                            }}}))
                        } else {
                            continue;
                        }
                    }
                    other => panic!("unexpected request {other}"),
                };
                ws.send(reply).await.unwrap();
            }
        });
        format!("http://{addr}")
    }

    /// The `state_changed` events the fake sends for a `turn_off` or a `turn_on` at `brightness_pct`.
    fn state_events(msg: &Value) -> Vec<Value> {
        let state = match msg["service_data"]["brightness_pct"].as_i64() {
            Some(p) => json!({ "state": "on", "attributes": { "brightness": p * 255 / 100 } }),
            None => json!({ "state": "off", "attributes": {} }),
        };
        msg["target"]["entity_id"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entity| json!({ "type": "event", "event": { "data": { "entity_id": entity, "new_state": state } } }))
            .collect()
    }

    fn config(url: &str) -> Config {
        Config::for_area(url, "/unused".into(), "gaeste buero")
    }

    async fn page_where(rx: &mut watch::Receiver<Page>, pred: impl Fn(&Page) -> bool) -> Page {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if pred(&rx.borrow_and_update()) {
                    return rx.borrow().clone();
                }
                rx.changed().await.unwrap();
            }
        })
        .await
        .expect("timed out waiting for a page")
    }

    #[tokio::test]
    async fn lights_show_toggle_and_follow_state_changes() {
        let (calls_tx, mut calls) = mpsc::unbounded_channel();
        let url = fake_ha(calls_tx).await;
        let (events_tx, events) = mpsc::channel(16);
        let (page_tx, mut page) = watch::channel(Page::default());
        let hub = crate::hub::Hub::new();
        let activity = crate::modules::Activity::new(std::sync::Arc::clone(&hub), "homeassistant");
        let session = tokio::spawn(async move {
            let mut io = ModuleIo {
                events,
                page: page_tx,
                activity,
            };
            run(&config(&url), "good-token", &mut io).await
        });

        let p = page_where(&mut page, |p| p.faces.len() == 3).await;
        assert_eq!(p.faces[2].label, "Strip");
        assert_eq!(p.faces[2].state.as_known(), Some(FaceState::FACE_STATE_IDLE));

        // Key 3 toggles the strip; HA's state_changed turns its key on.
        events_tx
            .send(ModuleEvent::Input(Input::Key {
                key: 3,
                pressed: true,
            }))
            .await
            .unwrap();
        let call = calls.recv().await.unwrap();
        assert_eq!(
            (call["service"].as_str(), call["target"]["entity_id"][0].as_str()),
            (Some("toggle"), Some("light.strip"))
        );
        page_where(&mut page, |p| p.faces[2].detail == "100 %").await;
        // The activity feed got the connection and the toggle's result.
        let texts: Vec<_> = hub
            .subscribe()
            .activity
            .into_iter()
            .map(|a| (a.level.as_known(), a.text))
            .collect();
        assert!(
            texts
                .iter()
                .any(|(_, t)| t.starts_with("connected: Ceiling, Desk, Strip")),
            "{texts:?}"
        );
        assert!(
            texts.contains(&(
                Some(streamdock_proto::streamdock::v1::ActivityLevel::ACTIVITY_LEVEL_OK),
                "toggle Strip".to_owned()
            )),
            "{texts:?}"
        );

        // Three knob-1 detents become one batched brightness call for the lights that are on.
        for _ in 0..3 {
            events_tx
                .send(ModuleEvent::Input(Input::KnobTurn { knob: 1, delta: 1 }))
                .await
                .unwrap();
        }
        let call = calls.recv().await.unwrap();
        assert_eq!(call["service_data"], json!({ "brightness_step_pct": 15 }));
        assert_eq!(call["target"]["entity_id"], json!(["light.desk", "light.strip"]));

        drop(events_tx);
        assert_eq!(session.await.unwrap().unwrap(), End::Stopping);
    }

    #[tokio::test]
    async fn dimmer_switches_the_lights_on_from_the_bottom_and_off_at_zero() {
        let (calls_tx, mut calls) = mpsc::unbounded_channel();
        let url = fake_ha(calls_tx).await;
        let (events_tx, events) = mpsc::channel(16);
        let (page_tx, mut page) = watch::channel(Page::default());
        let hub = crate::hub::Hub::new();
        let activity = crate::modules::Activity::new(std::sync::Arc::clone(&hub), "homeassistant");
        let session = tokio::spawn(async move {
            let mut io = ModuleIo {
                events,
                page: page_tx,
                activity,
            };
            run(&config(&url), "good-token", &mut io).await
        });
        page_where(&mut page, |p| p.faces.len() == 3).await;
        let send = |input| {
            let tx = events_tx.clone();
            async move { tx.send(ModuleEvent::Input(input)).await.unwrap() }
        };

        // Knob 1 pressed: all off (the fake reports them off).
        send(Input::KnobPress {
            knob: 1,
            pressed: true,
        })
        .await;
        assert_eq!(calls.recv().await.unwrap()["service"], "turn_off");
        page_where(&mut page, |p| p.faces[1].detail == "off").await;

        // Turning the dimmer right switches them on at 1 %, further detents add to it.
        send(Input::KnobTurn { knob: 1, delta: 1 }).await;
        let on = calls.recv().await.unwrap();
        assert_eq!(
            (on["service"].as_str(), &on["service_data"]),
            (Some("turn_on"), &json!({ "brightness_pct": 1 }))
        );
        assert_eq!(on["target"]["entity_id"], json!(["light.desk", "light.strip"]));
        page_where(&mut page, |p| p.faces[1].detail == "1 %").await;

        // Turning left to 0 switches them off again.
        send(Input::KnobTurn { knob: 1, delta: -1 }).await;
        let off = calls.recv().await.unwrap();
        assert_eq!(off["service"], "turn_off");
        assert_eq!(off["target"]["entity_id"], json!(["light.desk", "light.strip"]));

        drop(events_tx);
        assert_eq!(session.await.unwrap().unwrap(), End::Stopping);
    }

    #[tokio::test]
    async fn cover_keys_send_scoped_services_and_follow_fake_ha_motion() {
        let (calls_tx, mut calls) = mpsc::unbounded_channel();
        let url = fake_ha(calls_tx).await;
        let mut config = config(&url);
        config.pinned = true;
        config.pins = [(4, "open"), (5, "stop"), (6, "close")]
            .into_iter()
            .map(|(key, action)| super::super::Pin {
                key,
                entity: "cover.test".into(),
                action: action.into(),
                ..super::super::Pin::default()
            })
            .collect();
        let (events_tx, events) = mpsc::channel(16);
        let (page_tx, mut page) = watch::channel(Page::default());
        let activity = crate::modules::Activity::new(crate::hub::Hub::new(), "homeassistant");
        let session = tokio::spawn(async move {
            let mut io = ModuleIo {
                events,
                page: page_tx,
                activity,
            };
            run(&config, "good-token", &mut io).await
        });
        let initial = page_where(&mut page, |p| p.faces.len() == 3).await;
        assert!(initial.faces.iter().all(|f| f.detail == "0 % · closed"));
        events_tx
            .send(ModuleEvent::Input(Input::Key {
                key: 4,
                pressed: true,
            }))
            .await
            .unwrap();
        let call = calls.recv().await.unwrap();
        assert_eq!(call["domain"], "cover");
        assert_eq!(call["service"], "open_cover");
        assert_eq!(call["target"], json!({"entity_id":["cover.test"]}));
        assert_eq!(call["service_data"], json!({}));
        let moving = page_where(&mut page, |p| {
            p.faces.iter().all(|f| f.detail == "42 % · opening")
        })
        .await;
        assert_eq!(moving.faces[0].state.as_known(), Some(FaceState::FACE_STATE_ON));
        for key in [4, 5, 6] {
            events_tx
                .send(ModuleEvent::Input(Input::Key { key, pressed: true }))
                .await
                .unwrap();
            let call = calls.recv().await.unwrap();
            assert_eq!(call["service"], "stop_cover");
            assert_eq!(call["target"], json!({"entity_id":["cover.test"]}));
        }
        drop(events_tx);
        assert_eq!(session.await.unwrap().unwrap(), End::Stopping);
    }

    #[tokio::test]
    async fn bad_token_is_reported() {
        let (calls_tx, _calls) = mpsc::unbounded_channel();
        let url = fake_ha(calls_tx).await;
        let (_events_tx, events) = mpsc::channel(1);
        let (page_tx, _page) = watch::channel(Page::default());
        let mut io = ModuleIo {
            events,
            page: page_tx,
            activity: crate::modules::Activity::disabled(),
        };
        let err = run(&config(&url), "wrong", &mut io)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("auth rejected"), "{err}");
    }
}

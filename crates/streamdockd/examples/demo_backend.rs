//! A fake Home Assistant and Lyrion Music Server with neutral demo data, for screenshots and UI
//! work without a real house (H15-102). Nothing here reaches a real device.
//!
//! ```sh
//! cargo run -p streamdockd --example demo_backend   # HA on :18123, Lyrion on :19000
//! just demo                                          # the backend plus a daemon on :7399
//! ```
//!
//! Home Assistant: the WebSocket API (`/api/websocket`; any token is accepted) with two areas
//! ("Living room", "Kids room"), lights with brightness and colour temperature, and blinds.
//! Service calls change the state and are pushed back as `state_changed`, so the keys react.
//! Lyrion: `POST /jsonrpc.js` with three players, `_dock` playlists and a playing track.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use axum::Json;
use axum::extract::State;
use axum::routing::post;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

const HA_ADDR: &str = "127.0.0.1:18123";
const LYRION_ADDR: &str = "127.0.0.1:19000";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let ha = Arc::new(Ha::new());
    let lyrion = Arc::new(Mutex::new(Lyrion::new()));
    let ha_listener = TcpListener::bind(HA_ADDR).await?;
    let app = axum::Router::new()
        .route("/jsonrpc.js", post(jsonrpc))
        .with_state(lyrion);
    let lyrion_listener = TcpListener::bind(LYRION_ADDR).await?;
    eprintln!("demo Home Assistant on http://{HA_ADDR}, demo Lyrion on http://{LYRION_ADDR}");
    tokio::spawn(async move { axum::serve(lyrion_listener, app).await });
    loop {
        let (stream, _) = ha_listener.accept().await?;
        let ha = ha.clone();
        tokio::spawn(async move {
            if let Err(e) = ha.serve(stream).await {
                eprintln!("demo HA connection ended: {e}");
            }
        });
    }
}

/// The demo house: registries plus live states, shared by every connection.
struct Ha {
    areas: Value,
    entities: Value,
    states: Mutex<HashMap<String, Value>>,
    changes: broadcast::Sender<Value>,
}

impl Ha {
    fn new() -> Self {
        let areas = json!([
            { "area_id": "living_room", "name": "Living room" },
            { "area_id": "kids_room", "name": "Kids room" }
        ]);
        let entity = |id: &str, area: &str, name: &str| json!({ "entity_id": id, "area_id": area, "device_id": null, "disabled_by": null, "hidden_by": null, "name": name });
        let entities = json!([
            entity("light.living_ceiling", "living_room", "Ceiling"),
            entity("light.living_floor_lamp", "living_room", "Floor lamp"),
            entity("cover.living_blind", "living_room", "Blind"),
            entity("light.kids_ceiling", "kids_room", "Ceiling"),
            entity("cover.kids_blind", "kids_room", "Blind")
        ]);
        let light = |name: &str, on: bool, brightness: u8, kelvin: u32| {
            json!({ "state": if on { "on" } else { "off" }, "attributes": {
                "friendly_name": name, "brightness": brightness, "color_temp_kelvin": kelvin,
                "min_color_temp_kelvin": 2200, "max_color_temp_kelvin": 6500,
                "supported_color_modes": ["color_temp"], "color_mode": "color_temp" } })
        };
        let cover = |name: &str, position: u8| {
            json!({ "state": if position > 0 { "open" } else { "closed" },
                    "attributes": { "friendly_name": name, "current_position": position, "supported_features": 15 } })
        };
        let states = HashMap::from([
            (
                "light.living_ceiling".to_owned(),
                light("Living room Ceiling", true, 178, 3000),
            ),
            (
                "light.living_floor_lamp".to_owned(),
                light("Living room Floor lamp", false, 120, 2700),
            ),
            ("cover.living_blind".to_owned(), cover("Living room Blind", 60)),
            (
                "light.kids_ceiling".to_owned(),
                light("Kids room Ceiling", true, 140, 3500),
            ),
            ("cover.kids_blind".to_owned(), cover("Kids room Blind", 100)),
        ]);
        Self {
            areas,
            entities,
            states: Mutex::new(states),
            changes: broadcast::channel(64).0,
        }
    }

    fn states(&self) -> std::sync::MutexGuard<'_, HashMap<String, Value>> {
        self.states.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn all_states(&self) -> Value {
        let states = self.states();
        Value::Array(
            states
                .iter()
                .map(|(id, s)| json!({ "entity_id": id, "state": s["state"], "attributes": s["attributes"] }))
                .collect(),
        )
    }

    async fn serve(&self, stream: tokio::net::TcpStream) -> anyhow::Result<()> {
        let ws = tokio_tungstenite::accept_async(stream).await?;
        let (mut tx, mut rx) = ws.split();
        let mut changes = self.changes.subscribe();
        tx.send(text(
            &json!({ "type": "auth_required", "ha_version": "2026.10.0" }),
        ))
        .await?;
        loop {
            tokio::select! {
                msg = rx.next() => {
                    let Some(Ok(Message::Text(raw))) = msg else { return Ok(()) };
                    let msg: Value = serde_json::from_str(&raw)?;
                    if let Some(reply) = self.reply(&msg) {
                        tx.send(text(&reply)).await?;
                    }
                }
                Ok(event) = changes.recv() => tx.send(text(&event)).await?,
            }
        }
    }

    fn reply(&self, msg: &Value) -> Option<Value> {
        let id = msg["id"].clone();
        let result = |r: Value| Some(json!({ "id": id, "type": "result", "success": true, "result": r }));
        match msg["type"].as_str()? {
            "auth" => Some(json!({ "type": "auth_ok", "ha_version": "2026.10.0" })),
            "config/area_registry/list" => result(self.areas.clone()),
            "config/device_registry/list" => result(json!([])),
            "config/entity_registry/list" => result(self.entities.clone()),
            "get_states" => result(self.all_states()),
            "call_service" => {
                self.call_service(msg);
                result(Value::Null)
            }
            _ => result(Value::Null),
        }
    }

    /// Applies a service call to the demo states and broadcasts the new states.
    fn call_service(&self, msg: &Value) {
        let service = msg["service"].as_str().unwrap_or_default();
        let data = &msg["service_data"];
        let targets: Vec<String> = match &msg["target"]["entity_id"] {
            Value::Array(ids) => ids.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
            Value::String(id) => vec![id.clone()],
            _ => Vec::new(),
        };
        let mut states = self.states();
        for id in targets {
            let Some(state) = states.get_mut(&id) else {
                continue;
            };
            apply(state, service, data);
            let event = json!({ "type": "event", "event": { "event_type": "state_changed", "data": {
                "entity_id": id, "new_state": { "entity_id": id, "state": state["state"], "attributes": state["attributes"] } } } });
            let _ = self.changes.send(event);
        }
    }
}

fn apply(state: &mut Value, service: &str, data: &Value) {
    let attrs = &mut state["attributes"];
    match service {
        "turn_on" => {
            state["state"] = json!("on");
            if let Some(pct) = data["brightness_pct"].as_f64() {
                state["attributes"]["brightness"] = json!((pct * 2.55).round());
            }
            if let Some(step) = data["brightness_step_pct"].as_f64() {
                let now = state["attributes"]["brightness"].as_f64().unwrap_or(0.0);
                state["attributes"]["brightness"] = json!((now + step * 2.55).clamp(3.0, 255.0).round());
            }
            if let Some(k) = data["color_temp_kelvin"].as_u64() {
                state["attributes"]["color_temp_kelvin"] = json!(k);
            }
        }
        "turn_off" => state["state"] = json!("off"),
        "toggle" => {
            let on = state["state"] == "on";
            state["state"] = json!(if on { "off" } else { "on" });
        }
        "open_cover" => {
            attrs["current_position"] = json!(100);
            state["state"] = json!("open");
        }
        "close_cover" => {
            attrs["current_position"] = json!(0);
            state["state"] = json!("closed");
        }
        _ => {}
    }
}

fn text(v: &Value) -> Message {
    Message::text(v.to_string())
}

/// The demo music server: players, playlists and what each player plays.
struct Lyrion {
    players: Vec<(&'static str, &'static str)>,
    volume: HashMap<String, i64>,
    playing: HashMap<String, bool>,
}

const PLAYLISTS: [&str; 6] = [
    "_dock Evening Jazz",
    "_dock Morning Coffee",
    "_dock Road Trip",
    "_Kids_ Bedtime Stories",
    "_Kids_ Sing Along",
    "Chores",
];

impl Lyrion {
    fn new() -> Self {
        let players = vec![
            ("00:04:20:aa:00:01", "Living room"),
            ("00:04:20:aa:00:02", "Kitchen"),
            ("00:04:20:aa:00:03", "Kids room"),
        ];
        let volume = players.iter().map(|(id, _)| ((*id).to_owned(), 35)).collect();
        let playing = players.iter().map(|(id, _)| ((*id).to_owned(), true)).collect();
        Self {
            players,
            volume,
            playing,
        }
    }

    fn request(&mut self, player: &str, cmd: &[Value]) -> Value {
        let word = |i: usize| cmd.get(i).and_then(Value::as_str).unwrap_or_default();
        match word(0) {
            "players" => {
                json!({ "count": self.players.len(), "players_loop": self.players.iter().enumerate().map(|(i, (id, name))| json!({
                "playerid": id, "name": name, "playerindex": i, "connected": 1, "power": 1, "isplaying": 1,
                "model": "squeezelite", "modelname": "SqueezeLite", "ip": format!("192.0.2.{}:3483", 10 + i)
            })).collect::<Vec<_>>() })
            }
            "playlists" => {
                json!({ "count": PLAYLISTS.len(), "playlists_loop": PLAYLISTS.iter().enumerate().map(|(i, p)| json!({ "id": 1000 + i, "playlist": p })).collect::<Vec<_>>() })
            }
            "status" => self.status(player),
            "mixer" if word(1) == "volume" => {
                let v = self.volume.entry(player.to_owned()).or_insert(35);
                match word(2) {
                    s if s.starts_with('+') || s.starts_with('-') => {
                        *v = (*v + s.parse::<i64>().unwrap_or(0)).clamp(0, 100);
                    }
                    s => *v = s.parse().unwrap_or(*v),
                }
                json!({})
            }
            "pause" | "play" => {
                let p = self.playing.entry(player.to_owned()).or_insert(true);
                *p = word(0) == "play" || !*p;
                json!({})
            }
            _ => json!({}),
        }
    }

    fn status(&self, player: &str) -> Value {
        let (title, artist, album) = match player {
            "00:04:20:aa:00:03" => ("Twinkle, Twinkle", "The Demo Choir", "Sing Along"),
            "00:04:20:aa:00:02" => ("Slow Morning", "Coffee House Trio", "Morning Coffee"),
            _ => ("Blue in Green", "The Demo Quartet", "Evening Jazz"),
        };
        let playing = self.playing.get(player).copied().unwrap_or(true);
        json!({
            "mode": if playing { "play" } else { "pause" }, "power": 1, "time": 83.0, "duration": 337.0,
            "mixer volume": self.volume.get(player).copied().unwrap_or(35),
            "playlist repeat": 0, "playlist shuffle": 0, "playlist_cur_index": "2", "playlist_tracks": 12,
            "playlist_loop": [{ "playlist index": 2, "title": title, "artist": artist, "album": album, "duration": 337.0, "id": 42 }],
            "remoteMeta": { "title": title, "artist": artist, "album": album }
        })
    }
}

async fn jsonrpc(State(lyrion): State<Arc<Mutex<Lyrion>>>, Json(body): Json<Value>) -> Json<Value> {
    let player = body["params"][0].as_str().unwrap_or_default().to_owned();
    let cmd = body["params"][1].as_array().cloned().unwrap_or_default();
    let result = lyrion
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .request(&player, &cmd);
    Json(json!({ "id": body["id"], "method": "slim.request", "params": body["params"], "result": result }))
}

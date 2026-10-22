//! What the built-in player tells Lyrion and asks it over JSON-RPC (slimproto carries neither): the
//! output's volume when it changed elsewhere, the replay gain setting, and what is playing.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::watch;

use super::{lyrion_percent, output_percent};
use crate::config::{ReplayGain, Transition};
use crate::hub::{Hub, HubEvent};
use crate::modules::lyrion::client::Client;
use streamdock_proto::streamdock::v1 as pb;

/// Lyrion's web port (JSON-RPC).
const HTTP_PORT: u16 = 9000;
/// How often the track info is read while playing.
const TRACK_EVERY: Duration = Duration::from_secs(5);
/// No volume from Lyrion yet.
pub(super) const NO_VOLUME: u32 = u32::MAX;

fn client(server: Ipv4Addr) -> anyhow::Result<Client> {
    Client::new(&format!("http://{server}:{HTTP_PORT}"))
}

/// The current track as Lyrion describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct TrackInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub bitrate: String,
    /// The file's sample rate (Lyrion's `samplerate`); 0 if unknown.
    pub sample_rate: u32,
}

impl TrackInfo {
    fn from_status(status: &Value) -> Self {
        let track = &status["playlist_loop"][0];
        let text = |key: &str| match &track[key] {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        };
        Self {
            title: text("title"),
            artist: text("artist"),
            album: text("album"),
            bitrate: text("bitrate"),
            sample_rate: text("samplerate").parse().unwrap_or(0),
        }
    }
}

/// The selected (or the player's fixed) output's volume changed elsewhere (the settings page,
/// another tab's knob): tell Lyrion, so its volume for this player stays the output's. Changes that
/// Lyrion's own `audg` made (`lyrion_volume`, ±1 for PipeWire's rounding) are not sent back.
///
/// When the output itself changes (another one selected, a headset back after a drop), Lyrion's
/// volume wins and goes to it (H15-79): a fallback output at its own 60 % must not raise Lyrion
/// to 40, and a headset that reconnects at 0 % gets the music's volume back.
pub(super) async fn volume_back(
    hub: Arc<Hub>,
    id: String,
    output: Option<String>,
    lyrion_volume: Arc<AtomicU32>,
) {
    let mut events = hub.subscribe().events;
    let mut last: Option<String> = None;
    loop {
        let audio = match events.recv().await {
            Ok(HubEvent::Audio(audio)) => audio,
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        let now = audio
            .outputs
            .iter()
            .find(|o| output.as_deref().map_or(o.selected, |n| o.name == n));
        let lyrion = lyrion_volume.load(Ordering::Relaxed);
        let action = volume_sync(last.as_deref(), now, lyrion);
        last = now.map(|o| o.name.clone());
        match action {
            VolumeSync::Nothing => {}
            VolumeSync::ToOutput(name, percent) => to_output(&hub, &name, percent),
            VolumeSync::ToLyrion(percent) => to_lyrion(&hub, &id, percent).await,
        }
    }
}

/// What a new audio state asks for, given the output seen before and Lyrion's volume.
#[derive(Debug, PartialEq, Eq)]
enum VolumeSync {
    Nothing,
    /// Another output: give it Lyrion's volume (output %).
    ToOutput(String, u32),
    /// The same output's volume changed elsewhere: tell Lyrion (Lyrion %).
    ToLyrion(u32),
}

fn volume_sync(last: Option<&str>, now: Option<&pb::AudioOutput>, lyrion: u32) -> VolumeSync {
    let Some(now) = now else {
        return VolumeSync::Nothing;
    };
    if lyrion == NO_VOLUME || output_percent(lyrion).abs_diff(now.volume_percent) <= 1 {
        return VolumeSync::Nothing;
    }
    if last != Some(now.name.as_str()) {
        return VolumeSync::ToOutput(now.name.clone(), output_percent(lyrion));
    }
    VolumeSync::ToLyrion(lyrion_percent(now.volume_percent))
}

fn to_output(hub: &Hub, name: &str, percent: u32) {
    let Some(audio) = hub.audio() else { return };
    if audio.set_player_volume(Some(name), percent) {
        tracing::info!(
            output = name,
            percent,
            "output changed: Lyrion's volume applied to it"
        );
    }
}

async fn to_lyrion(hub: &Hub, id: &str, percent: u32) {
    let Some(server) = connected_server(hub) else {
        return;
    };
    let command = json!(["mixer", "volume", percent.to_string()]);
    if let Err(e) = request(server, id, command).await {
        tracing::warn!(error = %format!("{e:#}"), "player volume not sent to Lyrion");
    }
}

fn connected_server(hub: &Hub) -> Option<Ipv4Addr> {
    let state = hub.player_state().filter(|p| p.connected)?;
    state.server.parse::<SocketAddrV4>().ok().map(|s| *s.ip())
}

async fn request(server: Ipv4Addr, id: &str, command: Value) -> anyhow::Result<Value> {
    client(server)?.request(id, command).await
}

/// Lyrion's player preferences we set on every (re)connect; `None` leaves Lyrion's own.
#[derive(Debug, Clone, Copy)]
pub(super) struct Prefs {
    pub replay_gain: Option<ReplayGain>,
    pub transition: Option<Transition>,
}

impl Prefs {
    fn commands(self) -> Vec<Value> {
        let mut commands = Vec::new();
        if let Some(gain) = self.replay_gain {
            commands.push(json!([
                "playerpref",
                "replayGainMode",
                gain.lyrion_mode().to_string()
            ]));
        }
        if let Some(t) = self.transition {
            commands.push(json!([
                "playerpref",
                "transitionType",
                t.kind.lyrion_type().to_string()
            ]));
            commands.push(json!(["playerpref", "transitionDuration", t.seconds.to_string()]));
        }
        commands
    }
}

/// Follows the client: sets the player preferences ([`Prefs`]) on every (re)connect, and reads the track
/// info whenever the playing track may have changed and every few seconds while playing.
pub(super) async fn sync(
    id: String,
    prefs: Prefs,
    mut state: watch::Receiver<slimproto::PlayerState>,
    track: watch::Sender<TrackInfo>,
) {
    let mut was_connected = false;
    let mut tick = tokio::time::interval(TRACK_EVERY);
    // Read the track info at once when this changes (a new track starts with its format), else on
    // the tick: buffer fill changes don't need a request.
    let mut seen = None;
    let mut ticked = true;
    loop {
        let current = state.borrow_and_update().clone();
        let server = current.server.filter(|_| current.connected).map(|s| *s.ip());
        if let Some(server) = server
            && !was_connected
        {
            for command in prefs.commands() {
                if let Err(e) = request(server, &id, command).await {
                    tracing::warn!(error = %format!("{e:#}"), "player preference not set in Lyrion");
                }
            }
        }
        was_connected = server.is_some();
        let key = Some((server, current.playing, current.paused, current.format.clone()));
        if ticked || key != seen {
            update_track(server.filter(|_| current.playing), &id, &track).await;
            seen = key;
        }
        ticked = tokio::select! {
            changed = state.changed() => {
                if changed.is_err() {
                    return;
                }
                false
            }
            _ = tick.tick() => true,
        };
    }
}

async fn update_track(server: Option<Ipv4Addr>, id: &str, track: &watch::Sender<TrackInfo>) {
    let info = match server {
        None => TrackInfo::default(),
        Some(server) => match request(server, id, json!(["status", "-", "1", "tags:alrT"])).await {
            Ok(status) => TrackInfo::from_status(&status),
            Err(e) => {
                tracing::debug!(error = %format!("{e:#}"), "track info not read");
                return;
            }
        },
    };
    track.send_if_modified(|t| {
        let changed = *t != info;
        *t = info;
        changed
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str, volume: u32) -> pb::AudioOutput {
        pb::AudioOutput {
            name: name.into(),
            selected: true,
            volume_percent: volume,
            ..Default::default()
        }
    }

    const XM4: &str = "bluez_output.02_00_00_00_00_B4.a2dp-sink";
    const ANALOG: &str = "alsa_output.platform-rv1106-acodec.stereo-fallback";

    #[test]
    fn a_fallback_output_gets_lyrions_volume_instead_of_raising_it() {
        // H15-78 night: the XM4 dropped, the analog output (60 %) took over, Lyrion went 3 -> 40.
        let analog = output(ANALOG, 60);
        assert_eq!(
            volume_sync(Some(XM4), Some(&analog), 3),
            VolumeSync::ToOutput(ANALOG.into(), 5)
        );
        // A headset back at 0 % gets the music's volume.
        let xm4 = output(XM4, 0);
        assert_eq!(
            volume_sync(Some(ANALOG), Some(&xm4), 50),
            VolumeSync::ToOutput(XM4.into(), 75)
        );
        assert_eq!(
            volume_sync(None, Some(&xm4), 50),
            VolumeSync::ToOutput(XM4.into(), 75),
            "after no output"
        );
    }

    #[test]
    fn the_same_outputs_volume_changed_elsewhere_goes_to_lyrion() {
        let xm4 = output(XM4, 60);
        assert_eq!(volume_sync(Some(XM4), Some(&xm4), 50), VolumeSync::ToLyrion(40));
        let echo = output(XM4, 76);
        assert_eq!(
            volume_sync(Some(XM4), Some(&echo), 50),
            VolumeSync::Nothing,
            "Lyrion's own, rounded"
        );
        assert_eq!(
            volume_sync(Some(XM4), Some(&xm4), NO_VOLUME),
            VolumeSync::Nothing,
            "Lyrion not heard yet"
        );
        assert_eq!(volume_sync(Some(XM4), None, 50), VolumeSync::Nothing);
    }

    #[test]
    fn prefs_set_only_what_is_configured() {
        let none = Prefs {
            replay_gain: None,
            transition: None,
        };
        assert!(none.commands().is_empty(), "Lyrion's own settings stay");
        let both = Prefs {
            replay_gain: Some(ReplayGain::Album),
            transition: Some(Transition {
                kind: crate::config::player::TransitionKind::Crossfade,
                seconds: 6,
            }),
        };
        assert_eq!(
            both.commands(),
            vec![
                json!(["playerpref", "replayGainMode", "2"]),
                json!(["playerpref", "transitionType", "1"]),
                json!(["playerpref", "transitionDuration", "6"]),
            ]
        );
    }

    #[test]
    fn track_info_from_lyrion_status() {
        let status = json!({
            "mode": "play",
            "playlist_loop": [{"title": "So What", "artist": "Miles Davis", "album": "Kind of Blue", "bitrate": "1411kbps", "samplerate": "44100"}]
        });
        let info = TrackInfo::from_status(&status);
        assert_eq!(info.title, "So What");
        assert_eq!(info.artist, "Miles Davis");
        assert_eq!(info.album, "Kind of Blue");
        assert_eq!(info.bitrate, "1411kbps");
        assert_eq!(info.sample_rate, 44_100);
        assert_eq!(
            TrackInfo::from_status(&json!({})),
            TrackInfo::default(),
            "nothing playing"
        );
    }
}

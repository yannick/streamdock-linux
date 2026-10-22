//! Lyrion Music Server (LMS) module (H15-17): the server's `_dock…` playlists on the LCD keys,
//! played on one fixed player.
//!
//! ```toml
//! [modules.lyrion]
//! url = "http://192.168.1.13:9000"   # LMS web address; JSON-RPC is POST <url>/jsonrpc.js
//! player = "Buero Essence"            # player name or MAC, as LMS lists it
//! prefix = "_dock"                    # optional; playlists starting with this are shown
//! title = "Music"                     # optional tab name (id "lyrion")
//! show = true                         # false: no tab; `player` is then only the default player of
//!                                     # the tabs below and of custom keys' lyrion actions
//!
//! [[modules.lyrion.tab]]              # more tabs (H15-32): another player and/or prefix
//! id = "kitchen"                      # stable id; default lyrion-2, lyrion-3 …
//! title = "Kitchen"
//! player = "Kitchen Radio"            # default: the top-level player
//! prefix = "_kitchen"                 # default: the top-level prefix
//! ```
//!
//! Layout (decided on H15-14): keys 1-6 = playlists (press = play on the player; the one we
//! started is highlighted while it plays), knob 2 (the big one) = volume (H15-95), knob 1 turn =
//! page (more than 6) or the bass (`layout = "controls"`), both presses = play/pause, buttons 7/8/9 =
//! previous / play-pause / next track.
//!
//! The server is only polled while the mode is active: player status every 2 s, the playlist
//! list every 30 s (and right after activation). While it is offline, only every 15 s.

pub(crate) mod client;
pub mod now_playing;
pub mod songpal;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use serde_json::{Value, json};

use self::client::Client;
use super::{FaceState, Input, Module, ModuleEvent, ModuleIo, ModuleTask, Page, face, knob};

const STATUS_EVERY: Duration = Duration::from_secs(2);
/// A playlist that stops within this time after we loaded it couldn't be played (e.g. its tracks
/// point to files LMS can't open: it skips through them all in seconds and stops).
const CANT_PLAY_WITHIN: Duration = Duration::from_secs(60);
/// While the server is offline, refreshes are tried only this often.
const OFFLINE_RETRY: Duration = Duration::from_secs(15);
const PLAYLISTS_EVERY: Duration = Duration::from_secs(30);
/// Volume change per knob detent, in percent.
const VOLUME_STEP: i32 = 2;
const KEYS: u8 = 6;
/// Failed refreshes in a row before the keys say "Lyrion offline" (a slow server can miss one).
const OFFLINE_AFTER: u32 = 2;

/// The player of a music tab that names none and has no top-level `player` to inherit (H15-87):
/// empty = the first player the server lists.
pub const DEFAULT_PLAYER: &str = "";

const TAB_KEYS: [&str; 11] = [
    "id",
    "title",
    "player",
    "prefix",
    "volume",
    "now_playing",
    "layout",
    "then",
    "tone",
    "volume_bar",
    "follow_headset",
];

/// How long the volume bar stays on its key after a change, with `show = "changes"` (H15-95).
const VOLUME_BAR_FOR: Duration = Duration::from_secs(3);

/// `volume_bar = { key = 3, show = "changes" }`: the volume as a filled bar plus % on a key
/// (H15-95), after each change for a few seconds (`"changes"`) or all the time (`"always"`, the
/// now-playing band then leaves that key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeBar {
    pub key: u8,
    pub always: bool,
}

impl VolumeBar {
    fn parse(table: &toml::Table, at: &str) -> anyhow::Result<Option<Self>> {
        let Some(value) = table.get("volume_bar") else {
            return Ok(None);
        };
        let t = value.as_table().with_context(|| {
            format!("{at} volume_bar must be a table, e.g. {{ key = 3, show = \"changes\" }}")
        })?;
        let key = match t.get("key") {
            None => 3,
            Some(k) => k
                .as_integer()
                .and_then(|k| u8::try_from(k).ok())
                .filter(|k| (1..=KEYS).contains(k))
                .with_context(|| format!("{at} volume_bar.key must be an LCD key 1-6"))?,
        };
        let always = match t.get("show").and_then(toml::Value::as_str) {
            None | Some("changes") => false,
            Some("always") => true,
            Some(other) => bail!("{at} volume_bar.show must be \"changes\" or \"always\", got {other:?}"),
        };
        Ok(Some(Self { key, always }))
    }
}

/// What a music tab's keys show (H15-90).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Layout {
    /// Playlists on the keys the now-playing band leaves free (the default).
    #[default]
    Playlists,
    /// The band, then ⏮ ⏯ ⏭ on the next three keys; knob 1 volume, press = play/pause.
    Controls,
}

impl Layout {
    fn parse(table: &toml::Table, at: &str) -> anyhow::Result<Option<Self>> {
        Ok(match text(table, "layout", at)?.as_deref() {
            None => None,
            Some("playlists") => Some(Self::Playlists),
            Some("controls") => Some(Self::Controls),
            Some(other) => bail!("{at} layout must be \"playlists\" or \"controls\", got {other:?}"),
        })
    }
}

fn flag(table: &toml::Table, key: &str, at: &str) -> anyhow::Result<bool> {
    table.get(key).map_or(Ok(false), |v| {
        v.as_bool()
            .with_context(|| format!("{at} {key} must be true or false"))
    })
}

fn text(table: &toml::Table, key: &str, at: &str) -> anyhow::Result<Option<String>> {
    match table.get(key) {
        None => Ok(None),
        Some(v) => Ok(Some(
            v.as_str()
                .with_context(|| format!("{at} {key} must be a string"))?
                .to_owned(),
        )),
    }
}

/// One Lyrion tab: which player plays which playlists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub id: String,
    pub title: String,
    pub player: String,
    pub prefix: String,
    /// Knob 1 turns this machine's audio output (H15-50) instead of the Lyrion player's volume.
    pub output_volume: bool,
    /// The now-playing band (H15-88).
    pub now_playing: now_playing::Config,
    pub layout: Layout,
    /// After a playlist key starts a playlist, switch to this tab (e.g. the music screen, H15-90).
    pub then: Option<String>,
    /// Knob 2 on the music screen: a band of the speaker's own equaliser (H15-90).
    pub tone: Option<songpal::Config>,
    /// The volume as a bar on a key (H15-95).
    pub volume_bar: Option<VolumeBar>,
    /// While headset mode has the music on this machine's built-in player (H15-91), the tab
    /// controls that player instead (H15-95).
    pub follow_headset: bool,
}

/// `volume = "lyrion"` (default: the player's volume in Lyrion) or `"output"` (this machine's audio
/// output, where streamdockd manages it: for Bluetooth the headset's own volume, H15-50).
fn output_volume(table: &toml::Table, at: &str) -> anyhow::Result<Option<bool>> {
    Ok(match text(table, "volume", at)?.as_deref() {
        None => None,
        Some("lyrion") => Some(false),
        Some("output") => Some(true),
        Some(other) => bail!("{at} volume must be \"lyrion\" or \"output\", got {other:?}"),
    })
}

/// The server address and every tab of `[modules.lyrion]`: the top level (when it names a player)
/// and each `[[tab]]`.
pub fn tabs(table: &toml::Table) -> anyhow::Result<(String, Vec<Tab>)> {
    let at = "[modules.lyrion]";
    for key in table.keys() {
        if !["enabled", "url", "tab", "show"].contains(&key.as_str()) && !TAB_KEYS.contains(&key.as_str()) {
            bail!(
                "{at} unknown setting {key:?} (known: url, player, prefix, title, id, volume, now_playing, layout, then, tone, volume_bar, follow_headset, show, tab)"
            );
        }
    }
    let url = text(table, "url", at)?
        .with_context(|| format!("{at} url is required, e.g. \"http://192.168.1.13:9000\""))?;
    let player = text(table, "player", at)?;
    let prefix = text(table, "prefix", at)?.unwrap_or_else(|| "_dock".to_owned());
    let volume = output_volume(table, at)?.unwrap_or(false);
    let band = now_playing::Config::from_value(table.get("now_playing")).with_context(|| at.to_owned())?;
    let tone = songpal::Config::from_value(table.get("tone")).with_context(|| at.to_owned())?;
    let show = match table.get("show") {
        None => true,
        Some(v) => v
            .as_bool()
            .with_context(|| format!("{at} show must be true or false"))?,
    };
    let mut tabs = Vec::new();
    if let Some(player) = player.as_ref().filter(|_| show) {
        tabs.push(Tab {
            id: text(table, "id", at)?.unwrap_or_else(|| "lyrion".into()),
            title: text(table, "title", at)?.unwrap_or_else(|| "Music".into()),
            player: player.clone(),
            prefix: prefix.clone(),
            output_volume: volume,
            now_playing: band.clone(),
            layout: Layout::parse(table, at)?.unwrap_or_default(),
            then: text(table, "then", at)?,
            tone: tone.clone(),
            volume_bar: VolumeBar::parse(table, at)?,
            follow_headset: flag(table, "follow_headset", at)?,
        });
    }
    if let Some(list) = table.get("tab") {
        let list = list
            .as_array()
            .with_context(|| format!("{at} tab must be a list of [[modules.lyrion.tab]] tables"))?;
        for (i, t) in list.iter().enumerate() {
            let n = i + 2;
            let at = format!("[[modules.lyrion.tab]] #{}", i + 1);
            let t = t.as_table().with_context(|| format!("{at} must be a table"))?;
            for key in t.keys() {
                if !TAB_KEYS.contains(&key.as_str()) {
                    bail!("{at} unknown setting {key:?} (known: {})", TAB_KEYS.join(", "));
                }
            }
            tabs.push(Tab {
                id: text(t, "id", &at)?.unwrap_or_else(|| format!("lyrion-{n}")),
                title: text(t, "title", &at)?.unwrap_or_else(|| format!("Music {n}")),
                player: text(t, "player", &at)?
                    .or_else(|| player.clone())
                    .unwrap_or_else(|| DEFAULT_PLAYER.to_owned()),
                prefix: text(t, "prefix", &at)?.unwrap_or_else(|| prefix.clone()),
                output_volume: output_volume(t, &at)?.unwrap_or(volume),
                now_playing: match t.get("now_playing") {
                    None => band.clone(),
                    Some(v) => now_playing::Config::from_value(Some(v)).with_context(|| at.clone())?,
                },
                layout: Layout::parse(t, &at)?.unwrap_or_default(),
                then: text(t, "then", &at)?,
                tone: match t.get("tone") {
                    None => tone.clone(),
                    Some(v) => songpal::Config::from_value(Some(v)).with_context(|| at.clone())?,
                },
                volume_bar: match VolumeBar::parse(t, &at)? {
                    None => VolumeBar::parse(table, "[modules.lyrion]")?,
                    some => some,
                },
                follow_headset: flag(t, "follow_headset", &at)?,
            });
        }
    }
    // A volume bar shown all the time takes its key from the now-playing band (H15-95).
    for t in &mut tabs {
        if let Some(bar) = t.volume_bar.filter(|b| b.always) {
            t.now_playing.keys.retain(|&k| k != bar.key);
        }
    }
    for (i, t) in tabs.iter().enumerate() {
        anyhow::ensure!(!t.id.is_empty(), "{at} a tab id must not be empty");
        anyhow::ensure!(
            !tabs[..i].iter().any(|o| o.id == t.id),
            "{at} two tabs have the id {:?}",
            t.id
        );
    }
    Ok((url, tabs))
}

/// Builds one Lyrion mode per tab of `[modules.lyrion]`.
pub fn from_table(table: &toml::Table) -> anyhow::Result<Vec<Box<dyn Module>>> {
    from_table_with(table, None, None)
}

/// The mode switch, set once the mode controller runs (see `modules::Context::modes`).
pub type ModeSlot = Arc<std::sync::OnceLock<crate::modes::ModeSwitch>>;

/// [`from_table`] with the hub (for tabs whose knob 1 turns this machine's audio output) and the
/// mode switch (for `then`, H15-90).
pub fn from_table_with(
    table: &toml::Table,
    hub: Option<&Arc<crate::hub::Hub>>,
    modes: Option<&ModeSlot>,
) -> anyhow::Result<Vec<Box<dyn Module>>> {
    let (url, tabs) = tabs(table)?;
    tabs.into_iter()
        .map(|tab| -> anyhow::Result<Box<dyn Module>> {
            Ok(Box::new(Lyrion {
                client: Client::new(&url)?,
                id: tab.id,
                title: tab.title,
                player: tab.player,
                prefix: tab.prefix,
                output_volume: tab.output_volume.then(|| hub.cloned()).flatten(),
                now_playing: tab.now_playing,
                layout: tab.layout,
                then: tab.then,
                modes: modes.cloned(),
                tone: tab.tone.map(songpal::Songpal::new).transpose()?,
                volume_bar: tab.volume_bar,
                headset_hub: tab.follow_headset.then(|| hub.cloned()).flatten(),
            }))
        })
        .collect()
}

#[derive(Debug)]
pub struct Lyrion {
    id: String,
    title: String,
    client: Client,
    /// As configured: name or MAC.
    player: String,
    prefix: String,
    /// `volume = "output"`: knob 1 turns this machine's audio output (H15-50).
    output_volume: Option<Arc<crate::hub::Hub>>,
    now_playing: now_playing::Config,
    layout: Layout,
    then: Option<String>,
    modes: Option<ModeSlot>,
    tone: Option<songpal::Songpal>,
    volume_bar: Option<VolumeBar>,
    /// `follow_headset = true`: the hub, to see where the built-in player's music is (H15-95).
    headset_hub: Option<Arc<crate::hub::Hub>>,
}

/// A `_dock` playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Playlist {
    id: u64,
    /// Shown on the key: prefix and separators removed, `_` as spaces.
    name: String,
}

/// What we know about the server and the player.
#[derive(Debug, Default, Clone, PartialEq)]
struct State {
    /// What the keys show (H15-90).
    layout: Layout,
    /// The speaker's equaliser band on knob 2, as last read or set (H15-90).
    tone_level: Option<i32>,
    volume_bar: Option<VolumeBar>,
    /// With `show = "changes"`: the bar is on its key until then (H15-95).
    volume_shown_until: Option<tokio::time::Instant>,
    /// Headset mode: the built-in player has the music, so the tab controls it, up to its
    /// `max_volume` (H15-95).
    headset: Option<(String, u32)>,
    /// `None`: not loaded yet, or the server is unreachable.
    playlists: Option<Vec<Playlist>>,
    /// Failed status/playlist refreshes in a row; "offline" shows from [`OFFLINE_AFTER`] on.
    failures: u32,
    /// Resolved player MAC, once the player list has been read.
    player_id: Option<String>,
    player_missing: bool,
    /// The player is known but not connected to Lyrion (switched off, out of reach, H15-87).
    player_offline: bool,
    page: usize,
    mode: PlayMode,
    volume: Option<i32>,
    /// The playlist we loaded, and the server's playlist timestamp right after loading it. When the
    /// timestamp changes, someone else changed the queue and the highlight goes.
    loaded: Option<(u64, Option<f64>)>,
    /// When we loaded `loaded` (for the "can't play" check).
    loaded_at: Option<tokio::time::Instant>,
    /// A playlist that stopped right after we loaded it; shown as "can't play" until pressed again.
    failed: Option<u64>,
    playlist_timestamp: Option<f64>,
    /// Results and status changes for the activity feed, sent after each step of the loop.
    notes: Vec<Note>,
    /// The now-playing band, when the tab has one (H15-88).
    band: Option<now_playing::Config>,
    /// What the player plays (H15-88).
    track: Option<now_playing::Track>,
    /// The band's text and its width in pixels, as last laid out.
    band_text: String,
    band_text_width: f32,
    /// When the band's text last changed: its scroll starts from there.
    scroll_from: Option<tokio::time::Instant>,
    /// The band's cover in the render cache (`render::cover_key`), once fetched.
    cover: Option<String>,
    /// The cover URL last fetched (or tried), so it is fetched once per song.
    cover_tried: Option<String>,
}

/// An activity feed entry (H15-21).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Note {
    Info(String),
    Ok(String),
    Error(String),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum PlayMode {
    #[default]
    Stop,
    Play,
    Pause,
}

/// `_dock_evening_jazz` → `evening jazz`; `_dock Focus` → `Focus`.
fn display_name(raw: &str, prefix: &str) -> Option<String> {
    let rest = raw.strip_prefix(prefix)?;
    let name = rest.trim_start_matches(['_', '-', ' ']).replace('_', " ");
    let name = name.trim().to_owned();
    Some(if name.is_empty() { raw.to_owned() } else { name })
}

/// The `_dock` playlists from a `playlists` response, sorted by name.
fn parse_playlists(result: &Value, prefix: &str) -> Vec<Playlist> {
    let mut list: Vec<Playlist> = result["playlists_loop"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let raw = p["playlist"].as_str()?;
            let id = p["id"].as_u64().or_else(|| p["id"].as_str()?.parse().ok())?;
            Some(Playlist {
                id,
                name: display_name(raw, prefix)?,
            })
        })
        .collect();
    list.sort_by_key(|p| p.name.to_lowercase());
    list
}

/// The playlists a tab with `prefix` shows, as `(id, shown name)`, sorted like on the keys.
pub async fn tab_playlists(url: &str, prefix: &str) -> anyhow::Result<Vec<(u64, String)>> {
    let result = Client::new(url)?
        .request("", json!(["playlists", "0", "1000"]))
        .await?;
    Ok(parse_playlists(&result, prefix)
        .into_iter()
        .map(|p| (p.id, p.name))
        .collect())
}

/// Every player the server announces, for the settings' player choice (H15-87): stored by MAC,
/// shown by name, connected and playing ones first.
pub async fn player_choices(url: &str) -> anyhow::Result<Vec<crate::settings::Choice>> {
    let result = Client::new(url)?
        .request("", json!(["players", "0", "200"]))
        .await?;
    Ok(choices_of(&result))
}

fn choices_of(result: &Value) -> Vec<crate::settings::Choice> {
    let mut players: Vec<(u8, crate::settings::Choice)> = result["players_loop"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let value = p["playerid"].as_str()?.to_owned();
            let label = p["name"].as_str().unwrap_or(&value).to_owned();
            let truthy = |k: &str| p[k].as_i64() == Some(1) || p[k].as_bool() == Some(true);
            let (rank, detail) = match (truthy("connected"), truthy("isplaying")) {
                (true, true) => (0, "playing"),
                (true, false) => (1, ""),
                (false, _) => (2, "offline"),
            };
            Some((
                rank,
                crate::settings::Choice {
                    value,
                    label,
                    detail: detail.to_owned(),
                },
            ))
        })
        .collect();
    players.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.label.to_lowercase().cmp(&b.1.label.to_lowercase()))
    });
    players.into_iter().map(|(_, c)| c).collect()
}

/// The player id for a configured name or MAC, from a `players` response.
pub(crate) fn find_player(result: &Value, wanted: &str) -> Option<String> {
    let players = result["players_loop"].as_array()?;
    if wanted.is_empty() {
        return players.first()?["playerid"].as_str().map(str::to_owned);
    }
    players.iter().find_map(|p| {
        let id = p["playerid"].as_str()?;
        let name = p["name"].as_str().unwrap_or_default();
        (id.eq_ignore_ascii_case(wanted) || name.eq_ignore_ascii_case(wanted)).then(|| id.to_owned())
    })
}

/// `id` (a player id from Lyrion) is this machine's built-in player (H15-55).
fn is_builtin_player(hub: &crate::hub::Hub, id: Option<&str>) -> bool {
    match (hub.player_state(), id) {
        (Some(p), Some(id)) => p.enabled && p.player_id.eq_ignore_ascii_case(id),
        _ => false,
    }
}

/// A volume as LMS reports it (a float or string), as a whole percentage 0-100.
#[allow(clippy::cast_possible_truncation)] // clamped to 0..=100 first
fn percent(v: f64) -> i32 {
    v.round().clamp(0.0, 100.0) as i32
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.parse().ok())
}

impl State {
    /// Applies a `status` response.
    fn apply_status(&mut self, result: &Value) {
        self.apply_status_at(result, tokio::time::Instant::now());
    }

    fn apply_status_at(&mut self, result: &Value, now: tokio::time::Instant) {
        self.mode = match result["mode"].as_str() {
            Some("play") => PlayMode::Play,
            Some("pause") => PlayMode::Pause,
            _ => PlayMode::Stop,
        };
        let volume = number(&result["mixer volume"]).map(percent);
        if self.volume.is_some() && volume != self.volume {
            self.volume_changed(now);
        }
        self.volume = volume;
        self.playlist_timestamp = number(&result["playlist_timestamp"]);
        if let Some((id, ts)) = self.loaded {
            match ts {
                // First status after loading: remember the timestamp our load produced.
                None => self.loaded = Some((id, self.playlist_timestamp)),
                Some(ts) if Some(ts) != self.playlist_timestamp => self.loaded = None,
                Some(_) => {}
            }
        }
        // Stopped soon after our load, on our queue: the playlist can't be played.
        if let (Some((id, _)), Some(at)) = (self.loaded, self.loaded_at) {
            if self.mode == PlayMode::Stop && now.duration_since(at) < CANT_PLAY_WITHIN {
                self.failed = Some(id);
                self.loaded = None;
                self.loaded_at = None;
            } else if now.duration_since(at) >= CANT_PLAY_WITHIN {
                self.loaded_at = None;
            }
        }
    }

    fn offline(&self) -> bool {
        self.failures >= OFFLINE_AFTER
    }

    /// The LCD keys of the band (H15-88); empty without one.
    fn band_keys(&self) -> &[u8] {
        self.band.as_ref().map_or(&[], |b| b.keys.as_slice())
    }

    /// The LCD keys left for playlists.
    fn playlist_keys(&self) -> Vec<u8> {
        (1..=KEYS).filter(|k| !self.band_keys().contains(k)).collect()
    }

    /// The music screen's ⏮ ⏯ ⏭: the first three keys the band leaves free (4, 5, 6 by default).
    fn transport_keys(&self) -> Vec<u8> {
        self.playlist_keys().into_iter().take(3).collect()
    }

    /// The music screen's keys apart from the band: big icons, ⏯ showing what a press does.
    fn transport_faces(&self) -> Vec<crate::modules::KeyFace> {
        let playing = self.mode == PlayMode::Play;
        let icons = [
            ("previous", FaceState::FACE_STATE_IDLE),
            (
                if playing { "pause" } else { "play" },
                if playing {
                    FaceState::FACE_STATE_ACTIVE
                } else {
                    FaceState::FACE_STATE_IDLE
                },
            ),
            ("next", FaceState::FACE_STATE_IDLE),
        ];
        self.transport_keys()
            .into_iter()
            .zip(icons)
            .map(|(key, (icon, state))| crate::modules::KeyFace {
                image: icon.to_owned(),
                ..face(key, "", "", state)
            })
            .collect()
    }

    fn pages(&self) -> usize {
        let per_page = self.playlist_keys().len().max(1);
        self.playlists
            .as_ref()
            .map_or(1, |l| l.len().div_ceil(per_page).max(1))
    }

    /// The playlist on LCD key `key` (1-6) of the current page.
    fn playlist_on(&self, key: u8) -> Option<&Playlist> {
        let keys = self.playlist_keys();
        let slot = keys.iter().position(|&k| k == key)?;
        let index = self.page * keys.len() + slot;
        self.playlists.as_ref()?.get(index)
    }

    /// Takes a new `status` for the band: the track, and the scroll restarting when the text
    /// changes (H15-88).
    fn set_track(&mut self, track: Option<now_playing::Track>, now: tokio::time::Instant) {
        self.track = track;
        let Some(band) = &self.band else { return };
        let text = self
            .track
            .as_ref()
            .map_or_else(|| "Nothing playing".to_owned(), |t| t.text(band));
        if text != self.band_text {
            self.band_text_width = crate::render::band_text_width(&text);
            self.band_text = text;
            self.scroll_from = Some(now);
        }
    }

    /// The band needs frames: its text is too long for it and it scrolls (only while playing).
    fn scrolling(&self) -> bool {
        let Some(band) = &self.band else { return false };
        let count = u32::try_from(band.keys.len()).unwrap_or(1);
        band.scroll
            && !band.keys.is_empty()
            && self.track.as_ref().is_some_and(|t| t.playing)
            && self.band_text_width > crate::render::band_inner_width(count)
    }

    /// The band's faces at `now` (H15-88).
    fn band_faces(&self, now: tokio::time::Instant) -> Vec<crate::modules::KeyFace> {
        let Some(band) = &self.band else { return Vec::new() };
        if band.keys.is_empty() {
            return Vec::new();
        }
        let count = u32::try_from(band.keys.len()).unwrap_or(1);
        let (text, offset) = if self.scrolling() {
            let since = self
                .scroll_from
                .map_or_else(Default::default, |at| now.saturating_duration_since(at));
            let inner = crate::render::band_inner_width(count);
            let offset =
                now_playing::marquee_offset(since, self.band_text_width, inner, band.speed, band.pause);
            (self.band_text.clone(), offset)
        } else if band.scroll {
            (self.band_text.clone(), 0)
        } else {
            (crate::render::band_fit(&self.band_text, count), 0)
        };
        let playing = self.track.as_ref().is_some_and(|t| t.playing);
        let progress = self.track.as_ref().map_or(0, |t| t.progress(now));
        let cover = if band.cover {
            self.cover.as_deref().unwrap_or_default()
        } else {
            ""
        };
        now_playing::band_faces(band, &text, offset, progress, cover, !playing)
    }

    fn page_view(&self) -> Page {
        self.page_view_at(tokio::time::Instant::now())
    }

    fn page_view_at(&self, now: tokio::time::Instant) -> Page {
        let mut page = self.playlist_view();
        if let Some(band) = &self.band {
            page.now_playing = Some(now_playing::message(self.track.as_ref(), band));
            let ok =
                !self.offline() && !self.player_missing && !self.player_offline && self.playlists.is_some();
            if ok {
                page.faces.extend(self.band_faces(now));
            }
        }
        if let Some(bar) = self.volume_face(now) {
            page.faces.retain(|f| f.key != bar.key);
            page.faces.push(bar);
        }
        page
    }

    /// The player the tab controls now: the built-in one in headset mode, else its own.
    fn target(&self) -> Option<String> {
        self.headset
            .as_ref()
            .map(|(id, _)| id.clone())
            .or_else(|| self.player_id.clone())
    }

    /// The volume as shown: in headset mode at most the built-in player's `max_volume`.
    fn shown_volume(&self) -> Option<i32> {
        let cap = self
            .headset
            .as_ref()
            .map_or(100, |(_, max)| i32::try_from(*max).unwrap_or(100));
        self.volume.map(|v| v.min(cap))
    }

    /// The volume bar's face, while it is shown (H15-95).
    fn volume_face(&self, now: tokio::time::Instant) -> Option<crate::modules::KeyFace> {
        let bar = self.volume_bar?;
        let shown = bar.always || self.volume_shown_until.is_some_and(|until| now < until);
        if !shown || self.problem().is_some() {
            return None;
        }
        let level = u32::try_from(self.shown_volume()?.clamp(0, 100)).ok()?;
        Some(crate::modules::KeyFace {
            level: Some(level),
            ..face(bar.key, "Volume", "", FaceState::FACE_STATE_ON)
        })
    }

    /// The volume changed (a knob, Lyrion, another controller): show the bar for a while.
    fn volume_changed(&mut self, now: tokio::time::Instant) {
        if self.volume_bar.is_some_and(|b| !b.always) {
            self.volume_shown_until = Some(now + VOLUME_BAR_FOR);
        }
    }

    /// What keeps the tab from working, as a key's label and detail.
    fn problem(&self) -> Option<(&'static str, &'static str)> {
        if self.offline() {
            Some(("Lyrion offline", "retrying"))
        } else if self.player_missing {
            Some(("Player not found", "pick another one"))
        } else if self.player_offline {
            Some(("Player offline", "switched off?"))
        } else {
            None
        }
    }

    /// Knob 2 (the big one) is the volume on every music tab; knob 1 the bass on the music
    /// screen, the page on the playlists (H15-95). Both presses play or pause.
    fn knobs(&self) -> Vec<crate::modules::KnobRole> {
        let play = match self.mode {
            PlayMode::Play => "Pause",
            _ => "Play",
        };
        let volume = self
            .shown_volume()
            .map_or_else(|| "Volume".to_owned(), |v| format!("Volume {v}%"));
        let first = match self.layout {
            Layout::Controls => self.tone_level.map(|l| format!("Bass {l:+}")),
            Layout::Playlists => {
                (self.pages() > 1).then(|| format!("Page {}/{}", self.page + 1, self.pages()))
            }
        };
        vec![
            knob(
                1,
                first.map_or_else(|| format!("press {play}"), |f| format!("{f} · press {play}")),
            ),
            knob(2, format!("{volume} · press {play}")),
        ]
    }

    fn playlist_view(&self) -> Page {
        let knobs = self.knobs();

        if let Some((label, detail)) = self.problem() {
            return Page {
                now_playing: None,
                faces: vec![face(1, label, detail, FaceState::FACE_STATE_UNAVAILABLE)],
                knobs,
            };
        }
        if self.layout == Layout::Controls {
            return Page {
                now_playing: None,
                faces: self.transport_faces(),
                knobs,
            };
        }
        let Some(playlists) = &self.playlists else {
            return Page {
                now_playing: None,
                faces: vec![face(1, "Loading…", "", FaceState::FACE_STATE_IDLE)],
                knobs,
            };
        };
        if playlists.is_empty() {
            return Page {
                now_playing: None,
                faces: vec![face(
                    self.playlist_keys().first().copied().unwrap_or(1),
                    "No _dock playlists",
                    "create one in Lyrion",
                    FaceState::FACE_STATE_IDLE,
                )],
                knobs,
            };
        }
        let playing_id = self.loaded.map(|(id, _)| id);
        let faces = self
            .playlist_keys()
            .into_iter()
            .filter_map(|key| {
                let p = self.playlist_on(key)?;
                let (detail, state) = if Some(p.id) == self.failed {
                    ("can't play", FaceState::FACE_STATE_UNAVAILABLE)
                } else if Some(p.id) == playing_id {
                    match self.mode {
                        PlayMode::Play => ("playing", FaceState::FACE_STATE_ACTIVE),
                        PlayMode::Pause => ("paused", FaceState::FACE_STATE_ON),
                        PlayMode::Stop => ("", FaceState::FACE_STATE_IDLE),
                    }
                } else {
                    ("", FaceState::FACE_STATE_IDLE)
                };
                Some(face(key, p.name.clone(), detail, state))
            })
            .collect();
        Page {
            faces,
            knobs,
            now_playing: None,
        }
    }
}

impl Module for Lyrion {
    fn id(&self) -> &str {
        &self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn kind(&self) -> &'static str {
        "music"
    }

    fn run(self: Box<Self>, mut io: ModuleIo) -> ModuleTask {
        Box::pin(async move {
            let mut state = State {
                band: Some(self.now_playing.clone()),
                layout: self.layout,
                volume_bar: self.volume_bar,
                ..State::default()
            };
            let mut active = false;
            // Frames of the scrolling band (H15-88), only while it scrolls.
            let mut frame_tick = tokio::time::interval(Duration::from_millis(
                1000 / u64::from(self.now_playing.fps.max(1)),
            ));
            frame_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut status_tick = tokio::time::interval(STATUS_EVERY);
            let mut playlists_tick = tokio::time::interval(PLAYLISTS_EVERY);
            status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            playlists_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut shown = Page::default();
            let mut last_retry = tokio::time::Instant::now();
            let publish = |state: &mut State, shown: &mut Page, io: &ModuleIo| {
                for note in state.notes.drain(..) {
                    match note {
                        Note::Info(text) => io.activity.info(text),
                        Note::Ok(text) => io.activity.ok(text),
                        Note::Error(text) => io.activity.error(text),
                    }
                }
                let page = state.page_view();
                if page != *shown {
                    shown.clone_from(&page);
                    io.page.send_replace(page);
                }
            };
            publish(&mut state, &mut shown, &io);

            // An event read ahead while merging volume turns, handled next.
            let mut pending: Option<ModuleEvent> = None;
            loop {
                if let Some(event) = pending.take() {
                    self.on_event(&mut state, &mut active, event).await;
                    publish(&mut state, &mut shown, &io);
                    continue;
                }
                tokio::select! {
                    event = io.events.recv() => {
                        let Some(mut event) = event else { return };
                        // Volume turns that queued up behind a slow request become one command.
                        if let ModuleEvent::Input(Input::KnobTurn { knob: 1, delta }) = &mut event {
                            while let Ok(next) = io.events.try_recv() {
                                match next {
                                    ModuleEvent::Input(Input::KnobTurn { knob: 1, delta: more }) => *delta += more,
                                    other => {
                                        pending = Some(other);
                                        break;
                                    }
                                }
                            }
                        }
                        self.on_event(&mut state, &mut active, event).await;
                    }
                    _ = status_tick.tick(), if active => {
                        if state.offline() {
                            // Back off: one combined retry per OFFLINE_RETRY, not every tick.
                            if last_retry.elapsed() >= OFFLINE_RETRY {
                                last_retry = tokio::time::Instant::now();
                                self.refresh_playlists(&mut state).await;
                                self.refresh_status(&mut state).await;
                            }
                        } else {
                            self.refresh_status(&mut state).await;
                        }
                    }
                    _ = playlists_tick.tick(), if active && !state.offline() => {
                        self.refresh_playlists(&mut state).await;
                        self.refresh_tone(&mut state).await;
                    }
                    _ = frame_tick.tick(), if active && state.scrolling() => {}
                }
                publish(&mut state, &mut shown, &io);
            }
        })
    }
}

impl Lyrion {
    async fn on_event(&self, state: &mut State, active: &mut bool, event: ModuleEvent) {
        match event {
            ModuleEvent::Activated => {
                *active = true;
                self.refresh_playlists(state).await;
                self.refresh_tone(state).await;
                self.refresh_status(state).await;
            }
            ModuleEvent::Deactivated => *active = false,
            ModuleEvent::Input(input) => self.handle(state, input).await,
        }
    }

    /// Sends a command to the player, then refreshes the status. Returns whether the server
    /// answered; a failure is reported as `what failed: …`.
    async fn player_command(&self, state: &mut State, what: &str, command: Value) -> bool {
        let Some(player) = state.target() else {
            state
                .notes
                .push(Note::Error(format!("{what}: player {:?} not found", self.player)));
            return false;
        };
        let sent = match self.client.request(&player, command).await {
            Ok(_) => true,
            // The command may still have reached the server (slow during a rescan); the status
            // refresh below shows what really happened. Only refreshes decide "offline".
            Err(e) => {
                tracing::warn!(error = %e, "lyrion: command failed");
                state.notes.push(Note::Error(format!("{what} failed: {e:#}")));
                false
            }
        };
        self.refresh_status(state).await;
        sent
    }

    /// Button 8, knob 2's press and the now-playing band (H15-88).
    async fn play_pause(&self, state: &mut State) {
        if self.player_command(state, "play/pause", json!(["pause"])).await {
            let now = if state.mode == PlayMode::Play {
                "play"
            } else {
                "pause"
            };
            state.notes.push(Note::Ok(now.to_owned()));
        }
    }

    async fn handle(&self, state: &mut State, input: Input) {
        match input {
            Input::Key {
                key: key @ 1..=6,
                pressed: true,
            } => self.press_lcd(state, key).await,
            Input::Key {
                key: 7,
                pressed: true,
            } => self.skip(state, -1).await,
            Input::Key {
                key: 8,
                pressed: true,
            }
            | Input::KnobPress {
                knob: 1 | 2,
                pressed: true,
            } => self.play_pause(state).await,
            Input::Key {
                key: 9,
                pressed: true,
            } => self.skip(state, 1).await,
            // The big knob is the volume on every music tab (H15-95).
            Input::KnobTurn { knob: 2, delta } => self.turn_volume(state, delta).await,
            Input::KnobTurn { knob: 1, delta } if delta != 0 && state.layout == Layout::Controls => {
                self.turn_tone(state, delta).await;
            }
            Input::KnobTurn { knob: 1, delta } if delta != 0 && state.layout == Layout::Playlists => {
                let pages = state.pages();
                state.page = if delta > 0 {
                    (state.page + 1) % pages
                } else {
                    (state.page + pages - 1) % pages
                };
            }
            _ => {}
        }
    }

    /// An LCD key: the band = play/pause; on the music screen ⏮ ⏯ ⏭; else a playlist.
    async fn press_lcd(&self, state: &mut State, key: u8) {
        if state.band_keys().contains(&key) {
            return self.play_pause(state).await;
        }
        if state.layout == Layout::Controls {
            match state.transport_keys().iter().position(|&k| k == key) {
                Some(0) => self.skip(state, -1).await,
                Some(1) => self.play_pause(state).await,
                Some(2) => self.skip(state, 1).await,
                _ => {}
            }
            return;
        }
        let Some(playlist) = state.playlist_on(key).cloned() else {
            return;
        };
        tracing::info!(playlist = %playlist.name, id = playlist.id, "lyrion: play playlist");
        state.loaded = Some((playlist.id, None));
        state.loaded_at = Some(tokio::time::Instant::now());
        state.failed = None;
        let what = format!("load playlist {} on {}", playlist.name, self.player);
        let command = json!([
            "playlistcontrol",
            "cmd:load",
            format!("playlist_id:{}", playlist.id)
        ]);
        if self.player_command(state, &what, command).await {
            state.notes.push(Note::Ok(what));
            self.switch_to_then().await;
        }
    }

    /// `then = "<tab>"`: show that tab after a playlist started (H15-90).
    async fn switch_to_then(&self) {
        let (Some(then), Some(modes)) = (&self.then, self.modes.as_ref().and_then(|m| m.get())) else {
            return;
        };
        if let Err(e) = modes.set(then).await {
            tracing::warn!(tab = %self.id, then, error = %e, "lyrion: can't switch to the `then` tab");
        }
    }

    /// Previous (`by` = -1) or next (+1) track.
    async fn skip(&self, state: &mut State, by: i32) {
        let (what, index) = if by < 0 {
            ("previous", "-1")
        } else {
            ("next", "+1")
        };
        if self
            .player_command(state, what, json!(["playlist", "index", index]))
            .await
        {
            state.notes.push(Note::Ok(what.to_owned()));
        }
    }

    /// Knob 2 on the music screen: the speaker's bass (H15-90). Quick turns add up from the last
    /// level we know; the speaker answers with nothing to read back, so that is what we show.
    async fn turn_tone(&self, state: &mut State, delta: i32) {
        let Some(tone) = &self.tone else { return };
        let current = match state.tone_level {
            Some(level) => level,
            None => match tone.level().await {
                Ok(level) => level,
                Err(e) => return state.notes.push(Note::Error(format!("bass: {e:#}"))),
            },
        };
        match tone.set_level(current + delta).await {
            Ok(level) => {
                state.tone_level = Some(level);
                state.notes.push(Note::Ok(format!("bass {level:+}")));
            }
            Err(e) => state.notes.push(Note::Error(format!("bass: {e:#}"))),
        }
    }

    /// Reads the speaker's band level for knob 2's label (on activation and with the playlists).
    async fn refresh_tone(&self, state: &mut State) {
        if let Some(tone) = &self.tone
            && state.layout == Layout::Controls
        {
            match tone.level().await {
                Ok(level) => state.tone_level = Some(level),
                Err(e) => tracing::debug!(error = %format!("{e:#}"), "lyrion: speaker tone not read"),
            }
        }
    }

    async fn turn_volume(&self, state: &mut State, delta: i32) {
        if self.turns_output(state) {
            return self.turn_output(state, delta).await;
        }
        let step = delta * VOLUME_STEP;
        // In headset mode the knob stops at the built-in player's `max_volume`.
        let command = match (&state.headset, state.volume) {
            (Some((_, max)), Some(v)) => {
                let max = i32::try_from(*max).unwrap_or(100);
                json!(["mixer", "volume", (v.min(max) + step).clamp(0, max).to_string()])
            }
            _ => json!(["mixer", "volume", format!("{step:+}")]),
        };
        if self.player_command(state, "volume", command).await {
            // Shown at once; the next status confirms the value.
            if let Some(v) = state.volume.as_mut() {
                *v = (*v + step).clamp(0, 100);
            }
            state.volume_changed(tokio::time::Instant::now());
            let volume = state
                .volume
                .map_or_else(|| format!("{step:+}"), |v| v.to_string());
            state.notes.push(Note::Ok(format!("volume {volume} %")));
        }
    }

    async fn refresh_playlists(&self, state: &mut State) {
        if state.player_id.is_none() {
            match self.client.request("", json!(["players", "0", "100"])).await {
                Ok(result) => {
                    self.reachable(state);
                    state.player_id = find_player(&result, &self.player);
                    self.note_output_volume(state);
                    let missing = state.player_id.is_none();
                    if missing {
                        tracing::warn!(player = %self.player, "lyrion: player not found on the server");
                    }
                    match (state.player_missing, missing) {
                        (false, true) => state.notes.push(Note::Error(format!(
                            "player {:?} not found on the server",
                            self.player
                        ))),
                        (true, false) => state
                            .notes
                            .push(Note::Info(format!("player {} found", self.player))),
                        _ => {}
                    }
                    state.player_missing = missing;
                }
                Err(e) => return self.unreachable(state, &e),
            }
        }
        match self.client.request("", json!(["playlists", "0", "1000"])).await {
            Ok(result) => {
                self.reachable(state);
                let list = parse_playlists(&result, &self.prefix);
                if state.playlists.as_ref() != Some(&list) {
                    tracing::info!(count = list.len(), "lyrion: _dock playlists");
                }
                state.playlists = Some(list);
                state.page = state.page.min(state.pages() - 1);
            }
            Err(e) => self.unreachable(state, &e),
        }
    }

    /// Knob 1 turns this machine's output only with `volume = "output"` and while the tab's player
    /// is this machine's built-in player (H15-87): any other player (the tab switched to another
    /// room) gets Lyrion's volume, so the knob never turns the board's headset for music played
    /// elsewhere. For the built-in player both are the same, since its Lyrion volume and the output
    /// follow each other (H15-52).
    fn turns_output(&self, state: &State) -> bool {
        let Some(hub) = &self.output_volume else {
            return false;
        };
        is_builtin_player(hub, state.player_id.as_deref())
    }

    /// `volume = "output"` on a tab whose player is another device has no effect: say so in the log.
    fn note_output_volume(&self, state: &State) {
        if self.output_volume.is_some() && state.player_id.is_some() && !self.turns_output(state) {
            tracing::info!(
                player = %self.player,
                "lyrion: volume = \"output\" doesn't apply, the tab's player isn't this machine's built-in player; knob 1 = Lyrion volume"
            );
        }
    }

    /// Knob 1 with `volume = "output"`: this machine's audio output (H15-50).
    async fn turn_output(&self, state: &mut State, delta: i32) {
        let Some(audio) = self.output_volume.as_ref().and_then(|hub| hub.audio()) else {
            state.notes.push(Note::Error(
                "this streamdockd doesn't manage the audio".to_owned(),
            ));
            return;
        };
        match audio.step_volume(delta * VOLUME_STEP).await {
            Ok(volume) => {
                state.volume = i32::try_from(volume).ok();
                state.notes.push(Note::Ok(format!("output volume {volume} %")));
            }
            Err(e) => state.notes.push(Note::Error(format!("volume: {e:#}"))),
        }
    }

    /// The selected output's volume, shown on knob 1 with `volume = "output"`.
    async fn output_level(&self) -> Option<i32> {
        let audio = self.output_volume.as_ref()?.audio()?;
        let state = audio.current().await;
        let output = state.outputs.iter().find(|o| o.selected)?;
        i32::try_from(output.volume_percent).ok()
    }

    async fn refresh_status(&self, state: &mut State) {
        self.follow_headset(state);
        let Some(player) = state.target() else {
            return;
        };
        let command = if state.band.is_some() {
            // Artist, album, artwork URL, cover id, duration (H15-88).
            json!(["status", "-", "1", "tags:alKcd"])
        } else {
            json!(["status", "-", "1"])
        };
        match self.client.request(&player, command).await {
            Ok(result) => {
                self.reachable(state);
                // Missing on older servers: then the player counts as connected.
                self.set_player_offline(state, result["player_connected"].as_i64() == Some(0));
                let failed_before = state.failed;
                state.apply_status(&result);
                if state.band.is_some() {
                    let now = tokio::time::Instant::now();
                    let track = now_playing::Track::from_status(&result, self.client.url(), &player, now);
                    state.set_track(track, now);
                    self.fetch_cover(state).await;
                }
                if self.turns_output(state) {
                    state.volume = self.output_level().await;
                }
                if let Some(id) = state.failed.filter(|id| Some(*id) != failed_before) {
                    let name = state
                        .playlists
                        .iter()
                        .flatten()
                        .find(|p| p.id == id)
                        .map_or_else(|| format!("playlist {id}"), |p| p.name.clone());
                    state.notes.push(Note::Error(format!("can't play {name}")));
                }
            }
            // Lyrion drops the connection for a player it doesn't know: a player that was forgotten
            // looks like a network error. Ask the player list before calling the server offline.
            Err(e) => {
                if !self.player_gone(state).await {
                    self.unreachable(state, &e);
                }
            }
        }
    }

    /// `follow_headset`: whether the built-in player has the music now (headset mode, H15-91).
    fn follow_headset(&self, state: &mut State) {
        let Some(hub) = &self.headset_hub else { return };
        let builtin = hub
            .player_state()
            .filter(|p| p.enabled && p.connected && (p.playing || p.paused));
        let max = hub.builtin_player().and_then(|p| p.max_volume()).unwrap_or(100);
        let now = builtin.map(|p| (p.player_id, max));
        if now.is_some() != state.headset.is_some() {
            state.notes.push(Note::Info(if now.is_some() {
                "headset mode: controlling the built-in player".to_owned()
            } else {
                format!("controlling {} again", self.player)
            }));
        }
        state.headset = now;
    }

    /// Whether the server answers but no longer knows the player; then the tab says "Player not
    /// found" (once in the activity feed) and resolves it again with the playlists, every 30 s.
    async fn player_gone(&self, state: &mut State) -> bool {
        let Ok(result) = self.client.request("", json!(["players", "0", "200"])).await else {
            return false;
        };
        self.reachable(state);
        if find_player(&result, &self.player).is_some() {
            return false;
        }
        if !state.player_missing {
            tracing::warn!(player = %self.player, "lyrion: player no longer known to the server");
            state.notes.push(Note::Error(format!(
                "player {:?} is no longer in Lyrion",
                self.player
            )));
        }
        state.player_missing = true;
        state.player_id = None;
        true
    }

    /// Notes when the player goes offline or comes back (once per change, not every refresh).
    fn set_player_offline(&self, state: &mut State, offline: bool) {
        if state.player_offline == offline {
            return;
        }
        state.player_offline = offline;
        state.notes.push(if offline {
            Note::Error(format!("player {} is offline", self.player))
        } else {
            Note::Info(format!("player {} is back", self.player))
        });
    }

    /// Fetches the band's cover once per song (H15-88); without one, or when it fails, the band
    /// has none.
    async fn fetch_cover(&self, state: &mut State) {
        let Some(band) = state.band.as_ref().filter(|b| b.cover && !b.keys.is_empty()) else {
            return;
        };
        let count = u32::try_from(band.keys.len()).unwrap_or(1);
        let url = state.track.as_ref().and_then(|t| t.cover.clone());
        if url == state.cover_tried {
            return;
        }
        state.cover_tried.clone_from(&url);
        state.cover = None;
        let Some(url) = url else { return };
        let key = crate::render::cover_key(&url, count);
        if crate::render::has_cover(&key) {
            state.cover = Some(key);
            return;
        }
        match self.client.get_bytes(&url, 4 << 20).await {
            Ok(bytes) => match crate::render::store_cover(&key, count, &bytes) {
                Ok(()) => state.cover = Some(key),
                Err(e) => tracing::debug!(%url, error = %e, "lyrion: cover not decodable"),
            },
            Err(e) => tracing::debug!(%url, error = %e, "lyrion: cover not loaded"),
        }
    }

    /// The server answered: back online if it was offline.
    fn reachable(&self, state: &mut State) {
        if state.offline() {
            tracing::info!(url = %self.client.url(), "lyrion: server back online");
            state
                .notes
                .push(Note::Info(format!("Lyrion back online ({})", self.client.url())));
        }
        state.failures = 0;
    }

    fn unreachable(&self, state: &mut State, error: &anyhow::Error) {
        state.failures += 1;
        if state.failures == OFFLINE_AFTER {
            tracing::warn!(url = %self.client.url(), error = %error, "lyrion: server unreachable");
            state.notes.push(Note::Error(format!(
                "Lyrion offline: {} unreachable ({error:#})",
                self.client.url()
            )));
        } else {
            tracing::debug!(error = %error, "lyrion: refresh failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let text = match name {
            "playlists" => include_str!("fixtures/playlists.json"),
            "players" => include_str!("fixtures/players.json"),
            "status_office" => include_str!("fixtures/status_office.json"),
            "status_playing" => include_str!("fixtures/status_playing.json"),
            _ => unreachable!(),
        };
        serde_json::from_str::<Value>(text).unwrap()["result"].clone()
    }

    fn playlists(names: &[&str]) -> Vec<Playlist> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| Playlist {
                id: i as u64 + 1,
                name: (*n).to_owned(),
            })
            .collect()
    }

    fn ready(list: Vec<Playlist>) -> State {
        State {
            playlists: Some(list),
            player_id: Some("cc:cc:00:00:00:ae".into()),
            ..State::default()
        }
    }

    #[test]
    fn knows_the_built_in_player_by_its_id() {
        let hub = crate::hub::Hub::new();
        assert!(
            !is_builtin_player(&hub, Some("02:00:00:00:00:01")),
            "no player yet"
        );
        hub.set_player_state(streamdock_proto::streamdock::v1::PlayerState {
            enabled: true,
            player_id: "02:00:00:00:00:01".into(),
            ..Default::default()
        });
        assert!(is_builtin_player(&hub, Some("02:00:00:00:00:01")));
        assert!(is_builtin_player(
            &hub,
            Some("02:00:00:00:00:01".to_uppercase().as_str())
        ));
        assert!(!is_builtin_player(&hub, Some("cc:cc:00:00:00:ae")));
        assert!(!is_builtin_player(&hub, None));
    }

    #[test]
    fn player_choices_by_mac_with_state() {
        let result = json!({"players_loop": [
            {"playerid": "aa:aa:00:00:00:06", "name": "airplay Bedroom", "connected": 1, "isplaying": 0},
            {"playerid": "cc:cc:00:00:00:ae", "name": "Buero Essence", "connected": 0, "isplaying": 0},
            {"playerid": "02:00:00:00:00:ee", "name": "Luckfox", "connected": 1, "isplaying": 1},
        ]});
        let choices = choices_of(&result);
        let shown: Vec<(&str, &str, &str)> = choices
            .iter()
            .map(|c| (c.value.as_str(), c.label.as_str(), c.detail.as_str()))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("02:00:00:00:00:ee", "Luckfox", "playing"),
                ("aa:aa:00:00:00:06", "airplay Bedroom", ""),
                ("cc:cc:00:00:00:ae", "Buero Essence", "offline"),
            ]
        );
    }

    #[test]
    fn display_names() {
        assert_eq!(
            display_name("_dock_evening_jazz", "_dock").as_deref(),
            Some("evening jazz")
        );
        assert_eq!(display_name("_dock Focus", "_dock").as_deref(), Some("Focus"));
        assert_eq!(
            display_name("_dock-Morning_Mix", "_dock").as_deref(),
            Some("Morning Mix")
        );
        assert_eq!(display_name("_dock", "_dock").as_deref(), Some("_dock"));
        assert_eq!(display_name("Jazz", "_dock"), None);
    }

    #[test]
    fn parses_only_dock_playlists_from_the_real_server() {
        let list = parse_playlists(&fixture("playlists"), "_dock");
        assert_eq!(
            list,
            [Playlist {
                id: 1_190_695,
                name: "evening jazz".into()
            }]
        );
    }

    #[test]
    fn finds_the_player_by_name_or_mac() {
        let players = fixture("players");
        assert_eq!(
            find_player(&players, "Buero Essence").as_deref(),
            Some("cc:cc:00:00:00:ae")
        );
        assert_eq!(
            find_player(&players, "buero essence").as_deref(),
            Some("cc:cc:00:00:00:ae")
        );
        assert_eq!(
            find_player(&players, "CC:CC:00:00:00:AE").as_deref(),
            Some("cc:cc:00:00:00:ae")
        );
        assert_eq!(find_player(&players, "Kitchen"), None);
    }

    #[test]
    fn status_from_the_real_server() {
        let mut s = ready(vec![]);
        s.apply_status(&fixture("status_office"));
        assert_eq!((s.mode, s.volume), (PlayMode::Play, Some(40)));
        s.apply_status(&fixture("status_playing"));
        assert_eq!((s.mode, s.volume), (PlayMode::Play, Some(42)));
        s.apply_status(&json!({"mode": "stop", "mixer volume": "55"}));
        assert_eq!(
            (s.mode, s.volume),
            (PlayMode::Stop, Some(55)),
            "volume may arrive as a string"
        );
    }

    #[test]
    fn highlights_the_playlist_we_started_until_the_queue_changes() {
        let mut s = ready(playlists(&["a", "b"]));
        s.loaded = Some((2, None));
        s.apply_status(&json!({"mode": "play", "playlist_timestamp": 100.0, "mixer volume": 30}));
        let page = s.page_view();
        assert_eq!(
            page.faces[1].state,
            face(0, "", "", FaceState::FACE_STATE_ACTIVE).state
        );
        assert_eq!(page.faces[1].detail, "playing");
        assert_eq!(
            page.faces[0].state,
            face(0, "", "", FaceState::FACE_STATE_IDLE).state
        );

        s.apply_status(&json!({"mode": "pause", "playlist_timestamp": 100.0}));
        assert_eq!(s.page_view().faces[1].detail, "paused");

        // Someone loaded something else: the highlight goes.
        s.apply_status(&json!({"mode": "play", "playlist_timestamp": 200.0}));
        assert!(s.loaded.is_none());
        assert!(s.page_view().faces.iter().all(|f| f.detail.is_empty()));
    }

    #[test]
    fn a_playlist_that_stops_right_after_loading_is_flagged() {
        let mut s = ready(playlists(&["a", "b"]));
        let t0 = tokio::time::Instant::now();
        s.loaded = Some((1, None));
        s.loaded_at = Some(t0);
        // LMS skipped through unplayable tracks and stopped, 20 s after the load.
        s.apply_status_at(
            &json!({"mode": "stop", "playlist_timestamp": 5.0}),
            t0 + Duration::from_secs(20),
        );
        assert_eq!(s.failed, Some(1));
        let page = s.page_view();
        assert_eq!(page.faces[0].detail, "can't play");
        assert_eq!(
            page.faces[0].state,
            face(0, "", "", FaceState::FACE_STATE_UNAVAILABLE).state
        );
        assert_eq!(page.faces[1].detail, "");
    }

    #[test]
    fn a_stop_long_after_loading_is_just_the_end_of_the_queue() {
        let mut s = ready(playlists(&["a"]));
        let t0 = tokio::time::Instant::now();
        s.loaded = Some((1, None));
        s.loaded_at = Some(t0);
        s.apply_status_at(
            &json!({"mode": "play", "playlist_timestamp": 5.0}),
            t0 + Duration::from_secs(10),
        );
        s.apply_status_at(
            &json!({"mode": "stop", "playlist_timestamp": 5.0}),
            t0 + Duration::from_secs(600),
        );
        assert_eq!(s.failed, None);
        assert_eq!(s.page_view().faces[0].detail, "");
    }

    #[test]
    fn pages_through_more_than_six() {
        let mut s = ready(playlists(&["1", "2", "3", "4", "5", "6", "7", "8"]));
        assert_eq!(s.pages(), 2);
        assert_eq!(s.page_view().faces.len(), 6);
        assert_eq!(
            s.page_view().knobs[0].label,
            "Page 1/2 · press Play",
            "knob 1 pages (H15-95)"
        );
        assert_eq!(
            s.page_view().knobs[1].label,
            "Volume · press Play",
            "the big knob is the volume"
        );
        s.page = 1;
        let page = s.page_view();
        assert_eq!(
            page.faces.iter().map(|f| f.label.as_str()).collect::<Vec<_>>(),
            ["7", "8"]
        );
        assert_eq!(s.playlist_on(2).unwrap().name, "8");
        assert!(s.playlist_on(3).is_none());
    }

    #[test]
    fn the_now_playing_band_takes_its_keys_and_scrolls_long_text() {
        let mut s = ready(playlists(&["1", "2", "3", "4", "5"]));
        s.band = Some(now_playing::Config::default());
        // Playlists move to keys 4-6: two pages of three.
        assert_eq!(s.playlist_keys(), [4, 5, 6]);
        assert_eq!(s.pages(), 2);
        assert_eq!(s.playlist_on(4).unwrap().name, "1");
        assert!(s.playlist_on(1).is_none(), "key 1 is the band");
        let start = tokio::time::Instant::now();
        let status = json!({
            "mode": "play", "time": 10.0, "duration": 100.0,
            "playlist_loop": [{ "title": "A rather long title that cannot fit on three keys", "artist": "Somebody" }]
        });
        s.apply_status(&status);
        let track = now_playing::Track::from_status(&status, "http://lms", "aa", start);
        s.set_track(track, start);
        assert!(s.scrolling());
        let band = |s: &State, at| -> Vec<(u32, i32, u32)> {
            s.page_view_at(at)
                .faces
                .iter()
                .filter_map(|f| f.band.as_option().map(|b| (f.key, b.offset, b.progress)))
                .collect()
        };
        let still = band(&s, start + Duration::from_millis(500));
        assert_eq!(still.iter().map(|b| b.0).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(still.iter().all(|b| b.1 == 0), "paused at the start: {still:?}");
        assert_eq!(still[0].2, 105, "10.5 s of 100 s");
        let moving = band(&s, start + Duration::from_millis(2500));
        assert!(moving[0].1 > 0, "{moving:?}");
        let page = s.page_view_at(start);
        let np = page.now_playing.unwrap();
        assert_eq!(np.title, "A rather long title that cannot fit on three keys");
        assert_eq!(
            (np.duration_ms, np.elapsed_ms, np.playing),
            (100_000, 10_000, true)
        );
        assert_eq!(np.keys, [1, 2, 3]);

        // Paused or short: no frames needed.
        s.track.as_mut().unwrap().playing = false;
        assert!(!s.scrolling());
        let short = json!({ "mode": "play", "playlist_loop": [{ "title": "Short" }] });
        s.set_track(
            now_playing::Track::from_status(&short, "http://lms", "aa", start),
            start,
        );
        assert!(!s.scrolling());
        // Nothing loaded: the band says so, dimmed.
        s.set_track(None, start);
        let idle = s.page_view_at(start);
        let first = idle
            .faces
            .iter()
            .find(|f| f.key == 1)
            .unwrap()
            .band
            .as_option()
            .unwrap()
            .clone();
        assert_eq!(first.text, "Nothing playing");
        assert!(first.dim);
        assert!(idle.now_playing.unwrap().idle);
    }

    #[test]
    fn tabs_share_the_server_and_default_to_the_top_level_player() {
        let t: toml::Table = toml::from_str(
            "url = \"http://lms:9000\"\nplayer = \"Office\"\n[[tab]]\nid = \"kitchen\"\ntitle = \"Kitchen\"\nplayer = \"Kitchen Radio\"\n[[tab]]\nprefix = \"_party\"\n",
        )
        .unwrap();
        let (url, found) = tabs(&t).unwrap();
        assert_eq!(url, "http://lms:9000");
        let got: Vec<_> = found
            .iter()
            .map(|t| {
                (
                    t.id.as_str(),
                    t.title.as_str(),
                    t.player.as_str(),
                    t.prefix.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("lyrion", "Music", "Office", "_dock"),
                ("kitchen", "Kitchen", "Kitchen Radio", "_dock"),
                ("lyrion-3", "Music 3", "Office", "_party")
            ]
        );
        let hidden: toml::Table = toml::from_str(
            "url = \"http://lms:9000\"\nplayer = \"Office\"\nshow = false\n[[tab]]\ntitle = \"K\"\n",
        )
        .unwrap();
        let (_, found) = tabs(&hidden).unwrap();
        assert_eq!(found.len(), 1, "no top-level tab");
        assert_eq!(found[0].player, "Office", "but its player is the default");
        // A tab without any player takes the default (H15-87): see a_tab_without_any_player_uses_the_default.
    }

    #[test]
    fn headset_mode_moves_the_controls_to_the_built_in_player() {
        let hub = crate::hub::Hub::new();
        let lyrion = Lyrion {
            id: "arthur".into(),
            title: "Musik".into(),
            client: Client::new("http://127.0.0.1:9").unwrap(),
            player: "aa:aa:00:00:00:06".into(),
            prefix: "_Bedroom_".into(),
            output_volume: None,
            now_playing: now_playing::Config::default(),
            layout: Layout::Controls,
            then: None,
            modes: None,
            tone: None,
            volume_bar: None,
            headset_hub: Some(Arc::clone(&hub)),
        };
        let mut s = ready(Vec::new());
        s.player_id = Some("aa:aa:00:00:00:06".into());
        lyrion.follow_headset(&mut s);
        assert_eq!(s.target().as_deref(), Some("aa:aa:00:00:00:06"), "the speaker");
        hub.set_player_state(streamdock_proto::streamdock::v1::PlayerState {
            enabled: true,
            connected: true,
            playing: true,
            player_id: "02:00:00:00:00:ee".into(),
            ..Default::default()
        });
        lyrion.follow_headset(&mut s);
        assert_eq!(s.target().as_deref(), Some("02:00:00:00:00:ee"), "headset mode");
        s.headset = Some(("02:00:00:00:00:ee".into(), 60));
        s.volume = Some(80);
        assert_eq!(s.shown_volume(), Some(60), "capped at max_volume");
        hub.set_player_state(streamdock_proto::streamdock::v1::PlayerState {
            enabled: true,
            connected: true,
            player_id: "02:00:00:00:00:ee".into(),
            ..Default::default()
        });
        lyrion.follow_headset(&mut s);
        assert_eq!(
            s.target().as_deref(),
            Some("aa:aa:00:00:00:06"),
            "back on the speaker"
        );
        assert_eq!(s.notes.len(), 2, "one note per switch: {:?}", s.notes);
    }

    #[test]
    fn reports_offline_and_back_online_once() {
        let lyrion = Lyrion {
            id: "lyrion".into(),
            title: "Music".into(),
            client: Client::new("http://127.0.0.1:9").unwrap(),
            player: "Buero Essence".into(),
            prefix: "_dock".into(),
            output_volume: None,
            now_playing: now_playing::Config {
                keys: Vec::new(),
                ..now_playing::Config::default()
            },
            layout: Layout::Playlists,
            then: None,
            modes: None,
            tone: None,
            volume_bar: None,
            headset_hub: None,
        };
        let mut s = State::default();
        let error = anyhow::anyhow!("connection refused");
        for _ in 0..4 {
            lyrion.unreachable(&mut s, &error);
        }
        lyrion.reachable(&mut s);
        lyrion.reachable(&mut s);
        assert_eq!(s.notes.len(), 2, "{:?}", s.notes);
        assert!(matches!(&s.notes[0], Note::Error(t) if t.starts_with("Lyrion offline")));
        assert!(matches!(&s.notes[1], Note::Info(t) if t.starts_with("Lyrion back online")));
    }

    #[test]
    fn explains_empty_offline_and_missing_player() {
        let mut s = ready(vec![]);
        assert_eq!(s.page_view().faces[0].label, "No _dock playlists");
        s.player_offline = true;
        assert_eq!(
            s.page_view().faces[0].label,
            "Player offline",
            "known to Lyrion, not connected"
        );
        s.player_missing = true;
        assert_eq!(s.page_view().faces[0].label, "Player not found");
        s.failures = OFFLINE_AFTER - 1;
        assert_eq!(
            s.page_view().faces[0].label,
            "Player not found",
            "one failed refresh is tolerated"
        );
        s.failures = OFFLINE_AFTER;
        let page = s.page_view();
        assert_eq!(page.faces[0].label, "Lyrion offline");
        assert_eq!(
            page.faces[0].state,
            face(0, "", "", FaceState::FACE_STATE_UNAVAILABLE).state
        );
    }

    #[test]
    fn config_requires_url_and_a_player_per_tab() {
        let table = |src: &str| -> toml::Table { toml::from_str(src).unwrap() };
        assert!(from_table(&table("url = \"http://lms:9000\"\nplayer = \"Buero Essence\"\n")).is_ok());
        assert!(from_table(&table("player = \"x\"\n")).is_err());
        // Only the connection (for custom keys' lyrion actions): no tab, no error.
        assert_eq!(
            from_table(&table("url = \"http://lms:9000\"\n")).unwrap().len(),
            0
        );
        assert!(from_table(&table("url = \"http://lms:9000\"\nplayer = \"x\"\ncolour = 1\n")).is_err());
    }

    #[test]
    fn music_screen_shows_transport_keys_after_the_band() {
        let mut s = ready(playlists(&["a"]));
        s.layout = Layout::Controls;
        s.band = Some(now_playing::Config::default());
        let page = s.playlist_view();
        let shown: Vec<(u32, &str)> = page.faces.iter().map(|f| (f.key, f.image.as_str())).collect();
        assert_eq!(shown, vec![(4, "previous"), (5, "play"), (6, "next")]);
        assert!(
            page.faces.iter().all(|f| f.label.is_empty()),
            "big icons, no text"
        );
        s.mode = PlayMode::Play;
        let page = s.playlist_view();
        assert_eq!(
            page.faces[1].image, "pause",
            "the middle key shows what a press does"
        );
        assert_eq!(page.knobs[0].label, "press Pause");
        s.tone_level = Some(3);
        assert_eq!(
            s.playlist_view().knobs[0].label,
            "Bass +3 · press Pause",
            "knob 1 = bass (H15-95)"
        );
        assert_eq!(s.transport_keys(), vec![4, 5, 6]);
    }

    #[test]
    fn volume_bar_after_a_change_and_always() {
        let start = tokio::time::Instant::now();
        let mut s = ready(playlists(&["a"]));
        s.layout = Layout::Controls;
        s.band = Some(now_playing::Config::default());
        s.volume_bar = Some(VolumeBar {
            key: 3,
            always: false,
        });
        s.apply_status_at(&json!({"mode": "play", "mixer volume": 40}), start);
        assert!(s.volume_face(start).is_none(), "the first value is no change");
        s.apply_status_at(&json!({"mode": "play", "mixer volume": 55}), start);
        let bar = s.volume_face(start).expect("shown after a change from elsewhere");
        assert_eq!((bar.key, bar.level), (3, Some(55)));
        let later = start + VOLUME_BAR_FOR + Duration::from_millis(1);
        assert!(s.volume_face(later).is_none(), "gone after a few seconds");
        assert_eq!(
            s.page_view_at(start).faces.iter().filter(|f| f.key == 3).count(),
            1,
            "the bar replaces the band's key 3"
        );
        s.volume_bar = Some(VolumeBar { key: 3, always: true });
        assert!(s.volume_face(later).is_some(), "always");
        s.player_offline = true;
        assert!(s.volume_face(later).is_none(), "not over a problem");
    }

    #[test]
    fn a_volume_bar_shown_always_takes_its_key_from_the_band() {
        let table: toml::Table = toml::from_str(
            "url = \"http://lms:9000\"\nplayer = \"x\"\nvolume_bar = { key = 3, show = \"always\" }\n\
             [[tab]]\nid = \"t\"\nvolume_bar = { show = \"changes\" }\n",
        )
        .unwrap();
        let (_, found) = tabs(&table).unwrap();
        assert_eq!(found[0].now_playing.keys, vec![1, 2], "band on 1-2");
        assert_eq!(
            found[1].now_playing.keys,
            vec![1, 2, 3],
            "only while changing: band keeps 1-3"
        );
        assert_eq!(
            found[1].volume_bar,
            Some(VolumeBar {
                key: 3,
                always: false
            })
        );
        let bad: toml::Table =
            toml::from_str("url = \"http://lms:9000\"\nplayer = \"x\"\nvolume_bar = { key = 9 }\n").unwrap();
        assert!(tabs(&bad).is_err());
    }

    #[test]
    fn layout_and_then_per_tab() {
        let table: toml::Table = toml::from_str(
            "url = \"http://lms:9000\"\nplayer = \"aa:aa:00:00:00:06\"\nlayout = \"controls\"\nid = \"music\"\n\
             [[tab]]\nid = \"lists\"\nprefix = \"_Bedroom_\"\nnow_playing = { keys = [] }\nthen = \"music\"\n",
        )
        .unwrap();
        let (_, found) = tabs(&table).unwrap();
        assert_eq!(
            (found[0].layout, found[0].then.as_deref()),
            (Layout::Controls, None)
        );
        assert_eq!(
            (found[1].layout, found[1].then.as_deref()),
            (Layout::Playlists, Some("music"))
        );
        assert!(found[1].now_playing.keys.is_empty(), "all 6 keys for playlists");
        let bad: toml::Table =
            toml::from_str("url = \"http://lms:9000\"\nplayer = \"x\"\nlayout = \"grid\"\n").unwrap();
        assert!(tabs(&bad).is_err());
    }

    #[test]
    fn a_tab_without_any_player_uses_the_default() {
        let table: toml::Table =
            toml::from_str("url = \"http://lms:9000\"\n[[tab]]\ntitle = \"Kitchen\"\n").unwrap();
        let (_, tabs) = tabs(&table).unwrap();
        assert_eq!(tabs.len(), 1, "url only: no top-level tab, the listed one");
        assert_eq!(tabs[0].player, DEFAULT_PLAYER);
    }
}

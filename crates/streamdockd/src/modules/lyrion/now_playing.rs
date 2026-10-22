//! Now playing (H15-88): the music tab's current track as a band of LCD keys that scrolls long
//! text (a marquee), and as [`pb::NowPlaying`] for the web UI.
//!
//! ```toml
//! [[modules.lyrion.tab]]
//! player = "Bedroom"
//! now_playing = { keys = [1, 2, 3], fields = ["title", "artist"], separator = " · ",
//!                 speed = 40, pause_ms = 1500, scroll = true, cover = true, progress = true, fps = 6 }
//! ```
//!
//! `keys = []` turns the band off on the dock (the web UI still shows the track). The band's frames
//! are rendered only while the text is too long for it and the tab is shown, at `fps` frames per
//! second; keys whose image doesn't change are not sent again.

use std::time::Duration;

use anyhow::{Context as _, bail};
use serde_json::Value;
use streamdock_proto::streamdock::v1 as pb;
use tokio::time::Instant;

use crate::modules::{FaceState, KeyFace};

/// What a field of the band shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Artist,
    Album,
}

impl Field {
    fn parse(name: &str) -> anyhow::Result<Self> {
        Ok(match name {
            "title" => Self::Title,
            "artist" => Self::Artist,
            "album" => Self::Album,
            other => bail!("now_playing: unknown field {other:?} (title, artist, album)"),
        })
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Artist => "artist",
            Self::Album => "album",
        }
    }
}

/// `now_playing = { … }` of a music tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// LCD keys of the band, left to right; empty: no band on the dock.
    pub keys: Vec<u8>,
    pub fields: Vec<Field>,
    pub separator: String,
    /// Scroll speed in pixels per second.
    pub speed: u32,
    /// Pause at each end of the text.
    pub pause: Duration,
    /// Scroll long text; `false`: cut it with "…".
    pub scroll: bool,
    pub cover: bool,
    pub progress: bool,
    /// Frames per second while scrolling (each frame re-sends the band's keys to the dock).
    pub fps: u32,
}

/// The keys the band takes when `now_playing.keys` is not set (H15-88 decision).
pub const DEFAULT_KEYS: [u8; 3] = [1, 2, 3];

impl Default for Config {
    fn default() -> Self {
        Self {
            keys: DEFAULT_KEYS.to_vec(),
            fields: vec![Field::Title, Field::Artist],
            separator: " · ".into(),
            speed: 40,
            pause: Duration::from_millis(1500),
            scroll: true,
            cover: true,
            progress: true,
            fps: 6,
        }
    }
}

impl Config {
    pub const KEYS: [&str; 9] = [
        "keys",
        "fields",
        "separator",
        "speed",
        "pause_ms",
        "scroll",
        "cover",
        "progress",
        "fps",
    ];

    /// As a `now_playing` inline table, every setting written out.
    #[must_use]
    pub fn to_inline_table(&self) -> toml_edit::InlineTable {
        let mut t = toml_edit::InlineTable::new();
        t.insert(
            "keys",
            self.keys
                .iter()
                .map(|&k| i64::from(k))
                .collect::<toml_edit::Array>()
                .into(),
        );
        t.insert(
            "fields",
            self.fields
                .iter()
                .map(|f| f.name())
                .collect::<toml_edit::Array>()
                .into(),
        );
        t.insert("separator", self.separator.as_str().into());
        t.insert("speed", i64::from(self.speed).into());
        t.insert(
            "pause_ms",
            i64::try_from(self.pause.as_millis()).unwrap_or(i64::MAX).into(),
        );
        t.insert("scroll", self.scroll.into());
        t.insert("cover", self.cover.into());
        t.insert("progress", self.progress.into());
        t.insert("fps", i64::from(self.fps).into());
        t
    }

    #[must_use]
    pub fn to_message(&self) -> pb::NowPlayingSettings {
        pb::NowPlayingSettings {
            keys: self.keys.iter().map(|&k| u32::from(k)).collect(),
            fields: self.fields.iter().map(|f| f.name().to_owned()).collect(),
            separator: self.separator.clone(),
            speed: self.speed,
            pause_ms: u32::try_from(self.pause.as_millis()).unwrap_or(u32::MAX),
            scroll: self.scroll,
            cover: self.cover,
            progress: self.progress,
            fps: self.fps,
            ..Default::default()
        }
    }

    /// From the API, checked like the config file.
    pub fn from_message(m: &pb::__buffa::view::NowPlayingSettingsView<'_>) -> anyhow::Result<Self> {
        let mut t = toml_edit::InlineTable::new();
        t.insert(
            "keys",
            m.keys
                .iter()
                .map(|&k| i64::from(k))
                .collect::<toml_edit::Array>()
                .into(),
        );
        t.insert(
            "fields",
            m.fields.iter().copied().collect::<toml_edit::Array>().into(),
        );
        t.insert("separator", m.separator.into());
        t.insert("speed", i64::from(m.speed).into());
        t.insert("pause_ms", i64::from(m.pause_ms).into());
        t.insert("scroll", m.scroll.into());
        t.insert("cover", m.cover.into());
        t.insert("progress", m.progress.into());
        t.insert("fps", i64::from(m.fps).into());
        Self::from_inline(&t)
    }

    /// Parses an inline table as written in the config.
    pub fn from_inline(t: &toml_edit::InlineTable) -> anyhow::Result<Self> {
        let table: toml::Table = toml::from_str(&format!("n = {t}"))?;
        Self::from_value(table.get("n"))
    }

    /// Reads `now_playing` (missing: the defaults).
    pub fn from_value(value: Option<&toml::Value>) -> anyhow::Result<Self> {
        let mut config = Self::default();
        let Some(value) = value else { return Ok(config) };
        let table = value.as_table().context("now_playing must be a table")?;
        for key in table.keys() {
            anyhow::ensure!(
                Self::KEYS.contains(&key.as_str()),
                "now_playing: unknown setting {key:?} (known: {})",
                Self::KEYS.join(", ")
            );
        }
        let int = |key: &str, range: std::ops::RangeInclusive<i64>| -> anyhow::Result<Option<u32>> {
            table
                .get(key)
                .map(|v| {
                    v.as_integer()
                        .filter(|n| range.contains(n))
                        .and_then(|n| u32::try_from(n).ok())
                        .with_context(|| {
                            format!("now_playing: {key} must be {}-{}", range.start(), range.end())
                        })
                })
                .transpose()
        };
        let flag = |key: &str| -> anyhow::Result<Option<bool>> {
            table
                .get(key)
                .map(|v| {
                    v.as_bool()
                        .with_context(|| format!("now_playing: {key} must be true or false"))
                })
                .transpose()
        };
        if let Some(keys) = table.get("keys") {
            let keys = keys
                .as_array()
                .context("now_playing: keys must be a list of keys 1-6")?;
            config.keys = keys
                .iter()
                .map(|k| {
                    k.as_integer()
                        .and_then(|k| u8::try_from(k).ok())
                        .filter(|k| (1..=6).contains(k))
                        .context("now_playing: keys must be LCD keys 1-6")
                })
                .collect::<anyhow::Result<_>>()?;
            let mut seen = config.keys.clone();
            seen.sort_unstable();
            seen.dedup();
            anyhow::ensure!(
                seen.len() == config.keys.len(),
                "now_playing: a key is listed twice"
            );
            anyhow::ensure!(config.keys.len() <= 3, "now_playing: at most 3 keys");
        }
        if let Some(fields) = table.get("fields") {
            config.fields = fields
                .as_array()
                .context("now_playing: fields must be a list")?
                .iter()
                .map(|f| Field::parse(f.as_str().unwrap_or_default()))
                .collect::<anyhow::Result<_>>()?;
            anyhow::ensure!(!config.fields.is_empty(), "now_playing: fields must not be empty");
        }
        if let Some(sep) = table.get("separator") {
            sep.as_str()
                .context("now_playing: separator must be a string")?
                .clone_into(&mut config.separator);
        }
        if let Some(speed) = int("speed", 5..=200)? {
            config.speed = speed;
        }
        if let Some(ms) = int("pause_ms", 0..=10_000)? {
            config.pause = Duration::from_millis(u64::from(ms));
        }
        if let Some(fps) = int("fps", 1..=10)? {
            config.fps = fps;
        }
        config.scroll = flag("scroll")?.unwrap_or(config.scroll);
        config.cover = flag("cover")?.unwrap_or(config.cover);
        config.progress = flag("progress")?.unwrap_or(config.progress);
        Ok(config)
    }
}

/// The track a player plays, from Lyrion's `status` (with `tags:aAlKcd`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Cover URL, absolute; `None` without art.
    pub cover: Option<String>,
    pub duration: Option<Duration>,
    pub elapsed: Duration,
    /// When `elapsed` was read, and the same as wall-clock time (for the web UI).
    pub at: Option<Instant>,
    pub at_unix_ms: i64,
    pub playing: bool,
    pub paused: bool,
}

fn unix_ms() -> i64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
}

fn text_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().trim().to_owned()
}

fn seconds(v: &Value) -> Option<Duration> {
    let s = v.as_f64().or_else(|| v.as_str()?.parse().ok())?;
    (s.is_finite() && s >= 0.0).then(|| Duration::from_secs_f64(s))
}

impl Track {
    /// The current track from a `status` response; `server` is the Lyrion base URL (for covers),
    /// `player` the player's MAC.
    #[must_use]
    pub fn from_status(result: &Value, server: &str, player: &str, now: Instant) -> Option<Self> {
        let song = result["playlist_loop"]
            .as_array()
            .and_then(|l| l.first())
            .unwrap_or(&Value::Null);
        // Radio streams put the current song into remoteMeta; files into playlist_loop.
        let meta = if result["remoteMeta"].is_object() {
            &result["remoteMeta"]
        } else {
            song
        };
        let pick = |key: &str| {
            let v = text_of(&meta[key]);
            if v.is_empty() { text_of(&song[key]) } else { v }
        };
        let mut title = pick("title");
        if title.is_empty() {
            title = text_of(&result["current_title"]);
        }
        if title.is_empty() {
            return None;
        }
        let mode = result["mode"].as_str().unwrap_or_default();
        Some(Self {
            title,
            artist: pick("artist"),
            album: pick("album"),
            cover: cover_url(meta, song, server, player),
            duration: seconds(&result["duration"]).filter(|d| !d.is_zero()),
            elapsed: seconds(&result["time"]).unwrap_or_default(),
            at: Some(now),
            at_unix_ms: unix_ms(),
            playing: mode == "play",
            paused: mode == "pause",
        })
    }

    /// Where the track is now: `elapsed`, counted on while playing.
    #[must_use]
    pub fn position(&self, now: Instant) -> Duration {
        let mut at = self.elapsed;
        if self.playing
            && let Some(read) = self.at
        {
            at += now.saturating_duration_since(read);
        }
        self.duration.map_or(at, |d| at.min(d))
    }

    /// Progress 0-1000, or 0 without a length.
    #[must_use]
    pub fn progress(&self, now: Instant) -> u32 {
        let Some(d) = self.duration else { return 0 };
        let p = self.position(now).as_secs_f64() / d.as_secs_f64();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "H15-88: clamped 0-1000"
        )]
        let p = (p.clamp(0.0, 1.0) * 1000.0).round() as u32;
        p
    }

    /// The configured fields, joined.
    #[must_use]
    pub fn text(&self, config: &Config) -> String {
        config
            .fields
            .iter()
            .map(|f| match f {
                Field::Title => self.title.as_str(),
                Field::Artist => self.artist.as_str(),
                Field::Album => self.album.as_str(),
            })
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(&config.separator)
    }
}

/// The cover: a remote stream's `artwork_url` (absolute, or relative to the server), else the
/// track's `coverid` (server-side scaled), else none.
fn cover_url(meta: &Value, song: &Value, server: &str, player: &str) -> Option<String> {
    let server = server.trim_end_matches('/');
    for v in [&meta["artwork_url"], &song["artwork_url"]] {
        let url = text_of(v);
        if url.starts_with("http://") || url.starts_with("https://") {
            return Some(url);
        }
        if url.starts_with('/') {
            return Some(format!("{server}{url}"));
        }
    }
    for v in [&song["coverid"], &meta["coverid"]] {
        let id = text_of(v);
        if !id.is_empty() && id != "0" && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Some(format!("{server}/music/{id}/cover_192x192_o.jpg"));
        }
    }
    let has_art = [&song["coverart"], &meta["coverart"]]
        .iter()
        .any(|v| text_of(v) == "1" || v.as_u64() == Some(1));
    has_art.then(|| format!("{server}/music/current/cover_192x192_o.jpg?player={player}"))
}

/// Scroll position of text `text_width` px wide in a band `band_width` px wide, `since` the song
/// (or the scroll) started: still at 0 for `pause`, then moving left at `speed` px/s, still at the
/// end for `pause`, then back to 0 and again.
#[must_use]
pub fn marquee_offset(since: Duration, text_width: f32, band_width: f32, speed: u32, pause: Duration) -> i32 {
    let distance = text_width - band_width;
    if distance <= 0.0 || speed == 0 {
        return 0;
    }
    let travel = f64::from(distance) / f64::from(speed);
    let cycle = pause.as_secs_f64() * 2.0 + travel;
    let t = since.as_secs_f64() % cycle;
    let moved = (t - pause.as_secs_f64()).clamp(0.0, travel) * f64::from(speed);
    #[allow(clippy::cast_possible_truncation, reason = "H15-88: pixels")]
    let px = moved.round() as i32;
    px
}

/// The band's faces: one slice per configured key.
#[must_use]
pub fn band_faces(
    config: &Config,
    text: &str,
    offset: i32,
    progress: u32,
    cover: &str,
    dim: bool,
) -> Vec<KeyFace> {
    let count = u32::try_from(config.keys.len()).unwrap_or(1);
    config
        .keys
        .iter()
        .enumerate()
        .map(|(i, &key)| KeyFace {
            key: u32::from(key),
            state: if dim {
                FaceState::FACE_STATE_IDLE
            } else {
                FaceState::FACE_STATE_ACTIVE
            }
            .into(),
            band: pb::BandSlice {
                text: text.to_owned(),
                offset,
                index: u32::try_from(i).unwrap_or(0),
                count,
                progress,
                show_progress: config.progress,
                cover: cover.to_owned(),
                dim,
                ..Default::default()
            }
            .into(),
            ..Default::default()
        })
        .collect()
}

/// The track for the web UI.
#[must_use]
pub fn message(track: Option<&Track>, config: &Config) -> pb::NowPlaying {
    let base = pb::NowPlaying {
        keys: config.keys.iter().map(|&k| u32::from(k)).collect(),
        scroll_px_per_second: config.speed,
        scroll_pause_ms: u32::try_from(config.pause.as_millis()).unwrap_or(u32::MAX),
        scroll: config.scroll,
        show_cover: config.cover,
        show_progress: config.progress,
        ..Default::default()
    };
    let Some(t) = track else {
        return pb::NowPlaying { idle: true, ..base };
    };
    let ms = |d: Duration| u32::try_from(d.as_millis()).unwrap_or(u32::MAX);
    pb::NowPlaying {
        title: t.title.clone(),
        artist: t.artist.clone(),
        album: t.album.clone(),
        text: t.text(config),
        cover_url: if config.cover {
            t.cover.clone().unwrap_or_default()
        } else {
            String::new()
        },
        duration_ms: t.duration.map_or(0, ms),
        elapsed_ms: ms(t.elapsed),
        // The position at the last read and its wall-clock time: the UI counts on by itself, and
        // the message changes only when the server says something new.
        elapsed_at_unix_ms: t.at_unix_ms,
        playing: t.playing,
        paused: t.paused,
        ..base
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn config() {
        assert_eq!(Config::from_value(None).unwrap(), Config::default());
        let v: toml::Value = toml::from_str::<toml::Table>(
            r#"now_playing = { keys = [4, 5], fields = ["artist", "album", "title"], separator = " / ", speed = 60, pause_ms = 500, scroll = false, cover = false, progress = false, fps = 4 }"#,
        )
        .unwrap()["now_playing"]
            .clone();
        let c = Config::from_value(Some(&v)).unwrap();
        assert_eq!(c.keys, [4, 5]);
        assert_eq!(c.fields, [Field::Artist, Field::Album, Field::Title]);
        assert_eq!((c.separator.as_str(), c.speed, c.fps), (" / ", 60, 4));
        assert_eq!(c.pause, Duration::from_millis(500));
        assert!(!c.scroll && !c.cover && !c.progress);
        let off: toml::Value = toml::from_str::<toml::Table>("n = { keys = [] }").unwrap()["n"].clone();
        assert_eq!(Config::from_value(Some(&off)).unwrap().keys, Vec::<u8>::new());
        for bad in [
            "n = { keys = [7] }",
            "n = { keys = [1, 1] }",
            "n = { keys = [1, 2, 3, 4] }",
            "n = { fields = [\"genre\"] }",
            "n = { fields = [] }",
            "n = { speed = 0 }",
            "n = { fps = 30 }",
            "n = { colour = \"red\" }",
        ] {
            let v = toml::from_str::<toml::Table>(bad).unwrap()["n"].clone();
            assert!(Config::from_value(Some(&v)).is_err(), "{bad}");
        }
    }

    #[test]
    fn inline_table_round_trip() {
        let c = Config {
            keys: vec![4, 5, 6],
            fields: vec![Field::Album, Field::Title],
            separator: " — ".into(),
            speed: 25,
            pause: Duration::from_millis(800),
            scroll: false,
            cover: false,
            progress: true,
            fps: 4,
        };
        assert_eq!(Config::from_inline(&c.to_inline_table()).unwrap(), c);
    }

    #[test]
    fn track_from_status() {
        let status: Value = serde_json::from_str(include_str!("fixtures/status_playing.json")).unwrap();
        let result = status.get("result").unwrap_or(&status);
        let now = Instant::now();
        let t = Track::from_status(result, "http://lms:9000", "aa:bb", now).unwrap();
        assert_eq!(t.title, "Kapitel 20 - Folge 10: Die Nebelinsel");
        assert_eq!(t.artist, "Die Nebelbande");
        assert_eq!(t.album, "Folge 10: Die Nebelinsel");
        assert_eq!(t.cover, None, "coverart 0: no cover");
        assert_eq!(t.duration, Some(Duration::from_secs_f64(120.01)));
        assert!(t.playing && !t.paused);
        assert_eq!(
            t.text(&Config::default()),
            "Kapitel 20 - Folge 10: Die Nebelinsel · Die Nebelbande"
        );
        // Position counts on while playing, stops at the end.
        assert_eq!(
            t.position(now + Duration::from_secs(10)),
            Duration::from_secs_f64(12.535)
        );
        assert_eq!(
            t.position(now + Duration::from_secs(1000)),
            Duration::from_secs_f64(120.01)
        );
        assert_eq!(t.progress(now), 21);
        // Nothing loaded: no track.
        assert!(Track::from_status(&json!({ "mode": "stop" }), "http://lms", "aa", now).is_none());
    }

    #[test]
    fn covers() {
        let now = Instant::now();
        let file = json!({ "mode": "play", "playlist_loop": [{ "title": "T", "coverid": "ab12cd" }] });
        assert_eq!(
            Track::from_status(&file, "http://lms:9000/", "aa", now)
                .unwrap()
                .cover
                .as_deref(),
            Some("http://lms:9000/music/ab12cd/cover_192x192_o.jpg")
        );
        let radio = json!({ "mode": "play", "remoteMeta": { "title": "R", "artwork_url": "/imageproxy/x/image.png" } });
        assert_eq!(
            Track::from_status(&radio, "http://lms:9000", "aa", now)
                .unwrap()
                .cover
                .as_deref(),
            Some("http://lms:9000/imageproxy/x/image.png")
        );
        let current = json!({ "mode": "pause", "playlist_loop": [{ "title": "T", "coverart": "1" }] });
        let t = Track::from_status(&current, "http://lms:9000", "aa:bb", now).unwrap();
        assert_eq!(
            t.cover.as_deref(),
            Some("http://lms:9000/music/current/cover_192x192_o.jpg?player=aa:bb")
        );
        assert!(t.paused && !t.playing);
        assert_eq!(
            t.position(now + Duration::from_secs(5)),
            Duration::ZERO,
            "paused: no counting"
        );
    }

    #[test]
    fn marquee() {
        let pause = Duration::from_secs(1);
        // Fits: never moves.
        assert_eq!(marquee_offset(Duration::from_secs(3), 100.0, 180.0, 40, pause), 0);
        // 80 px too long at 40 px/s: 1 s still, 2 s moving, 1 s still at the end, then again.
        let at = |s: f64| marquee_offset(Duration::from_secs_f64(s), 260.0, 180.0, 40, pause);
        assert_eq!(at(0.5), 0);
        assert_eq!(at(1.5), 20);
        assert_eq!(at(3.0), 80);
        assert_eq!(at(3.5), 80);
        assert_eq!(at(4.0), 0, "back to the start");
        assert_eq!(at(5.5), 20);
    }

    #[test]
    fn band() {
        let c = Config::default();
        let faces = band_faces(&c, "Song · Artist", 12, 500, "c1", false);
        assert_eq!(faces.iter().map(|f| f.key).collect::<Vec<_>>(), [1, 2, 3]);
        let b = faces[2].band.as_option().unwrap();
        assert_eq!((b.index, b.count, b.offset, b.progress), (2, 3, 12, 500));
        let m = message(None, &c);
        assert!(m.idle);
        assert_eq!(m.keys, [1, 2, 3]);
    }
}

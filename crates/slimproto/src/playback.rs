//! The interface between the protocol (this crate) and the audio side (decode + output), agreed on
//! H15-47. The protocol side calls these methods as the server's `strm`, `audg` and `aude` commands
//! arrive, and reads [`Playback::status`] once per status cycle (every 100 ms) to report `STAT`.

use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::mpsc;

/// The audio side of the player. Methods return at once; the work happens on the
/// implementation's own threads.
pub trait Playback: Send + Sync {
    /// `strm s`: a new track. `source` yields the HTTP body after the response headers, as it
    /// arrives; it closes at the end of the stream. A start while a track still plays is gapless:
    /// the new track plays after the buffered audio, it never flushes it.
    fn start(&self, track: Track, source: mpsc::Receiver<Bytes>);

    /// `strm p` with interval 0: stop the output now, keep every buffer.
    fn pause(&self);

    /// `strm u`: resume now (`None`) or at the given local time (sync start).
    fn unpause(&self, at: Option<Instant>);

    /// `strm q`: stop. Flush the decoder and the whole output buffer, reset elapsed time.
    fn stop(&self);

    /// `strm f`: drop the next, not yet playing track; the current one keeps playing out of the
    /// output buffer.
    fn flush_next(&self);

    /// `strm a`: drop this much decoded audio from the output buffer without playing it.
    fn skip(&self, ahead: Duration);

    /// `audg`: the server's volume. `left`/`right` are linear gains (1.0 = 0 dB, balance
    /// included); `percent` is the volume as Lyrion shows it (0-100). Not called for `audg` with the
    /// adjust flag off (fixed volume in Lyrion): then the player's own volume stays as it is.
    fn volume(&self, left: f32, right: f32, percent: u32);

    /// `aude`: the outputs on or off (the player's power).
    fn enable(&self, on: bool);

    /// What the status cycle reports. Called only from the 100 ms status loop, never per packet;
    /// events are those since the previous call.
    fn status(&self) -> PlaybackStatus;
}

/// A track to play, from `strm s`.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub format: Format,
    /// Replay gain for this track, linear (1.0 = none). Applies from its first sample on.
    pub replay_gain: f32,
    /// Start playing by itself once `output_threshold` is buffered (`strm` autostart 1/3); else
    /// wait for [`Playback::unpause`].
    pub autostart: bool,
    /// Output buffer to fill before playback starts.
    pub output_threshold: Duration,
    /// How the track blends into the previous one (Lyrion's player prefs `transitionType` and
    /// `transitionDuration`, sent with every `strm s`).
    pub transition: Transition,
}

/// A track transition (`strm`'s transition type and period).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Transition {
    pub kind: TransitionKind,
    pub duration: Duration,
}

/// Lyrion's `transitionType`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TransitionKind {
    #[default]
    None,
    /// Mix the end of the previous track into the start of this one.
    Crossfade,
    FadeIn,
    FadeOut,
    FadeInOut,
}

/// The stream's codec, from `strm`'s format byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Raw PCM; the parameters come from `strm` (or the WAV/AIFF header when the server says
    /// "from header": then the fields are `None`).
    Pcm {
        sample_rate: Option<u32>,
        /// Bits per sample: 8, 16, 24 or 32.
        sample_size: Option<u8>,
        channels: Option<u8>,
        big_endian: Option<bool>,
    },
    Flac,
    Mp3,
}

/// What [`Playback::status`] reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaybackStatus {
    /// Playing time of the current track: frames played minus the output device's delay.
    pub elapsed: Duration,
    /// Output buffer: bytes (or frames, consistently) filled and its size.
    pub output_fill: u32,
    pub output_size: u32,
    /// Decoder input buffer: filled and size.
    pub decode_fill: u32,
    pub decode_size: u32,
    /// Whether the output is paused.
    pub paused: bool,
    /// What happened since the previous call, oldest first.
    pub events: Vec<PlaybackEvent>,
    /// The sample rate of the audio playing now, as decoded; 0 if unknown (H15-73).
    pub sample_rate: u32,
}

/// Something the audio side reports, turned into a `STAT` event by the protocol side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackEvent {
    /// The track's first sample reached the output (`STMs`).
    TrackStarted,
    /// The decoder consumed the whole stream (`STMd`): the server may send the next track.
    DecodeDone,
    /// The output ran dry before the track's end (`STMo`) …
    OutputUnderrun,
    /// … or at the end of the last track (`STMu`): playback is over.
    PlaybackDone,
    /// With autostart off, the output threshold is buffered (`STMl`).
    Buffered,
    /// The decoder could not handle the stream (`STMn`).
    DecodeError(String),
}

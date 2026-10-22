//! The built-in Lyrion player's audio side (H15-47): [`decode`] turns the HTTP body of a track into
//! interleaved `f32` frames (H15-56) and pushes them into a [`FrameSink`]; the output side drains
//! that into PipeWire. The protocol half is the `slimproto` crate.

pub mod decode;
pub mod dsp;
pub mod output;
mod player;

pub use decode::{Decoder, DecoderConfig, TrackId};
pub use player::{Player, VolumeHandler};

/// Where the decoder pushes its frames: the output side's buffer. Called from the decoder thread.
pub trait FrameSink: Send {
    /// A new track starts with the next frame [`FrameSink::write`] gets, at this rate and channel
    /// count. Called at the start of every track, also when the format did not change: the output
    /// side reports `TrackStarted` and resets elapsed time when it plays that frame. `track` is
    /// what [`Decoder::start`] returned, for [`Decoder::flush_next`].
    fn begin_track(&mut self, track: TrackId, rate: u32, channels: u16);

    /// How `track` blends into the previous one (crossfade, fades; H15-64). Called before the
    /// decoder starts on it, so before its [`FrameSink::begin_track`].
    fn transition(&mut self, _track: TrackId, _transition: slimproto::Transition) {}

    /// Interleaved samples, a whole number of frames, replay gain applied. Blocks while the
    /// buffer is full (back pressure, never drops); returns [`Closed`] once the output side
    /// flushed (stop): the decoder then abandons the track.
    fn write(&mut self, frames: &[f32]) -> Result<(), Closed>;

    /// The track's stream ended cleanly after the last [`FrameSink::write`]: running dry after
    /// this marker is the end of playback (`PlaybackDone`), before it an underrun. Not called
    /// when the track failed or was cancelled.
    fn end_track(&mut self) {}
}

/// The output side no longer takes frames for this track (it was stopped or flushed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("frame sink closed")]
pub struct Closed;

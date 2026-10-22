//! Decoder (H15-56): FLAC, MP3 and PCM (raw, WAV, AIFF) to interleaved `f32` frames.
//!
//! [`Decoder::spawn`] starts one thread that decodes tracks one after the other into a
//! [`FrameSink`]. A [`Decoder::start`] while a track still decodes queues the new one after it
//! (gapless: its frames follow the previous track's in the sink). Replay gain is applied in the
//! samples; the server volume is not (that is the output's job).

mod pcm;
mod source;
mod track;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::{fmt, io, thread};

use bytes::Bytes;
use slimproto::{PlaybackEvent, Track};
use tokio::sync::mpsc;

use crate::FrameSink;
use source::{Cancel, StreamSource};
use track::{Outcome, Scratch};

/// Identifies a track given to [`Decoder::start`]; increases with every start (from 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackId(pub u64);

impl TrackId {
    /// An id made up outside the decoder: tests and examples that feed a sink directly.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_raw(id: u64) -> Self {
        Self(id)
    }
}

/// Settings of a [`Decoder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderConfig {
    /// Compressed input the decoder reads ahead of decoding, in bytes: the decode buffer of
    /// `PlaybackStatus` (squeezelite: 2 MiB).
    pub input_buffer: u32,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self {
            input_buffer: 2 * 1024 * 1024,
        }
    }
}

/// The decoder thread. Dropping it cancels every track and joins the thread, so a sink blocked in
/// [`FrameSink::write`] must be closed (return [`crate::Closed`]) first.
pub struct Decoder {
    shared: Arc<Shared>,
    input_buffer: u32,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for Decoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Decoder")
            .field("input_buffer", &self.input_buffer)
            .finish_non_exhaustive()
    }
}

struct Job {
    id: TrackId,
    track: Track,
    source: mpsc::Receiver<Bytes>,
    cancel: Arc<Cancel>,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Job>,
    /// The track being decoded.
    current: Option<(TrackId, Arc<Cancel>)>,
    next_id: u64,
    events: Vec<PlaybackEvent>,
    shutdown: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    fill: AtomicU32,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Decoder {
    /// Starts the decoder thread, writing into `sink`.
    pub fn spawn(sink: impl FrameSink + 'static, config: DecoderConfig) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        // Only waits for body chunks (or the cancel signal) on the decoder thread.
        let rt = tokio::runtime::Builder::new_current_thread().build()?;
        let thread = thread::Builder::new().name("player-decode".into()).spawn({
            let shared = Arc::clone(&shared);
            move || run(&shared, &rt, Box::new(sink), config)
        })?;
        Ok(Self {
            shared,
            input_buffer: config.input_buffer,
            thread: Some(thread),
        })
    }

    /// `strm s`: decodes `track` from `source` (the HTTP body) after the tracks before it.
    #[must_use]
    pub fn start(&self, track: Track, source: mpsc::Receiver<Bytes>) -> TrackId {
        let mut state = self.shared.lock();
        let id = TrackId(state.next_id);
        state.next_id += 1;
        state.queue.push_back(Job {
            id,
            track,
            source,
            cancel: Arc::default(),
        });
        drop(state);
        self.shared.wake.notify_one();
        id
    }

    /// `strm q`: cancels the track being decoded and every queued one.
    pub fn stop(&self) {
        self.cancel_after(None);
    }

    /// `strm f`: cancels every track started after `playing`, the track the output plays now
    /// (`None`: all tracks). The output drops their frames from `begin_track(id > playing)` on.
    pub fn flush_next(&self, playing: Option<TrackId>) {
        self.cancel_after(playing);
    }

    fn cancel_after(&self, keep: Option<TrackId>) {
        let after = |id: TrackId| keep.is_none_or(|keep| id > keep);
        let mut state = self.shared.lock();
        state.queue.retain(|job| !after(job.id));
        if let Some((id, cancel)) = &state.current
            && after(*id)
        {
            cancel.cancel();
        }
    }

    /// Bytes buffered ahead of the decoder, and the buffer size (`decode_fill`, `decode_size`).
    #[must_use]
    pub fn input_fill(&self) -> (u32, u32) {
        let fill = self.shared.fill.load(Ordering::Relaxed);
        (fill.min(self.input_buffer), self.input_buffer)
    }

    /// `DecodeDone` and `DecodeError` since the last call, oldest first.
    #[must_use]
    pub fn take_events(&self) -> Vec<PlaybackEvent> {
        std::mem::take(&mut self.shared.lock().events)
    }

    /// Whether a track is being decoded or waits to be.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        let state = self.shared.lock();
        state.current.is_some() || !state.queue.is_empty()
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.stop();
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_one();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("decoder thread panicked");
        }
    }
}

/// Waits for the next job; `None` at shutdown.
fn next_job(shared: &Shared) -> Option<Job> {
    let mut state = shared.lock();
    loop {
        if state.shutdown {
            return None;
        }
        if let Some(job) = state.queue.pop_front() {
            state.current = Some((job.id, Arc::clone(&job.cancel)));
            return Some(job);
        }
        state = shared.wake.wait(state).unwrap_or_else(PoisonError::into_inner);
    }
}

fn run(shared: &Shared, rt: &tokio::runtime::Runtime, mut sink: Box<dyn FrameSink>, config: DecoderConfig) {
    let mut scratch = Scratch::default();
    while let Some(job) = next_job(shared) {
        let source = StreamSource::new(
            job.source,
            config.input_buffer as usize,
            &shared.fill,
            &job.cancel,
            rt,
        );
        sink.transition(job.id, job.track.transition);
        let outcome = track::decode(
            job.id,
            job.track.format,
            job.track.replay_gain,
            &job.cancel,
            source,
            sink.as_mut(),
            &mut scratch,
        );
        shared.fill.store(0, Ordering::Relaxed);
        let mut state = shared.lock();
        state.current = None;
        let event = match outcome {
            _ if job.cancel.is_cancelled() => None,
            Outcome::Done => Some(PlaybackEvent::DecodeDone),
            Outcome::Cancelled => None,
            Outcome::Failed(msg) => {
                tracing::warn!(track = job.id.0, %msg, "decode failed");
                Some(PlaybackEvent::DecodeError(msg))
            }
        };
        state.events.extend(event);
    }
}

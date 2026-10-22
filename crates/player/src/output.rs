//! The output side (H15-47): a buffer of decoded frames that the decoder fills through
//! [`OutputSink`] and an output thread drains into a [`Device`] (on the board: native DSP and
//! `pw-cat` into the selected PipeWire output). The thread paces itself on the device: writes block while the
//! device is full, so it never spins. While paused, waiting or between tracks it writes silence, so
//! the device never starves (a starved `pw-cat` would stall its PipeWire node); after
//! [`IDLE_CLOSE`] without a track it closes the device.
//!
//! Track boundaries travel in the buffer as marks, so `TrackStarted`, elapsed time and the end of
//! playback are exact to the frame.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use slimproto::playback::PlaybackEvent;

use crate::{Closed, FrameSink, TrackId};

pub mod resample;
#[cfg(target_os = "linux")]
mod scheduling;
mod transition;

pub use transition::{Transition, TransitionKind};

/// Frames per write to the device: 10 ms at 44.1 kHz.
pub(crate) const CHUNK_FRAMES: usize = 441;
/// Without a track for this long the device is closed (its PipeWire node goes away).
pub const IDLE_CLOSE: Duration = Duration::from_secs(5);

/// Where the frames go: interleaved little-endian `f32` at one rate and channel count.
pub trait Device: Send + 'static {
    /// Opens the device for this format, closing a previous one first.
    fn open(&mut self, rate: u32, channels: u16) -> io::Result<()>;
    /// Writes interleaved little-endian `f32` samples; blocks while the device is full.
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Float block route; legacy devices may implement only the byte writer.
    fn write_frames(&mut self, samples: &mut [f32]) -> io::Result<()> {
        let bytes: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        self.write(&bytes)
    }
    /// Discard processing history after a stop or seek.
    fn reset(&mut self) {}
    /// Frames written but not heard yet (device buffer plus its latency).
    fn delay_frames(&self) -> u32;
    /// Lets the device play out what it has and closes it.
    fn close(&mut self);
    fn is_open(&self) -> bool;
}

/// What happens with a new track while nothing plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Start {
    /// Buffer this many frames before playing (`Track::output_threshold`).
    pub threshold_frames: usize,
    /// Play once buffered; else report `Buffered` and wait for [`Output::unpause`].
    pub autostart: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkKind {
    Begin {
        track: TrackId,
        rate: u32,
        channels: u16,
    },
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mark {
    /// Absolute sample index (counted since the output started) the mark sits before.
    at: u64,
    kind: MarkKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Nothing to play.
    Idle,
    /// Buffering up to the threshold.
    Waiting(Start),
    /// Buffered, autostart off: wait for unpause.
    Ready,
    Playing,
    Paused,
    /// Unpause at that time (sync start).
    StartAt(Instant),
}

#[derive(Debug)]
struct State {
    samples: VecDeque<f32>,
    capacity: usize,
    marks: VecDeque<Mark>,
    /// Absolute sample indices of the buffer's front and back.
    read: u64,
    write: u64,
    /// Bumped by stop and flush: writers of an older generation get [`Closed`].
    generation: u64,
    phase: Phase,
    enabled: bool,
    /// The format being played (from the last Begin mark the output passed).
    format: Option<(u32, u16)>,
    /// The decoder's id of the track being played.
    playing: Option<TrackId>,
    /// Frames of the current track handed to the device.
    track_frames: u64,
    /// The device's delay in frames, as of the last write.
    delay: u32,
    /// The current track's End mark was passed: running dry now means playback is over.
    ended: bool,
    events: Vec<PlaybackEvent>,
    /// The last time a track was playing or buffered (for [`IDLE_CLOSE`]).
    active_at: Instant,
    shutdown: bool,
    reset_device: Option<()>,
    /// Transitions announced for tracks the decoder has not begun yet (H15-64).
    transitions: Vec<(TrackId, Transition)>,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    /// Signalled when space frees up (the writer waits on it).
    space: Condvar,
    /// Signalled on new frames or a command (the output thread waits on it while idle).
    work: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("output state poisoned")
    }
}

/// The output: control from the protocol side, status for `STAT`.
#[derive(Debug, Clone)]
pub struct Output {
    shared: Arc<Shared>,
}

/// The decoder's end of the buffer.
#[derive(Debug)]
pub struct OutputSink {
    shared: Arc<Shared>,
    generation: u64,
    fades: transition::Fades,
}

/// What the output reports to [`slimproto::playback::Playback::status`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputStatus {
    pub elapsed: Duration,
    /// Buffered frames and the buffer's size, in frames of the current format (stereo when none).
    pub fill: u32,
    pub size: u32,
    pub paused: bool,
    pub events: Vec<PlaybackEvent>,
    /// The sample rate of what plays now; 0 before the first track (H15-73).
    pub sample_rate: u32,
}

impl Output {
    /// Starts the output thread with a buffer of `buffer` at 44.1 kHz stereo.
    pub fn spawn(device: impl Device, buffer: Duration) -> io::Result<(Self, OutputSink)> {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "seconds of audio"
        )]
        let capacity = (buffer.as_secs_f64() * 44_100.0 * 2.0) as usize;
        let shared = Arc::new(Shared {
            state: Mutex::new(State::new(capacity)),
            space: Condvar::new(),
            work: Condvar::new(),
        });
        let thread = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("player-output".into())
            .spawn(move || run(&thread, device))?;
        let sink = OutputSink {
            shared: Arc::clone(&shared),
            generation: 0,
            fades: transition::Fades::default(),
        };
        Ok((Self { shared }, sink))
    }

    /// A track was announced (`strm s`). When nothing plays, the output buffers up to the
    /// threshold first; while a track plays, the new one follows gaplessly and this does nothing.
    pub fn prepare(&self, start: Start) {
        let mut state = self.shared.lock();
        if matches!(state.phase, Phase::Idle) {
            state.phase = Phase::Waiting(start);
            state.active_at = Instant::now();
        }
        self.shared.work.notify_all();
    }

    pub fn pause(&self) {
        let mut state = self.shared.lock();
        if matches!(state.phase, Phase::Playing | Phase::StartAt(_)) {
            state.phase = Phase::Paused;
        }
    }

    /// Resumes now (`None`) or at a local time.
    pub fn unpause(&self, at: Option<Instant>) {
        let mut state = self.shared.lock();
        if matches!(state.phase, Phase::Paused | Phase::Ready | Phase::Waiting(_)) {
            state.phase = at.map_or(Phase::Playing, Phase::StartAt);
        }
        self.shared.work.notify_all();
    }

    /// Drops every buffered frame and track; elapsed time starts over.
    pub fn stop(&self) {
        let mut state = self.shared.lock();
        state.clear();
        drop(state);
        self.shared.space.notify_all();
    }

    /// Drops the next track if it has not started playing; the current one plays on.
    pub fn flush_next(&self) {
        let mut state = self.shared.lock();
        let read = state.read;
        let next = state
            .marks
            .iter()
            .find(|m| m.at > read && matches!(m.kind, MarkKind::Begin { .. }))
            .map(|m| m.at);
        if let Some(at) = next {
            state.truncate(at);
            drop(state);
            self.shared.space.notify_all();
        }
    }

    /// Drops this much audio of the current track without playing it (`strm a`).
    pub fn skip(&self, ahead: Duration) {
        let mut state = self.shared.lock();
        let (rate, channels) = state.format.unwrap_or((44_100, 2));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "frames of audio"
        )]
        let frames = (ahead.as_secs_f64() * f64::from(rate)) as u64;
        state.drop_front(frames * u64::from(channels));
        drop(state);
        self.shared.space.notify_all();
    }

    /// The outputs on or off (`aude`): off plays nothing and closes the device.
    pub fn enable(&self, on: bool) {
        let mut state = self.shared.lock();
        state.enabled = on;
        self.shared.work.notify_all();
    }

    /// The decoder's id of the track being played (for [`crate::Decoder::flush_next`]).
    #[must_use]
    pub fn playing(&self) -> Option<TrackId> {
        self.shared.lock().playing
    }

    #[must_use]
    pub fn status(&self) -> OutputStatus {
        let mut state = self.shared.lock();
        let (rate, channels) = state.format.unwrap_or((44_100, 2));
        let heard = state.track_frames.saturating_sub(u64::from(state.delay));
        #[allow(clippy::cast_precision_loss, reason = "frames of one track")]
        let elapsed = Duration::from_secs_f64(heard as f64 / f64::from(rate));
        let per_frame = usize::from(channels.max(1));
        OutputStatus {
            elapsed,
            fill: u32::try_from(state.samples.len() / per_frame).unwrap_or(u32::MAX),
            size: u32::try_from(state.capacity / per_frame).unwrap_or(u32::MAX),
            paused: matches!(state.phase, Phase::Paused | Phase::Ready),
            events: std::mem::take(&mut state.events),
            sample_rate: state.format.map_or(0, |(rate, _)| rate),
        }
    }

    /// Stops the output thread (the device plays out and closes).
    pub fn shutdown(&self) {
        let mut state = self.shared.lock();
        state.shutdown = true;
        state.clear();
        drop(state);
        self.shared.work.notify_all();
        self.shared.space.notify_all();
    }
}

impl FrameSink for OutputSink {
    fn begin_track(&mut self, track: TrackId, rate: u32, channels: u16) {
        let mut state = self.shared.lock();
        self.generation = state.generation;
        let transition = state
            .transitions
            .iter()
            .position(|(id, _)| *id == track)
            .map(|i| state.transitions.swap_remove(i).1)
            .unwrap_or_default();
        let at = self.fades.begin(&mut state, transition, rate, channels);
        transition::insert_mark(
            &mut state,
            Mark {
                at,
                kind: MarkKind::Begin {
                    track,
                    rate,
                    channels,
                },
            },
        );
        self.shared.work.notify_all();
    }

    fn write(&mut self, mut frames: &[f32]) -> Result<(), Closed> {
        while !frames.is_empty() {
            let mut state = self.shared.lock();
            loop {
                if state.generation != self.generation || state.shutdown {
                    return Err(Closed);
                }
                let mixed = self.fades.mix(&mut state, frames);
                frames = &frames[mixed..];
                if frames.is_empty() || state.samples.len() < state.capacity {
                    break;
                }
                state = self.shared.space.wait(state).expect("output state poisoned");
            }
            let n = frames.len().min(state.capacity - state.samples.len());
            state.samples.extend(self.fades.faded(&frames[..n]));
            state.write += n as u64;
            frames = &frames[n..];
            self.shared.work.notify_all();
        }
        Ok(())
    }

    fn transition(&mut self, track: TrackId, transition: Transition) {
        let mut state = self.shared.lock();
        state.transitions.retain(|(id, _)| *id != track);
        if transition.kind != TransitionKind::None {
            state.transitions.push((track, transition));
        }
    }

    fn end_track(&mut self) {
        let mut state = self.shared.lock();
        if state.generation == self.generation {
            self.fades.end(&mut state);
            let at = state.write;
            state.marks.push_back(Mark {
                at,
                kind: MarkKind::End,
            });
        }
        self.shared.work.notify_all();
    }
}

impl State {
    fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
            marks: VecDeque::new(),
            read: 0,
            write: 0,
            generation: 0,
            phase: Phase::Idle,
            enabled: true,
            format: None,
            playing: None,
            track_frames: 0,
            delay: 0,
            ended: false,
            events: Vec::new(),
            active_at: Instant::now(),
            shutdown: false,
            reset_device: None,
            transitions: Vec::new(),
        }
    }

    fn clear(&mut self) {
        self.reset_device = Some(());
        self.samples.clear();
        self.marks.clear();
        self.transitions.clear();
        self.read = self.write;
        self.generation += 1;
        self.phase = Phase::Idle;
        self.playing = None;
        self.track_frames = 0;
        self.ended = false;
    }

    /// Drops everything from absolute sample `at` on.
    fn truncate(&mut self, at: u64) {
        let keep = usize::try_from(at.saturating_sub(self.read)).unwrap_or(usize::MAX);
        self.samples.truncate(keep);
        // An End mark at `at` closes the kept track.
        self.marks
            .retain(|m| m.at < at || (m.at == at && m.kind == MarkKind::End));
        self.write = at.max(self.read);
        self.generation += 1;
    }

    /// Drops up to `n` samples from the front, not past the next mark.
    fn drop_front(&mut self, n: u64) {
        let limit = self.marks.front().map_or(self.write, |m| m.at.max(self.read));
        let n = n.min(limit - self.read);
        self.samples.drain(..usize::try_from(n).unwrap_or(usize::MAX));
        self.read += n;
    }

    /// Handles the marks at the read position. Returns the format the next frames need when it
    /// differs from the open one (the caller reopens the device first).
    fn pass_marks(&mut self) -> Option<(u32, u16)> {
        while let Some(mark) = self.marks.front().copied() {
            if mark.at > self.read {
                break;
            }
            match mark.kind {
                MarkKind::Begin {
                    track,
                    rate,
                    channels,
                } => {
                    if self.format != Some((rate, channels)) && self.format.is_some() && self.track_frames > 0
                    {
                        // The device must play out the old format first: reopen, then come back.
                        self.format = Some((rate, channels));
                        return Some((rate, channels));
                    }
                    self.marks.pop_front();
                    let reopen = self.format != Some((rate, channels));
                    self.format = Some((rate, channels));
                    self.playing = Some(track);
                    self.track_frames = 0;
                    self.ended = false;
                    self.events.push(PlaybackEvent::TrackStarted);
                    if reopen {
                        return Some((rate, channels));
                    }
                }
                MarkKind::End => {
                    self.marks.pop_front();
                    self.ended = true;
                }
            }
        }
        None
    }

    /// Frames buffered ahead of the read position (in the current or upcoming format).
    fn buffered_frames(&self) -> usize {
        let channels = self
            .marks
            .iter()
            .find_map(|m| match m.kind {
                MarkKind::Begin { channels, .. } => Some(channels),
                MarkKind::End => None,
            })
            .or(self.format.map(|f| f.1))
            .unwrap_or(2);
        self.samples.len() / usize::from(channels.max(1))
    }
}

/// What the output thread does next.
#[derive(Debug, PartialEq)]
enum Step {
    /// Write these frames (frames or silence) to the device.
    Write,
    /// Open the device for this format, then continue.
    Open(u32, u16),
    /// Close the device and wait for work.
    Close,
    Reset,
    Exit,
}

fn run(shared: &Shared, mut device: impl Device) {
    #[cfg(target_os = "linux")]
    scheduling::promote();
    let mut frames = Vec::with_capacity(CHUNK_FRAMES * 8);
    loop {
        let step = next(shared, device.is_open(), &mut frames);
        let result = match step {
            Step::Write => device.write_frames(&mut frames),
            Step::Open(rate, channels) => {
                frames.reserve(CHUNK_FRAMES * usize::from(channels));
                device.open(rate, channels)
            }
            Step::Reset => {
                device.reset();
                Ok(())
            }
            Step::Close => {
                device.close();
                Ok(())
            }
            Step::Exit => {
                device.close();
                return;
            }
        };
        let mut state = shared.lock();
        state.delay = if device.is_open() {
            device.delay_frames()
        } else {
            0
        };
        if let Err(e) = result {
            tracing::warn!(error = %e, "player output device failed");
            device.close();
            state.events.push(PlaybackEvent::OutputUnderrun);
            drop(state);
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

/// Decides the next step under the lock and fills `frames` for [`Step::Write`].
fn next(shared: &Shared, open: bool, frames: &mut Vec<f32>) -> Step {
    let mut state = shared.lock();
    loop {
        if state.shutdown {
            return Step::Exit;
        }
        if state.reset_device.take().is_some() {
            return Step::Reset;
        }
        let now = Instant::now();
        if let Phase::StartAt(at) = state.phase
            && now >= at
        {
            state.phase = Phase::Playing;
        }
        if let Phase::Waiting(start) = state.phase
            && (state.buffered_frames() >= start.threshold_frames.max(1) || ended_early(&state))
        {
            if start.autostart {
                state.phase = Phase::Playing;
            } else {
                state.phase = Phase::Ready;
                state.events.push(PlaybackEvent::Buffered);
            }
        }
        if !state.enabled {
            if open {
                return Step::Close;
            }
            state = shared.work.wait(state).expect("output state poisoned");
            continue;
        }
        if state.phase == Phase::Playing {
            if let Some((rate, channels)) = state.pass_marks() {
                return Step::Open(rate, channels);
            }
            if !open && let Some((rate, channels)) = state.format {
                return Step::Open(rate, channels);
            }
            if !state.samples.is_empty() {
                state.active_at = now;
                fill_frames(&mut state, frames);
                drop(state);
                shared.space.notify_all();
                return Step::Write;
            }
            // Ran dry.
            if state.ended && state.marks.is_empty() {
                state.events.push(PlaybackEvent::PlaybackDone);
                state.phase = Phase::Idle;
            } else {
                tracing::warn!("player output buffer underrun");
                state.events.push(PlaybackEvent::OutputUnderrun);
                state.phase = Phase::Waiting(Start {
                    threshold_frames: CHUNK_FRAMES * 10,
                    autostart: true,
                });
            }
            continue;
        }
        let waiting = !matches!(state.phase, Phase::Idle);
        if open && (waiting || now.duration_since(state.active_at) < IDLE_CLOSE) {
            if waiting {
                state.active_at = now;
            }
            let channels = state.format.map_or(2, |f| f.1);
            fill_silence(frames, channels);
            return Step::Write;
        }
        if open {
            return Step::Close;
        }
        let timeout = match state.phase {
            Phase::StartAt(at) => at.saturating_duration_since(now),
            _ => Duration::from_secs(1),
        };
        state = shared
            .work
            .wait_timeout(state, timeout)
            .expect("output state poisoned")
            .0;
    }
}

/// The stream ended before the threshold was reached (a short track): play what there is.
fn ended_early(state: &State) -> bool {
    state.marks.iter().any(|m| m.kind == MarkKind::End)
}

/// Moves up to one chunk of the current track (not past the next mark) into `frames`.
fn fill_frames(state: &mut State, frames: &mut Vec<f32>) {
    let channels = u64::from(state.format.map_or(2, |f| f.1).max(1));
    let limit = state.marks.front().map_or(state.write, |m| m.at) - state.read;
    let n = (CHUNK_FRAMES as u64 * channels)
        .min(limit)
        .min(state.samples.len() as u64);
    let n = n - n % channels;
    frames.clear();
    for sample in state.samples.drain(..usize::try_from(n).unwrap_or(0)) {
        frames.push(sample);
    }
    state.read += n;
    state.track_frames += n / channels;
}

fn fill_silence(frames: &mut Vec<f32>, channels: u16) {
    frames.clear();
    frames.resize(CHUNK_FRAMES * usize::from(channels.max(1)), 0.0);
}

/// `pw-cat` as the device: a streaming float WAV on its stdin (the board's PipeWire 0.3.48
/// `pw-cat` cannot read raw samples from stdin), played into a PipeWire node.
pub struct PwCat {
    program: String,
    env: Vec<(String, String)>,
    /// The PipeWire node to play into, resolved at every open (`--target` needs its id).
    target: Arc<dyn Fn() -> Option<u32> + Send + Sync>,
    latency: Duration,
    child: Option<(Child, ChildStdin)>,
    rate: u32,
    channels: u16,
    /// Bytes written since the last delay estimate, for [`Device::delay_frames`].
    pipe_frames: u32,
    bytes: Vec<u8>,
}

impl std::fmt::Debug for PwCat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PwCat")
            .field("program", &self.program)
            .field("open", &self.child.is_some())
            .field("rate", &self.rate)
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

/// A pipe's default size on Linux: the most `pw-cat` has not read yet.
const PIPE_BYTES: u32 = 65_536;

impl PwCat {
    #[must_use]
    pub fn new(
        program: impl Into<String>,
        env: Vec<(String, String)>,
        target: Arc<dyn Fn() -> Option<u32> + Send + Sync>,
        latency: Duration,
    ) -> Self {
        Self {
            program: program.into(),
            env,
            target,
            latency,
            child: None,
            rate: 44_100,
            channels: 2,
            pipe_frames: 0,
            bytes: Vec::new(),
        }
    }
}

impl Device for PwCat {
    fn open(&mut self, rate: u32, channels: u16) -> io::Result<()> {
        self.close();
        let mut command = Command::new(&self.program);
        command
            .args(["--playback", "--media-role", "Music"])
            .arg(format!("--latency={}ms", self.latency.as_millis()))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .envs(self.env.iter().map(|(k, v)| (k, v)));
        if let Some(id) = (self.target)() {
            command.arg(format!("--target={id}"));
        }
        let mut child = command.arg("-").spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("pw-cat without stdin"))?;
        stdin.write_all(&wav_header(rate, channels))?;
        self.child = Some((child, stdin));
        self.rate = rate;
        self.channels = channels;
        self.bytes.reserve(CHUNK_FRAMES * usize::from(channels) * 4);
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let (_, stdin) = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::other("device closed"))?;
        stdin.write_all(bytes)?;
        let frame = u32::from(self.channels.max(1)) * 4;
        self.pipe_frames = PIPE_BYTES / frame;
        Ok(())
    }

    fn write_frames(&mut self, samples: &mut [f32]) -> io::Result<()> {
        self.bytes.clear();
        self.bytes
            .extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
        let (_, stdin) = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::other("device closed"))?;
        stdin.write_all(&self.bytes)?;
        self.pipe_frames = PIPE_BYTES / (u32::from(self.channels.max(1)) * 4);
        Ok(())
    }

    fn delay_frames(&self) -> u32 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "frames of latency"
        )]
        let latency = (self.latency.as_secs_f64() * f64::from(self.rate)) as u32;
        // A blocking writer keeps the pipe about full.
        self.pipe_frames + latency
    }

    fn close(&mut self) {
        if let Some((mut child, stdin)) = self.child.take() {
            drop(stdin);
            let _ = child.wait();
        }
        self.pipe_frames = 0;
    }

    fn is_open(&self) -> bool {
        self.child.is_some()
    }
}

/// A float WAV header with unknown length (streaming).
fn wav_header(rate: u32, channels: u16) -> [u8; 44] {
    let block = channels * 4;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&3u16.to_le_bytes()); // IEEE float
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * u32::from(block)).to_le_bytes());
    h[32..34].copy_from_slice(&block.to_le_bytes());
    h[34..36].copy_from_slice(&32u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what it gets; each write takes 1 ms so the thread does not spin.
    #[derive(Debug, Default, Clone)]
    struct Fake(Arc<Mutex<Log>>);

    #[derive(Debug, Default)]
    struct Log {
        opens: Vec<(u32, u16)>,
        samples: Vec<f32>,
        silence_writes: usize,
        open: bool,
    }

    impl Device for Fake {
        fn open(&mut self, rate: u32, channels: u16) -> io::Result<()> {
            let mut log = self.0.lock().unwrap();
            log.opens.push((rate, channels));
            log.open = true;
            Ok(())
        }
        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            std::thread::sleep(Duration::from_millis(1));
            let mut log = self.0.lock().unwrap();
            if bytes.iter().all(|b| *b == 0) {
                log.silence_writes += 1;
            } else {
                log.samples
                    .extend(bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)));
            }
            Ok(())
        }
        fn delay_frames(&self) -> u32 {
            0
        }
        fn close(&mut self) {
            self.0.lock().unwrap().open = false;
        }
        fn is_open(&self) -> bool {
            self.0.lock().unwrap().open
        }
    }

    fn wait_for(output: &Output, event: &PlaybackEvent, seen: &mut Vec<PlaybackEvent>) {
        for _ in 0..2000 {
            seen.extend(output.status().events);
            if seen.contains(event) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("no {event:?}, saw {seen:?}");
    }

    fn track(sink: &mut OutputSink, id: u64, rate: u32, value: f32, frames: usize) {
        sink.begin_track(TrackId::from_raw(id), rate, 2);
        sink.write(&vec![value; frames * 2]).unwrap();
        sink.end_track();
    }

    struct FloatOnly {
        fake: Fake,
        resets: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Device for FloatOnly {
        fn open(&mut self, rate: u32, channels: u16) -> io::Result<()> {
            self.fake.open(rate, channels)
        }
        fn write(&mut self, _: &[u8]) -> io::Result<()> {
            panic!("output encoded before DSP")
        }
        fn write_frames(&mut self, frames: &mut [f32]) -> io::Result<()> {
            self.fake.write_frames(frames)
        }
        fn reset(&mut self) {
            self.resets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn delay_frames(&self) -> u32 {
            0
        }
        fn close(&mut self) {
            self.fake.close();
        }
        fn is_open(&self) -> bool {
            self.fake.is_open()
        }
    }
    #[test]
    fn output_passes_float_blocks_and_resets_the_device_on_stop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let fake = Fake::default();
        let resets = Arc::new(AtomicUsize::new(0));
        let (output, mut sink) = Output::spawn(
            FloatOnly {
                fake: fake.clone(),
                resets: resets.clone(),
            },
            Duration::from_secs(1),
        )
        .unwrap();
        output.prepare(Start {
            threshold_frames: 1,
            autostart: true,
        });
        track(&mut sink, 1, 44_100, 0.25, 1000);
        wait_for(&output, &PlaybackEvent::PlaybackDone, &mut Vec::new());
        assert_eq!(fake.0.lock().unwrap().samples, vec![0.25; 2000]);
        let before = resets.load(Ordering::SeqCst);
        output.stop();
        for _ in 0..1000 {
            if resets.load(Ordering::SeqCst) > before {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(resets.load(Ordering::SeqCst) > before);
        output.shutdown();
    }

    #[test]
    fn float_writes_preserve_samples_for_legacy_devices() {
        let mut fake = Fake::default();
        let mut samples = [0.125, -0.5, f32::MIN_POSITIVE, -0.0];
        fake.write_frames(&mut samples).unwrap();
        let recorded = &fake.0.lock().unwrap().samples;
        assert_eq!(
            recorded.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            samples.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn plays_two_tracks_gaplessly_and_reports_the_end() {
        let fake = Fake::default();
        let (output, mut sink) = Output::spawn(fake.clone(), Duration::from_secs(1)).unwrap();
        output.prepare(Start {
            threshold_frames: 1000,
            autostart: true,
        });
        track(&mut sink, 1, 44_100, 0.5, 2000);
        track(&mut sink, 2, 44_100, 0.25, 1000);
        let mut seen = Vec::new();
        wait_for(&output, &PlaybackEvent::PlaybackDone, &mut seen);
        let starts = seen.iter().filter(|e| **e == PlaybackEvent::TrackStarted).count();
        assert_eq!(starts, 2, "{seen:?}");
        assert!(!seen.contains(&PlaybackEvent::OutputUnderrun), "{seen:?}");
        let log = fake.0.lock().unwrap();
        assert_eq!(log.opens, vec![(44_100, 2)], "same format: one open");
        assert_eq!(log.samples.len(), 6000);
        assert!(
            log.samples[..4000]
                .iter()
                .all(|s| (*s - 0.5).abs() < f32::EPSILON)
        );
        assert!(
            log.samples[4000..]
                .iter()
                .all(|s| (*s - 0.25).abs() < f32::EPSILON)
        );
    }

    #[test]
    fn a_new_rate_reopens_the_device() {
        let fake = Fake::default();
        let (output, mut sink) = Output::spawn(fake.clone(), Duration::from_secs(1)).unwrap();
        output.prepare(Start {
            threshold_frames: 10,
            autostart: true,
        });
        track(&mut sink, 3, 44_100, 0.5, 500);
        track(&mut sink, 4, 48_000, 0.25, 500);
        let mut seen = Vec::new();
        wait_for(&output, &PlaybackEvent::PlaybackDone, &mut seen);
        assert_eq!(fake.0.lock().unwrap().opens, vec![(44_100, 2), (48_000, 2)]);
    }

    #[test]
    fn without_autostart_it_waits_for_unpause() {
        let fake = Fake::default();
        let (output, mut sink) = Output::spawn(fake.clone(), Duration::from_secs(1)).unwrap();
        output.prepare(Start {
            threshold_frames: 100,
            autostart: false,
        });
        sink.begin_track(TrackId::from_raw(9), 44_100, 2);
        sink.write(&[0.5; 400]).unwrap();
        let mut seen = Vec::new();
        wait_for(&output, &PlaybackEvent::Buffered, &mut seen);
        assert!(output.status().paused);
        assert_eq!(fake.0.lock().unwrap().samples.len(), 0);
        output.unpause(None);
        wait_for(&output, &PlaybackEvent::TrackStarted, &mut seen);
    }

    #[test]
    fn stop_closes_the_writer_and_resets() {
        let fake = Fake::default();
        let (output, mut sink) = Output::spawn(fake, Duration::from_millis(10)).unwrap();
        sink.begin_track(TrackId::from_raw(9), 44_100, 2);
        let writer = std::thread::spawn(move || {
            let result = sink.write(&vec![0.5; 44_100]);
            (sink, result)
        });
        std::thread::sleep(Duration::from_millis(20));
        output.stop();
        let (mut sink, result) = writer.join().unwrap();
        assert_eq!(
            result,
            Err(Closed),
            "the full buffer blocked the writer until stop"
        );
        sink.begin_track(TrackId::from_raw(9), 44_100, 2);
        assert_eq!(sink.write(&[0.5; 2]), Ok(()), "a new track writes again");
        assert_eq!(output.status().elapsed, Duration::ZERO);
    }

    #[test]
    fn flush_next_keeps_the_current_track() {
        let fake = Fake::default();
        let (output, mut sink) = Output::spawn(fake.clone(), Duration::from_secs(1)).unwrap();
        track(&mut sink, 5, 44_100, 0.5, 300);
        track(&mut sink, 6, 44_100, 0.25, 300);
        output.flush_next();
        output.prepare(Start {
            threshold_frames: 10,
            autostart: true,
        });
        let mut seen = Vec::new();
        wait_for(&output, &PlaybackEvent::PlaybackDone, &mut seen);
        let log = fake.0.lock().unwrap();
        assert_eq!(log.samples.len(), 600);
    }

    #[test]
    fn wav_header_describes_float_stereo() {
        let h = wav_header(44_100, 2);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(u16::from_le_bytes([h[20], h[21]]), 3);
        assert_eq!(u32::from_le_bytes([h[28], h[29], h[30], h[31]]), 352_800);
    }

    fn played(fake: &Fake, output: &Output) -> Vec<f32> {
        output.prepare(Start {
            threshold_frames: 1,
            autostart: true,
        });
        let mut seen = Vec::new();
        wait_for(output, &PlaybackEvent::PlaybackDone, &mut seen);
        // Left channel only.
        fake.0
            .lock()
            .unwrap()
            .samples
            .iter()
            .step_by(2)
            .copied()
            .collect()
    }

    fn transition(kind: TransitionKind, ms: u64) -> Transition {
        Transition {
            kind,
            duration: Duration::from_millis(ms),
        }
    }

    #[test]
    fn crossfade_mixes_the_next_track_into_the_buffered_end() {
        let fake = Fake::default();
        let (output, mut sink) = Output::spawn(fake.clone(), Duration::from_secs(1)).unwrap();
        sink.transition(TrackId::from_raw(2), transition(TransitionKind::Crossfade, 200));
        track(&mut sink, 1, 1000, 1.0, 1000);
        track(&mut sink, 2, 1000, 0.5, 1000);
        let left = played(&fake, &output);
        assert_eq!(left.len(), 1800, "200 frames overlap");
        assert!((left[799] - 1.0).abs() < 1e-6);
        assert!((left[900] - 0.75).abs() < 0.01, "halfway: {}", left[900]);
        assert!((left[1000] - 0.5).abs() < 1e-6, "B alone after the overlap");
    }

    #[test]
    fn crossfade_needs_the_same_format() {
        let fake = Fake::default();
        let (output, mut sink) = Output::spawn(fake.clone(), Duration::from_secs(1)).unwrap();
        sink.transition(TrackId::from_raw(2), transition(TransitionKind::Crossfade, 200));
        track(&mut sink, 1, 1000, 1.0, 1000);
        track(&mut sink, 2, 2000, 0.5, 1000);
        assert_eq!(played(&fake, &output).len(), 2000, "played one after the other");
    }

    #[test]
    fn fades_in_and_out() {
        let fake = Fake::default();
        let (output, mut sink) = Output::spawn(fake.clone(), Duration::from_secs(1)).unwrap();
        sink.transition(TrackId::from_raw(1), transition(TransitionKind::FadeInOut, 100));
        track(&mut sink, 1, 1000, 1.0, 1000);
        let left = played(&fake, &output);
        assert_eq!(left.len(), 1000);
        assert!(left[0].abs() < 1e-6, "starts silent");
        assert!((left[50] - 0.5).abs() < 0.02, "half way in: {}", left[50]);
        assert!((left[500] - 1.0).abs() < 1e-6);
        assert!((left[950] - 0.5).abs() < 0.02, "half way out: {}", left[950]);
        assert!(left[999] < 0.02, "ends near silence: {}", left[999]);
    }
}

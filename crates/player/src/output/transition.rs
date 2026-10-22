//! Lyrion's track transitions (H15-64): crossfade and fade in/out, done in the output buffer while
//! the decoder writes. A crossfade mixes the next track into the tail of the previous one that is
//! still buffered (so it is at most as long as the buffer holds); fades scale the first or last
//! frames of a track. Nothing here runs on the output thread.

use std::time::Duration;

use super::{Mark, MarkKind, State};

pub use slimproto::{Transition, TransitionKind};

/// The writer's transition state for the track it writes.
#[derive(Debug, Default)]
pub(super) struct Fades {
    channels: u64,
    /// Where the track's Begin mark sits (absolute sample index).
    start: u64,
    /// Crossfade: the new track's samples are mixed into `[at, end)` (absolute sample indices).
    overlap: Option<Overlap>,
    /// Fade in over the first frames written after the overlap.
    fade_in: Option<Ramp>,
    /// Fade out over this many frames at the end of the track.
    fade_out: u64,
    scratch: Vec<f32>,
}

#[derive(Debug, Clone, Copy)]
struct Overlap {
    start: u64,
    next: u64,
    end: u64,
}

#[derive(Debug, Clone, Copy)]
struct Ramp {
    done: u64,
    total: u64,
}

fn frames_of(duration: Duration, rate: u32) -> u64 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "frames of a transition"
    )]
    let frames = (duration.as_secs_f64() * f64::from(rate)) as u64;
    frames
}

#[allow(clippy::cast_precision_loss, reason = "a gain from frame counts")]
fn gain(done: u64, total: u64) -> f32 {
    if total == 0 {
        1.0
    } else {
        (done as f32 / total as f32).min(1.0)
    }
}

impl Fades {
    /// A track begins with this transition; returns where its Begin mark goes.
    pub(super) fn begin(
        &mut self,
        state: &mut State,
        transition: Transition,
        rate: u32,
        channels: u16,
    ) -> u64 {
        let frames = frames_of(transition.duration, rate);
        self.channels = u64::from(channels.max(1));
        self.overlap = None;
        self.fade_in = None;
        self.fade_out = 0;
        self.start = state.write;
        if frames == 0 {
            return state.write;
        }
        match transition.kind {
            TransitionKind::None => {}
            TransitionKind::Crossfade => {
                if let Some(start) = crossfade_start(state, rate, channels, frames * self.channels) {
                    self.overlap = Some(Overlap {
                        start,
                        next: start,
                        end: state.write,
                    });
                    self.start = start;
                    return start;
                }
            }
            TransitionKind::FadeIn => {
                self.fade_in = Some(Ramp {
                    done: 0,
                    total: frames,
                });
            }
            TransitionKind::FadeOut => self.fade_out = frames,
            TransitionKind::FadeInOut => {
                self.fade_in = Some(Ramp {
                    done: 0,
                    total: frames,
                });
                self.fade_out = frames;
            }
        }
        state.write
    }

    /// Mixes the start of `frames` into the overlap; returns how many samples it took.
    pub(super) fn mix(&mut self, state: &mut State, frames: &[f32]) -> usize {
        let Some(overlap) = &mut self.overlap else {
            return 0;
        };
        let take = usize::try_from(overlap.end - overlap.next).map_or(frames.len(), |n| n.min(frames.len()));
        let total = (overlap.end - overlap.start) / self.channels;
        for (i, &new) in frames[..take].iter().enumerate() {
            let at = overlap.next + i as u64;
            // Already played (the reader got there first): the old sample stays.
            let Some(index) = at.checked_sub(state.read).and_then(|i| usize::try_from(i).ok()) else {
                continue;
            };
            if let Some(old) = state.samples.get_mut(index) {
                let g = gain((at - overlap.start) / self.channels, total);
                *old = *old * (1.0 - g) + new * g;
            }
        }
        overlap.next += take as u64;
        if overlap.next >= overlap.end {
            self.overlap = None;
        }
        take
    }

    /// `frames` with the fade in applied (the scratch buffer is reused).
    pub(super) fn faded<'a>(&'a mut self, frames: &'a [f32]) -> &'a [f32] {
        let Some(ramp) = &mut self.fade_in else {
            return frames;
        };
        self.scratch.clear();
        self.scratch.extend_from_slice(frames);
        let channels = usize::try_from(self.channels).unwrap_or(1);
        for frame in self.scratch.chunks_mut(channels) {
            let g = gain(ramp.done, ramp.total);
            for s in frame.iter_mut() {
                *s *= g;
            }
            ramp.done += 1;
        }
        if ramp.done >= ramp.total {
            self.fade_in = None;
        }
        &self.scratch
    }

    /// The track ended: fades out its last buffered frames.
    pub(super) fn end(&mut self, state: &mut State) {
        if self.fade_out == 0 {
            return;
        }
        let samples = self.fade_out * self.channels;
        let start = state
            .write
            .saturating_sub(samples)
            .max(state.read)
            .max(self.start);
        let total = (state.write - start) / self.channels;
        for at in start..state.write {
            let left = (state.write - at).div_ceil(self.channels);
            if let Some(sample) = usize::try_from(at - state.read)
                .ok()
                .and_then(|i| state.samples.get_mut(i))
            {
                *sample *= gain(left, total);
            }
        }
        self.fade_out = 0;
    }
}

/// Where a crossfade of `samples` into the previous track can start: only when that track
/// ended cleanly right at the buffer's end, in the same format. Removes its End mark (the new
/// track takes over within it).
fn crossfade_start(state: &mut State, rate: u32, channels: u16, samples: u64) -> Option<u64> {
    let last = state.marks.back().copied()?;
    if last.kind != MarkKind::End || last.at != state.write {
        return None;
    }
    let previous = state.marks.iter().rev().find_map(|m| match m.kind {
        MarkKind::Begin { rate, channels, .. } => Some((m.at, (rate, channels))),
        MarkKind::End => None,
    });
    let (begin, format) = previous.map_or((state.read, state.format), |(at, f)| (at, Some(f)));
    if format != Some((rate, channels)) {
        return None;
    }
    let channels = u64::from(channels.max(1));
    let room = state.write - state.write.saturating_sub(samples).max(begin).max(state.read);
    let room = room - room % channels;
    if room == 0 {
        return None;
    }
    state.marks.pop_back();
    Some(state.write - room)
}

/// Inserts `mark` in order (a crossfade's Begin lands before the end of the buffer).
pub(super) fn insert_mark(state: &mut State, mark: Mark) {
    let index = state
        .marks
        .iter()
        .position(|m| m.at > mark.at)
        .unwrap_or(state.marks.len());
    state.marks.insert(index, mark);
}

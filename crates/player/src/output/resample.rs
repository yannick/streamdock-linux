//! Resampling in the player (H15-80): the decoded rate (44.1 kHz from most files) to the rate the
//! PipeWire graph runs at (48 kHz on the board), before the DSP, so neither the DSP nor `pw-cat`
//! has to convert. A [`ResampleDevice`] wraps the next device; when the rates match it passes
//! frames straight through.

use std::io;
use std::sync::Arc;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, Fft, FixedAsync, FixedSync, PolynomialDegree, Resampler, WindowFunction};

use super::{CHUNK_FRAMES, Device};

/// How the conversion trades CPU for quality (Settings → Lyrion player, "Resampling quality").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    /// Cubic polynomial interpolation without an anti-aliasing filter: cheapest, a little aliasing.
    Fast,
    /// FFT-based synchronous conversion (Blackman-Harris window): fast and high quality.
    #[default]
    Balanced,
    /// FFT conversion in one long block: the steepest filter, more delay.
    High,
}

impl Quality {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Balanced => "balanced",
            Self::High => "high",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "fast" => Some(Self::Fast),
            "balanced" => Some(Self::Balanced),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

/// The rate to convert to, asked at every open (`None`: keep the decoded rate).
pub type TargetRate = Arc<dyn Fn() -> Option<u32> + Send + Sync>;

/// One open conversion.
struct Active {
    resampler: Box<dyn Resampler<f32>>,
    /// Input not yet converted (interleaved, less than one resampler chunk).
    pending: Vec<f32>,
    /// Converted frames, reused for every chunk.
    out: Vec<f32>,
}

/// Converts to the target rate in front of `inner`.
pub struct ResampleDevice<D> {
    inner: D,
    target: TargetRate,
    quality: Quality,
    active: Option<Active>,
    in_rate: u32,
    out_rate: u32,
    channels: u16,
}

impl<D: std::fmt::Debug> std::fmt::Debug for ResampleDevice<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResampleDevice")
            .field("inner", &self.inner)
            .field("quality", &self.quality)
            .field("in_rate", &self.in_rate)
            .field("out_rate", &self.out_rate)
            .field("converting", &self.active.is_some())
            .finish_non_exhaustive()
    }
}

fn resampler(quality: Quality, from: u32, to: u32, channels: usize) -> io::Result<Box<dyn Resampler<f32>>> {
    let ratio = f64::from(to) / f64::from(from);
    let made: Result<Box<dyn Resampler<f32>>, _> = match quality {
        Quality::Fast => Async::<f32>::new_poly(
            ratio,
            1.0,
            PolynomialDegree::Cubic,
            CHUNK_FRAMES,
            channels,
            FixedAsync::Input,
        )
        .map(|r| Box::new(r) as Box<dyn Resampler<f32>>),
        Quality::Balanced => Fft::<f32>::new(
            from as usize,
            to as usize,
            CHUNK_FRAMES,
            channels,
            FixedSync::Input,
        )
        .map(|r| Box::new(r) as Box<dyn Resampler<f32>>),
        // One long FFT block (4 × 441 frames): a steeper anti-aliasing filter than Balanced's
        // ~256-frame sub-blocks, at about the same CPU; a 256-tap sinc costs a whole Cortex-A7
        // core (H15-80, measured on the board).
        Quality::High => Fft::<f32>::new_custom(
            from as usize,
            to as usize,
            CHUNK_FRAMES * 4,
            1,
            channels,
            WindowFunction::BlackmanHarris2,
            FixedSync::Input,
        )
        .map(|r| Box::new(r) as Box<dyn Resampler<f32>>),
    };
    made.map_err(|e| io::Error::other(format!("resampler {from} → {to} Hz: {e}")))
}

impl<D: Device> ResampleDevice<D> {
    #[must_use]
    pub fn new(inner: D, target: TargetRate, quality: Quality) -> Self {
        Self {
            inner,
            target,
            quality,
            active: None,
            in_rate: 0,
            out_rate: 0,
            channels: 0,
        }
    }

    /// The decoded and the output rate of the open device (equal when passing through).
    #[must_use]
    pub fn rates(&self) -> (u32, u32) {
        (self.in_rate, self.out_rate)
    }

    /// Converts every whole resampler chunk of `pending` and writes it on.
    fn drain(&mut self) -> io::Result<()> {
        let channels = usize::from(self.channels.max(1));
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        loop {
            let need = active.resampler.input_frames_next();
            if active.pending.len() < need * channels {
                return Ok(());
            }
            let out_frames = active.resampler.output_frames_next();
            active.out.resize(out_frames * channels, 0.0);
            let input = InterleavedSlice::new(&active.pending[..need * channels], channels, need)
                .map_err(|e| io::Error::other(format!("resampler input: {e}")))?;
            let mut output = InterleavedSlice::new_mut(&mut active.out, channels, out_frames)
                .map_err(|e| io::Error::other(format!("resampler output: {e}")))?;
            let (used, written) = active
                .resampler
                .process_into_buffer(&input, &mut output, None)
                .map_err(|e| io::Error::other(format!("resampling: {e}")))?;
            active.pending.drain(..used * channels);
            self.inner.write_frames(&mut active.out[..written * channels])?;
        }
    }
}

impl<D: Device> ResampleDevice<D> {
    /// Writes out what the resampler still holds (the pending input and its own delay), padding
    /// with silence to whole chunks, then forgets it.
    fn flush_tail(&mut self) -> io::Result<()> {
        let channels = usize::from(self.channels.max(1));
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        let ratio = f64::from(self.out_rate) / f64::from(self.in_rate.max(1));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "H15-80: frame counts"
        )]
        let mut left = ((active.pending.len() / channels) as f64 * ratio).round() as usize
            + active.resampler.output_delay();
        while left > 0 {
            let need = active.resampler.input_frames_next();
            active
                .pending
                .resize(need.max(active.pending.len() / channels) * channels, 0.0);
            let out_frames = active.resampler.output_frames_next();
            active.out.resize(out_frames * channels, 0.0);
            let input = InterleavedSlice::new(&active.pending[..need * channels], channels, need)
                .map_err(|e| io::Error::other(format!("resampler input: {e}")))?;
            let mut output = InterleavedSlice::new_mut(&mut active.out, channels, out_frames)
                .map_err(|e| io::Error::other(format!("resampler output: {e}")))?;
            let (used, written) = active
                .resampler
                .process_into_buffer(&input, &mut output, None)
                .map_err(|e| io::Error::other(format!("resampling: {e}")))?;
            active.pending.drain(..used * channels);
            let take = written.min(left);
            self.inner.write_frames(&mut active.out[..take * channels])?;
            left -= take;
            if written == 0 {
                break;
            }
        }
        Ok(())
    }
}

impl<D: Device> Device for ResampleDevice<D> {
    /// A new format. When the output side stays the same (e.g. 44.1 kHz after 48 kHz, both played
    /// at 48 kHz), the next device stays open and only the resampler changes: the old one's tail
    /// is flushed first, so the change is gapless.
    fn open(&mut self, rate: u32, channels: u16) -> io::Result<()> {
        let to = (self.target)().filter(|&r| r > 0).unwrap_or(rate);
        let keep = self.inner.is_open() && self.out_rate == to && self.channels == channels;
        if keep {
            self.flush_tail()?;
        } else {
            self.close();
        }
        self.in_rate = rate;
        self.out_rate = to;
        self.channels = channels;
        self.active = None;
        if to != rate {
            let n = usize::from(channels.max(1));
            self.active = Some(Active {
                resampler: resampler(self.quality, rate, to, n)?,
                pending: Vec::with_capacity(CHUNK_FRAMES * 4 * n),
                out: Vec::with_capacity(CHUNK_FRAMES * 4 * n),
            });
            tracing::info!(
                from = rate,
                to,
                quality = self.quality.as_str(),
                "player resamples"
            );
        }
        if keep {
            Ok(())
        } else {
            self.inner.open(to, channels)
        }
    }

    /// The byte route is only the fallback (the output thread uses `write_frames`): it allocates.
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut frames: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        self.write_frames(&mut frames)
    }

    fn write_frames(&mut self, samples: &mut [f32]) -> io::Result<()> {
        match self.active.as_mut() {
            None => self.inner.write_frames(samples),
            Some(active) => {
                active.pending.extend_from_slice(samples);
                self.drain()
            }
        }
    }

    fn reset(&mut self) {
        if let Some(active) = self.active.as_mut() {
            active.resampler.reset();
            active.pending.clear();
        }
        self.inner.reset();
    }

    /// In frames of the decoded rate: what the next device holds, the resampler's own delay
    /// and the input waiting for a whole chunk.
    fn delay_frames(&self) -> u32 {
        let inner = self.inner.delay_frames();
        let Some(active) = &self.active else { return inner };
        let channels = usize::from(self.channels.max(1));
        let scale = f64::from(self.in_rate) / f64::from(self.out_rate.max(1));
        #[allow(clippy::cast_precision_loss, reason = "H15-80: frame counts as f64")]
        let out_side = (f64::from(inner) + active.resampler.output_delay() as f64) * scale;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "H15-80: a non-negative frame count"
        )]
        let out_side = out_side.round() as u32;
        out_side + u32::try_from(active.pending.len() / channels).unwrap_or(u32::MAX)
    }

    /// Drops the resampler's tail (a few ms). At the end of playback the output's silence before
    /// the idle close has already pushed it through; on an output change mid-track it is lost.
    fn close(&mut self) {
        self.active = None;
        self.inner.close();
    }

    fn is_open(&self) -> bool {
        self.inner.is_open()
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "H15-80: test signals"
)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Collects what it gets.
    #[derive(Debug, Default, Clone)]
    struct Sink(Arc<Mutex<(u32, u16, Vec<f32>)>>);

    impl Device for Sink {
        fn open(&mut self, rate: u32, channels: u16) -> io::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.0 = rate;
            s.1 = channels;
            s.2.clear();
            Ok(())
        }
        fn write(&mut self, _: &[u8]) -> io::Result<()> {
            unreachable!("float route only")
        }
        fn write_frames(&mut self, samples: &mut [f32]) -> io::Result<()> {
            self.0.lock().unwrap().2.extend_from_slice(samples);
            Ok(())
        }
        fn delay_frames(&self) -> u32 {
            480
        }
        fn close(&mut self) {}
        fn is_open(&self) -> bool {
            true
        }
    }

    fn device(target: Option<u32>, quality: Quality) -> (ResampleDevice<Sink>, Sink) {
        let sink = Sink::default();
        (
            ResampleDevice::new(sink.clone(), Arc::new(move || target), quality),
            sink,
        )
    }

    /// A 1 kHz stereo sine at `rate`, `seconds` long.
    fn sine(rate: u32, seconds: f32) -> Vec<f32> {
        let n = (rate as f32 * seconds) as usize;
        (0..n)
            .flat_map(|i| {
                let v = (i as f32 / rate as f32 * 1000.0 * std::f32::consts::TAU).sin() * 0.5;
                [v, v]
            })
            .collect()
    }

    fn play(dev: &mut ResampleDevice<Sink>, samples: &[f32]) {
        for chunk in samples.chunks(CHUNK_FRAMES * 2) {
            dev.write_frames(&mut chunk.to_vec()).unwrap();
        }
    }

    #[test]
    fn same_rate_passes_through() {
        let (mut dev, sink) = device(Some(44_100), Quality::Balanced);
        dev.open(44_100, 2).unwrap();
        let input = sine(44_100, 0.1);
        play(&mut dev, &input);
        assert_eq!(sink.0.lock().unwrap().2, input);
        assert_eq!(dev.delay_frames(), 480, "only the device's own delay");
        // No target known: the decoded rate.
        let (mut dev, sink) = device(None, Quality::Balanced);
        dev.open(96_000, 2).unwrap();
        assert_eq!(sink.0.lock().unwrap().0, 96_000);
    }

    #[test]
    fn every_quality_converts_44k1_to_48k() {
        for quality in [Quality::Fast, Quality::Balanced, Quality::High] {
            let (mut dev, sink) = device(Some(48_000), quality);
            dev.open(44_100, 2).unwrap();
            assert_eq!(dev.rates(), (44_100, 48_000));
            play(&mut dev, &sine(44_100, 1.0));
            let s = sink.0.lock().unwrap();
            assert_eq!((s.0, s.1), (48_000, 2));
            let frames = s.2.len() / 2;
            // One second in, about one second out (minus what is still in the resampler).
            assert!(
                (46_000..=48_000).contains(&frames),
                "{quality:?}: {frames} frames"
            );
            // The level survives (peak of the 0.5 sine), after the start-up transient.
            let peak = s.2[s.2.len() / 2..].iter().fold(0f32, |m, v| m.max(v.abs()));
            assert!((0.45..0.55).contains(&peak), "{quality:?}: peak {peak}");
            // The delay is counted in input frames and includes the device's 480 output frames.
            let delay = dev.delay_frames();
            assert!(delay >= 441, "{quality:?}: delay {delay}");
        }
    }

    #[test]
    fn mono_and_rate_change_between_tracks() {
        let (mut dev, sink) = device(Some(48_000), Quality::Balanced);
        dev.open(44_100, 1).unwrap();
        let mono: Vec<f32> = sine(44_100, 0.5).into_iter().step_by(2).collect();
        for chunk in mono.chunks(CHUNK_FRAMES) {
            dev.write_frames(&mut chunk.to_vec()).unwrap();
        }
        assert_eq!(sink.0.lock().unwrap().1, 1);
        // Next track at 48 kHz: passes through.
        dev.open(48_000, 2).unwrap();
        assert_eq!(dev.rates(), (48_000, 48_000));
        let input = sine(48_000, 0.05);
        play(&mut dev, &input);
        assert_eq!(sink.0.lock().unwrap().2, input);
    }

    #[test]
    fn a_rate_change_keeps_the_device_open_and_loses_nothing() {
        let (mut dev, sink) = device(Some(48_000), Quality::Balanced);
        dev.open(48_000, 2).unwrap();
        play(&mut dev, &sine(48_000, 0.5));
        let opens_before = sink.0.lock().unwrap().2.len();
        // A 44.1 kHz track follows: same output side, so no reopen (the sink would clear).
        dev.open(44_100, 2).unwrap();
        play(&mut dev, &sine(44_100, 0.5));
        // And back to 48 kHz: the 44.1 kHz tail is flushed into the open device.
        dev.open(48_000, 2).unwrap();
        let s = sink.0.lock().unwrap();
        assert!(s.2.len() > opens_before, "nothing was cleared by a reopen");
        let frames = s.2.len() / 2;
        // 0.5 s + 0.5 s at 48 kHz, ± a block.
        assert!((47_000..=49_000).contains(&frames), "{frames} frames");
    }

    #[test]
    fn quality_names() {
        for q in [Quality::Fast, Quality::Balanced, Quality::High] {
            assert_eq!(Quality::parse(q.as_str()), Some(q));
        }
        assert_eq!(Quality::parse("best"), None);
    }
}

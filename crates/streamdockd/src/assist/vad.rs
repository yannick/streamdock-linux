//! Voice activity detection (H15-69): ends a recording when the speaker stops, without a key
//! release. A [`Detector`] scores 16 ms frames (256 samples at 16 kHz) for speech; the
//! [`Endpointer`] turns the scores into "speech ended" (speech, then `silence` of non-speech) or
//! "nothing heard" (no speech within `no_speech`). [`Vad::wrap`] applies both to a [`Capture`].
//!
//! The whole recording still goes to Home Assistant from its first sample: speech that starts
//! before the detector reacts is never lost, and the trailing silence is sent before the end, so
//! the last word is complete.

use std::io;
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::BoxFuture;

use super::capture::Capture;

/// Samples per frame: 16 ms at 16 kHz (`earshot`'s frame size).
pub const FRAME: usize = 256;
const FRAME_MS: u32 = 16;

/// How the end of speech is found (`[modules.homeassistant.assist]`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// Non-speech after speech that ends the recording.
    pub silence: Duration,
    /// Speech score threshold, 0-1: lower is more sensitive.
    pub threshold: f32,
    /// No speech within this long: nothing heard, the request is cancelled.
    pub no_speech: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            silence: Duration::from_millis(700),
            // 0.5 (earshot's suggestion) takes synthetic music for speech; 0.8 cuts pauses (H15-69).
            threshold: 0.6,
            no_speech: Duration::from_secs(5),
        }
    }
}

/// Scores frames for speech.
pub trait Detector: Send {
    /// Speech probability of one frame of [`FRAME`] samples, 0-1.
    fn score(&mut self, frame: &[i16]) -> f32;
}

/// `earshot`: a small pure-Rust neural VAD (ADR-001).
pub struct Earshot(Box<earshot::Detector>);

impl Default for Earshot {
    fn default() -> Self {
        Self(earshot::Detector::default_boxed())
    }
}

impl std::fmt::Debug for Earshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Earshot").finish_non_exhaustive()
    }
}

impl Detector for Earshot {
    fn score(&mut self, frame: &[i16]) -> f32 {
        self.0.predict_i16(frame)
    }
}

/// What the endpointer concluded after a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Waiting for speech.
    Waiting,
    /// Speech seen; still listening.
    Speaking,
    /// Speech followed by enough silence: the recording is complete.
    Ended,
    /// No speech in time.
    NothingHeard,
}

/// Frame scores to [`State`]s.
#[derive(Debug, Clone)]
pub struct Endpointer {
    settings: Settings,
    frames: u32,
    /// The last 32 frames, one bit each (1 = speech): speech counts once [`MIN_SPEECH`] of them
    /// are speech, so clicks or scattered notes do not.
    recent: u32,
    /// Consecutive non-speech frames since the last speech.
    silence_run: u32,
    state: State,
}

/// Speech frames within the last 32 (512 ms) that make speech: 160 ms.
const MIN_SPEECH: u32 = 10;

fn frames_in(d: Duration) -> u32 {
    u32::try_from(d.as_millis())
        .unwrap_or(u32::MAX)
        .div_ceil(FRAME_MS)
}

impl Endpointer {
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            frames: 0,
            recent: 0,
            silence_run: 0,
            state: State::Waiting,
        }
    }

    /// Feeds one frame's score.
    pub fn push(&mut self, score: f32) -> State {
        if matches!(self.state, State::Ended | State::NothingHeard) {
            return self.state;
        }
        self.frames += 1;
        let speech = score >= self.settings.threshold;
        self.recent = (self.recent << 1) | u32::from(speech);
        if speech {
            self.silence_run = 0;
            if self.recent.count_ones() >= MIN_SPEECH {
                self.state = State::Speaking;
            }
        } else {
            self.silence_run += 1;
        }
        match self.state {
            State::Speaking if self.silence_run >= frames_in(self.settings.silence) => {
                self.state = State::Ended;
            }
            State::Waiting if self.frames >= frames_in(self.settings.no_speech) => {
                self.state = State::NothingHeard;
            }
            _ => {}
        }
        self.state
    }

    /// Time into the recording, at the frame last pushed.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(u64::from(self.frames) * u64::from(FRAME_MS))
    }
}

/// The error a [`Vad`] capture ends with when nobody spoke.
#[derive(Debug, thiserror::Error)]
#[error("nothing heard")]
pub struct NothingHeard;

/// A capture that ends by itself at the end of speech.
pub struct Vad {
    inner: Box<dyn Capture>,
    detector: Box<dyn Detector>,
    endpointer: Endpointer,
    /// Samples not yet scored (less than a frame).
    pending: Vec<i16>,
    ended: bool,
}

impl std::fmt::Debug for Vad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vad")
            .field("endpointer", &self.endpointer)
            .finish_non_exhaustive()
    }
}

impl Vad {
    /// `inner` with end-of-speech detection by [`Earshot`].
    #[must_use]
    pub fn wrap(inner: Box<dyn Capture>, settings: Settings) -> Box<dyn Capture> {
        Self::with_detector(inner, Box::new(Earshot::default()), settings)
    }

    #[must_use]
    pub fn with_detector(
        inner: Box<dyn Capture>,
        detector: Box<dyn Detector>,
        settings: Settings,
    ) -> Box<dyn Capture> {
        Box::new(Self {
            inner,
            detector,
            endpointer: Endpointer::new(settings),
            pending: Vec::with_capacity(FRAME * 2),
            ended: false,
        })
    }

    /// Scores the whole frames in `block`; the state after the last one.
    fn feed(&mut self, block: &[u8]) -> State {
        let mut state = State::Waiting;
        self.pending
            .extend(block.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)));
        let whole = self.pending.len() / FRAME * FRAME;
        for frame in self.pending[..whole].as_chunks::<FRAME>().0 {
            state = self.endpointer.push(self.detector.score(frame));
            if matches!(state, State::Ended | State::NothingHeard) {
                break;
            }
        }
        self.pending.drain(..whole);
        state
    }
}

impl Capture for Vad {
    fn next(&mut self) -> BoxFuture<'_, io::Result<Option<Bytes>>> {
        Box::pin(async move {
            if self.ended {
                return Ok(None);
            }
            let Some(block) = self.inner.next().await? else {
                return Ok(None);
            };
            match self.feed(&block) {
                State::NothingHeard => Err(io::Error::other(NothingHeard)),
                State::Ended => {
                    // This block (trailing silence) still goes out; the next call ends.
                    tracing::debug!(at = ?self.endpointer.elapsed(), "assist: end of speech");
                    self.ended = true;
                    Ok(Some(block))
                }
                State::Waiting | State::Speaking => Ok(Some(block)),
            }
        })
    }

    /// The key's stop signal (release or a second tap) still ends the recording early.
    fn ends_on_release(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    reason = "H15-69: test signals and timings"
)]
pub mod tests {
    use super::*;

    /// A capture over prepared samples, in 20 ms blocks, as fast as it is read.
    pub struct Samples(pub Vec<i16>, pub usize);

    impl Capture for Samples {
        fn next(&mut self) -> BoxFuture<'_, io::Result<Option<Bytes>>> {
            Box::pin(async move {
                if self.1 >= self.0.len() {
                    return Ok(None);
                }
                let end = (self.1 + 320).min(self.0.len());
                let bytes: Vec<u8> = self.0[self.1..end].iter().flat_map(|s| s.to_le_bytes()).collect();
                self.1 = end;
                Ok(Some(Bytes::from(bytes)))
            })
        }

        fn ends_on_release(&self) -> bool {
            false
        }
    }

    #[test]
    fn endpointer_states() {
        let s = Settings {
            silence: Duration::from_millis(160),
            threshold: 0.5,
            no_speech: Duration::from_millis(800),
        };
        // 160 ms of speech makes speech; a pause shorter than the silence does not end it.
        let mut e = Endpointer::new(s);
        for _ in 0..9 {
            assert_eq!(e.push(0.9), State::Waiting, "under 160 ms of speech");
        }
        assert_eq!(e.push(0.9), State::Speaking);
        for _ in 0..9 {
            assert_eq!(e.push(0.1), State::Speaking, "a 144 ms pause does not end it");
        }
        assert_eq!(e.push(0.9), State::Speaking);
        let mut frames = 0;
        while e.push(0.1) != State::Ended {
            frames += 1;
        }
        assert_eq!(frames + 1, 10, "160 ms of silence = 10 frames");
        assert_eq!(e.push(0.9), State::Ended, "ended stays ended");

        // Scattered speech-like frames (clicks, notes): never speech, nothing heard after 800 ms.
        let mut e = Endpointer::new(s);
        let states: Vec<State> = (0..50)
            .map(|i| e.push(if i % 4 == 0 { 0.9 } else { 0.2 }))
            .collect();
        assert_eq!(states[49], State::NothingHeard);
        assert!(states.iter().all(|&s| s != State::Speaking));
    }

    /// Scores by sample amplitude: loud frames are speech (for wiring tests without a model).
    struct Loud;
    impl Detector for Loud {
        fn score(&mut self, frame: &[i16]) -> f32 {
            if frame.iter().any(|s| s.unsigned_abs() > 1000) {
                1.0
            } else {
                0.0
            }
        }
    }

    #[tokio::test]
    async fn vad_capture_ends_after_speech_and_passes_audio_through() {
        let mut samples = vec![5000i16; 16_000]; // 1 s "speech"
        samples.extend(vec![0i16; 32_000]); // 2 s silence
        let settings = Settings::default();
        let mut vad = Vad::with_detector(Box::new(Samples(samples, 0)), Box::new(Loud), settings);
        let mut sent = 0;
        while let Some(block) = vad.next().await.unwrap() {
            sent += block.len() / 2;
        }
        // Ended about 700 ms after the speech: 1.7 s ± one block.
        let at = Duration::from_secs_f64(sent as f64 / 16_000.0);
        assert!((at.as_secs_f64() - 1.7).abs() < 0.04, "ended at {at:?}");
    }

    #[tokio::test]
    async fn vad_capture_gives_up_without_speech() {
        let settings = Settings {
            no_speech: Duration::from_secs(1),
            ..Settings::default()
        };
        let mut vad = Vad::with_detector(Box::new(Samples(vec![0; 48_000], 0)), Box::new(Loud), settings);
        let err = loop {
            match vad.next().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("silence must not end as a request"),
                Err(e) => break e,
            }
        };
        assert_eq!(err.to_string(), "nothing heard");
    }

    // --- Fixture scenarios (H15-69): speech clips with silence, noise or music mixed in. ---

    fn clip(n: usize) -> Vec<i16> {
        let bytes: &[u8] = match n {
            1 => include_bytes!("fixtures/speech-1.wav"),
            2 => include_bytes!("fixtures/speech-2.wav"),
            _ => include_bytes!("fixtures/speech-3.wav"),
        };
        crate::assist::capture::wav_to_16k_mono(bytes).unwrap()
    }

    fn silence(ms: u32) -> Vec<i16> {
        vec![0; (ms * 16) as usize]
    }

    fn ms(samples: usize) -> f64 {
        samples as f64 / 16.0
    }

    /// Deterministic pink-ish noise (one-pole low-passed white noise) at `level` (peak, 0-1).
    fn noise(len: usize, level: f32, seed: u32) -> Vec<f32> {
        let mut x = seed.max(1);
        let mut low = 0.0f32;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let white = (x as f32 / u32::MAX as f32) * 2.0 - 1.0;
                low = 0.9 * low + 0.1 * white;
                low * 3.0 * level
            })
            .collect()
    }

    /// A music-like background: a chord that changes every 500 ms, with harmonics and a 2 Hz
    /// pulse, at `level` (peak, 0-1).
    fn music(len: usize, level: f32) -> Vec<f32> {
        let chords = [
            [220.0, 277.2, 329.6],
            [196.0, 246.9, 293.7],
            [174.6, 220.0, 261.6],
            [196.0, 246.9, 329.6],
        ];
        (0..len)
            .map(|i| {
                let t = i as f32 / 16_000.0;
                let chord = chords[(i / 8000) % chords.len()];
                let tone: f32 = chord
                    .iter()
                    .map(|f| {
                        (t * f * std::f32::consts::TAU).sin()
                            + 0.4 * (t * f * 2.0 * std::f32::consts::TAU).sin()
                    })
                    .sum::<f32>()
                    / 4.2;
                let pulse = 0.6 + 0.4 * (t * 2.0 * std::f32::consts::TAU).sin().abs();
                tone * pulse * level
            })
            .collect()
    }

    fn mix(speech: &[i16], background: &[f32]) -> Vec<i16> {
        speech
            .iter()
            .zip(background)
            .map(|(&s, &b)| (f32::from(s) + b * 32_767.0).clamp(-32_768.0, 32_767.0) as i16)
            .collect()
    }

    /// The first final state and its time (ms), or the state at the end.
    fn endpoint(samples: &[i16], detector: &mut dyn Detector, settings: Settings) -> (State, f64) {
        let mut e = Endpointer::new(settings);
        let mut state = State::Waiting;
        for frame in samples.as_chunks::<FRAME>().0 {
            state = e.push(detector.score(frame));
            if matches!(state, State::Ended | State::NothingHeard) {
                break;
            }
        }
        (state, e.elapsed().as_secs_f64() * 1000.0)
    }

    /// Energy + zero-crossing baseline (for the comparison only): speech is a frame well above
    /// the running noise floor with a voice-like zero-crossing rate.
    #[derive(Default)]
    struct Energy {
        floor: f32,
    }

    impl Detector for Energy {
        fn score(&mut self, frame: &[i16]) -> f32 {
            let energy = (frame.iter().map(|&s| f32::from(s).powi(2)).sum::<f32>() / FRAME as f32).sqrt();
            let crossings = frame.windows(2).filter(|w| (w[0] >= 0) != (w[1] >= 0)).count();
            if self.floor == 0.0 {
                self.floor = energy.max(30.0);
            }
            // The floor follows quiet frames quickly and loud ones slowly.
            let rate = if energy < self.floor { 0.2 } else { 0.005 };
            self.floor += (energy - self.floor) * rate;
            let voiced = (5..=120).contains(&crossings);
            if voiced && energy > self.floor.max(30.0) * 3.0 {
                1.0
            } else {
                0.0
            }
        }
    }

    struct Scenario {
        name: &'static str,
        samples: Vec<i16>,
        /// The end of the last speech, ms (None: no speech in it).
        speech_end: Option<f64>,
    }

    fn scenarios() -> Vec<Scenario> {
        let mut v = Vec::new();
        for n in 1..=3 {
            let c = clip(n);
            let mut s = silence(500);
            s.extend(&c);
            s.extend(silence(2000));
            v.push(Scenario {
                name: ["clean-1", "clean-2", "clean-3"][n - 1],
                speech_end: Some(500.0 + ms(c.len())),
                samples: s,
            });
        }
        // A word right at the key press.
        let c = clip(1);
        let mut s = c.clone();
        s.extend(silence(2000));
        v.push(Scenario {
            name: "no lead-in",
            speech_end: Some(ms(c.len())),
            samples: s,
        });
        // Speech resuming 100 ms before the silence deadline.
        let (a, b) = (clip(1), clip(3));
        let mut s = silence(300);
        s.extend(&a);
        s.extend(silence(600));
        s.extend(&b);
        s.extend(silence(2000));
        v.push(Scenario {
            name: "pause 600 ms",
            speech_end: Some(300.0 + ms(a.len()) + 600.0 + ms(b.len())),
            samples: s,
        });
        // Noise and music under the speech (and on their own).
        for (name, bg) in [("noise", 0), ("music", 1)] {
            let c = clip(2);
            let mut s = silence(500);
            s.extend(&c);
            s.extend(silence(2500));
            let back = if bg == 0 {
                noise(s.len(), 0.03, 7)
            } else {
                music(s.len(), 0.08)
            };
            v.push(Scenario {
                name: if bg == 0 { "speech+noise" } else { "speech+music" },
                speech_end: Some(500.0 + ms(c.len())),
                samples: mix(&s, &back),
            });
            let quiet = silence(7000);
            let back = if bg == 0 {
                noise(quiet.len(), 0.03, 9)
            } else {
                music(quiet.len(), 0.08)
            };
            v.push(Scenario {
                name: if bg == 0 { "noise only" } else { "music only" },
                speech_end: None,
                samples: mix(&quiet, &back),
            });
            let _ = name;
        }
        v.push(Scenario {
            name: "silence only",
            speech_end: None,
            samples: silence(7000),
        });
        v
    }

    #[test]
    #[ignore = "comparison report (H15-69): cargo test -p streamdockd vad_report -- --ignored --nocapture"]
    fn vad_report() {
        let threshold = std::env::var("VAD_THRESHOLD")
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(0.6);
        let settings = Settings {
            threshold,
            ..Settings::default()
        };
        let silence_ms = settings.silence.as_secs_f64() * 1000.0;
        for (name, make) in [
            (
                "earshot",
                (|| Box::new(Earshot::default()) as Box<dyn Detector>) as fn() -> Box<dyn Detector>,
            ),
            ("energy", || Box::new(Energy::default())),
        ] {
            println!("{name}:");
            for sc in scenarios() {
                let (state, at) = endpoint(&sc.samples, make().as_mut(), settings);
                let err = sc.speech_end.map(|end| at - (end + silence_ms));
                println!(
                    "  {:14} {:?} at {:6.0} ms  error {:?}",
                    sc.name,
                    state,
                    at,
                    err.map(f64::round)
                );
            }
        }
    }

    #[test]
    fn fixtures_end_within_150_ms() {
        let settings = Settings::default();
        let silence_ms = settings.silence.as_secs_f64() * 1000.0;
        for sc in scenarios() {
            let (state, at) = endpoint(&sc.samples, &mut Earshot::default(), settings);
            match sc.speech_end {
                Some(end) => {
                    assert_eq!(state, State::Ended, "{}", sc.name);
                    let error = at - (end + silence_ms);
                    assert!(
                        error.abs() <= 150.0,
                        "{}: ended at {at} ms, {error:+.0} ms off",
                        sc.name
                    );
                }
                // Silence, noise or music alone: never a request, given up after `no_speech`.
                None => assert_eq!(state, State::NothingHeard, "{}", sc.name),
            }
        }
    }

    #[test]
    #[ignore = "CPU per frame (H15-69); on the board: the armv7 test binary with --ignored vad_cpu --nocapture"]
    fn vad_cpu() {
        let samples = scenarios()
            .into_iter()
            .flat_map(|s| s.samples)
            .collect::<Vec<_>>();
        let mut d = Earshot::default();
        let frames = samples.len() / FRAME;
        let t = std::time::Instant::now();
        let mut sum = 0.0;
        for frame in samples.as_chunks::<FRAME>().0 {
            sum += d.score(frame);
        }
        let per = t.elapsed().as_secs_f64() / frames as f64;
        println!(
            "earshot: {frames} frames, {:.1} µs per 16 ms frame = {:.2} % of one core (checksum {sum:.1})",
            per * 1e6,
            per / 0.016 * 100.0
        );
    }
}

//! From audio to "heard": the frontend every 10 ms, the model every `stride` steps, and the
//! decision as microWakeWord makes it: the mean of the last `window` probabilities above the cutoff.

use std::collections::VecDeque;

use crate::frontend::{CHANNELS, Frontend, STEP};
use crate::{Error, Model};

/// What one model invocation found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    /// This invocation's probability, 0–1.
    pub probability: f32,
    /// The mean over the sliding window (0 until the window is full).
    pub mean: f32,
    /// The mean went above the cutoff: the wake word was heard. The window starts over, and the
    /// next second can't trigger again (one utterance, one detection).
    pub detected: bool,
}

/// Invocations after a detection that can't trigger (about 1 s at 30 ms each).
const REFRACTORY: usize = 33;

/// A wake word detector for one model.
#[derive(Debug, Clone)]
pub struct Detector {
    frontend: Frontend,
    model: Model,
    cutoff: f32,
    window: usize,
    probabilities: VecDeque<f32>,
    features: Vec<i8>,
    /// Samples short of a whole 10 ms step.
    carry: Vec<i16>,
    /// Invocations left before a detection counts again.
    quiet: usize,
}

impl Detector {
    /// `cutoff` 0–1 and `window` (invocations) as in the model's manifest.
    pub fn new(model: Model, cutoff: f32, window: usize) -> Result<Self, Error> {
        if !(0.0..=1.0).contains(&cutoff) || !(1..=100).contains(&window) {
            return Err(Error::Input("cutoff must be 0-1 and the window 1-100"));
        }
        Ok(Self {
            frontend: Frontend::new(),
            features: Vec::with_capacity(model.stride() * CHANNELS),
            model,
            cutoff,
            window,
            probabilities: VecDeque::with_capacity(window),
            carry: Vec::with_capacity(STEP),
            quiet: 0,
        })
    }

    #[must_use]
    pub fn cutoff(&self) -> f32 {
        self.cutoff
    }

    /// Feeds 16 kHz mono samples; returns a score per model invocation (one every 30 ms for the
    /// built-in models).
    pub fn push(&mut self, mut samples: &[i16]) -> Result<Vec<Score>, Error> {
        let mut scores = Vec::new();
        if !self.carry.is_empty() {
            let take = (STEP - self.carry.len()).min(samples.len());
            self.carry.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.carry.len() < STEP {
                return Ok(scores);
            }
            let step = std::mem::take(&mut self.carry);
            self.step(&step, &mut scores)?;
            self.carry = step;
            self.carry.clear();
        }
        let (chunks, rest) = samples.as_chunks::<STEP>();
        for chunk in chunks {
            self.step(chunk, &mut scores)?;
        }
        self.carry.extend_from_slice(rest);
        Ok(scores)
    }

    fn step(&mut self, chunk: &[i16], scores: &mut Vec<Score>) -> Result<(), Error> {
        let (Some(features), _) = self.frontend.process(chunk) else {
            return Ok(());
        };
        self.features
            .extend(features.iter().map(|&f| self.model.quantize(f)));
        if self.features.len() < self.model.stride() * CHANNELS {
            return Ok(());
        }
        let byte = self.model.invoke(&self.features)?;
        self.features.clear();
        // Dequantized as the models declare it: scale 1/256, zero point 0.
        let probability = f32::from(byte) / 256.0;
        if self.probabilities.len() == self.window {
            self.probabilities.pop_front();
        }
        self.probabilities.push_back(probability);
        let full = self.probabilities.len() == self.window;
        #[allow(clippy::cast_precision_loss, reason = "window ≤ 100")]
        let mean = if full {
            self.probabilities.iter().sum::<f32>() / self.window as f32
        } else {
            0.0
        };
        let detected = full && mean > self.cutoff && self.quiet == 0;
        self.quiet = self.quiet.saturating_sub(1);
        if detected {
            self.probabilities.clear();
            self.quiet = REFRACTORY;
        }
        scores.push(Score {
            probability,
            mean,
            detected,
        });
        Ok(())
    }

    /// Starts over: frontend, model state and window (after a request, before listening again).
    pub fn reset(&mut self) {
        self.frontend.reset();
        self.model.reset();
        self.probabilities.clear();
        self.features.clear();
        self.carry.clear();
        self.quiet = 0;
    }
}

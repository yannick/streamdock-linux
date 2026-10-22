//! RBJ Audio EQ Cookbook coefficients, normalized in f64 and processed in f32.
//! <https://www.w3.org/TR/audio-eq-cookbook/> . Shelves use slope S=1, matching
//! PipeWire 0.3.48; its shelf Q control does not change the shelf's coefficients.

use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Peak,
    LowShelf,
    HighShelf,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefficients {
    pub(crate) b: [f32; 3],
    pub(crate) a: [f32; 2],
}

impl Coefficients {
    pub(crate) const IDENTITY: Self = Self {
        b: [1.0, 0.0, 0.0],
        a: [0.0; 2],
    };
    pub fn new(kind: Kind, rate: u32, freq: f64, q: f64, gain: f64) -> io::Result<Self> {
        if rate < 2000
            || !freq.is_finite()
            || freq <= 0.0
            || !q.is_finite()
            || q <= 0.0
            || !gain.is_finite()
            || gain.abs() > 24.0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid biquad parameters",
            ));
        }
        if gain == 0.0 {
            return Ok(Self::constant(1.0));
        }
        let amplitude = 10.0_f64.powf(gain / 40.0);
        if freq >= f64::from(rate) / 2.0 {
            return Ok(Self::constant(if kind == Kind::LowShelf {
                amplitude * amplitude
            } else {
                1.0
            }));
        }
        let omega = std::f64::consts::TAU * freq / f64::from(rate);
        let (sin, cos) = omega.sin_cos();
        let alpha = sin / (2.0 * q);
        let (b, a) = match kind {
            Kind::Peak => (
                [1.0 + alpha * amplitude, -2.0 * cos, 1.0 - alpha * amplitude],
                [1.0 + alpha / amplitude, -2.0 * cos, 1.0 - alpha / amplitude],
            ),
            Kind::LowShelf | Kind::HighShelf => shelf(kind, amplitude, sin, cos),
        };
        Ok(Self::normalized(b, a))
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "H15-67: normalized audio coefficients stored as f32"
    )]
    fn normalized(b: [f64; 3], a: [f64; 3]) -> Self {
        Self {
            b: b.map(|v| (v / a[0]) as f32),
            a: [(a[1] / a[0]) as f32, (a[2] / a[0]) as f32],
        }
    }

    fn constant(gain: f64) -> Self {
        Self::normalized([gain, 0.0, 0.0], [1.0, 0.0, 0.0])
    }
}

fn shelf(kind: Kind, amplitude: f64, sin: f64, cos: f64) -> ([f64; 3], [f64; 3]) {
    let sum = amplitude + 1.0;
    let difference = amplitude - 1.0;
    let beta = (2.0 * amplitude).sqrt() * sin;
    match kind {
        Kind::LowShelf => (
            [
                amplitude * (sum - difference * cos + beta),
                2.0 * amplitude * (difference - sum * cos),
                amplitude * (sum - difference * cos - beta),
            ],
            [
                sum + difference * cos + beta,
                -2.0 * (difference + sum * cos),
                sum + difference * cos - beta,
            ],
        ),
        Kind::HighShelf => (
            [
                amplitude * (sum + difference * cos + beta),
                -2.0 * amplitude * (difference + sum * cos),
                amplitude * (sum + difference * cos - beta),
            ],
            [
                sum - difference * cos + beta,
                2.0 * (difference - sum * cos),
                sum - difference * cos - beta,
            ],
        ),
        Kind::Peak => unreachable!("shelf kind"),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Biquad {
    coefficients: Coefficients,
    history: [f32; 2],
    bypass: bool,
}

impl Biquad {
    #[cfg(test)]
    pub(crate) fn new(kind: Kind, rate: u32, freq: f64, q: f64, gain: f64) -> io::Result<Self> {
        Ok(Self::from_coefficients(Coefficients::new(
            kind, rate, freq, q, gain,
        )?))
    }

    pub(crate) fn from_coefficients(coefficients: Coefficients) -> Self {
        Self {
            coefficients,
            history: [0.0; 2],
            bypass: coefficients == Coefficients::IDENTITY,
        }
    }

    #[cfg(test)]
    pub(crate) fn process(&mut self, input: f32) -> f32 {
        if self.bypass {
            return input;
        }
        let c = self.coefficients;
        let output = c.b[0] * input + self.history[0];
        self.history[0] = c.b[1] * input - c.a[0] * output + self.history[1];
        self.history[1] = c.b[2] * input - c.a[1] * output;
        if !output.is_finite() {
            self.reset();
            return 0.0;
        }
        self.history = self.history.map(|v| if v.abs() < 1e-20 { 0.0 } else { v });
        output
    }
    /// Stride over one channel; keep the two delay values in registers for the block.
    pub(crate) fn process_block(&mut self, samples: &mut [f32], channel: usize, channels: usize) {
        if self.bypass {
            return;
        }
        let c = self.coefficients;
        let [mut z0, mut z1] = self.history;
        for sample in samples.iter_mut().skip(channel).step_by(channels) {
            let input = *sample;
            let output = c.b[0] * input + z0;
            z0 = c.b[1] * input - c.a[0] * output + z1;
            z1 = c.b[2] * input - c.a[1] * output;
            *sample = output;
        }
        self.history = [z0, z1].map(|v| if v.abs() < 1e-20 { 0.0 } else { v });
        if !self.history.iter().all(|v| v.is_finite()) {
            self.reset();
            for sample in samples.iter_mut().skip(channel).step_by(channels) {
                if !sample.is_finite() {
                    *sample = 0.0;
                }
            }
        }
    }

    pub(crate) fn set_coefficients(&mut self, coefficients: Coefficients) {
        self.coefficients = coefficients;
        self.bypass = coefficients == Coefficients::IDENTITY;
        if self.bypass {
            self.reset();
        }
    }
    pub(crate) fn reset(&mut self) {
        self.history = [0.0; 2];
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "H15-67: bounded synthetic f32 test signals"
)]
mod tests {
    use super::*;

    #[test]
    fn block_processing_matches_sample_reference_and_keeps_other_channel() {
        let mut reference = Biquad::new(Kind::Peak, 44_100, 1000.0, 0.7, 6.0).unwrap();
        let mut block_filter = reference.clone();
        let mut samples: Vec<_> = (0..882).map(|i| (i as f32 * 0.17).sin() * 0.3).collect();
        let mut expected = samples.clone();
        for frame in expected.as_chunks_mut::<2>().0 {
            frame[0] = reference.process(frame[0]);
        }
        block_filter.process_block(&mut samples, 0, 2);
        for (actual, expected) in samples.iter().zip(&expected) {
            assert!((actual - expected).abs() < 1e-6);
        }
    }

    fn response(kind: Kind, rate: u32, center: f64, probe: f64) -> f64 {
        let mut filter = Biquad::new(kind, rate, center, 0.7, 6.0).unwrap();
        let mut input_energy = 0.0;
        let mut output_energy = 0.0;
        for i in 0..rate {
            let x = (std::f64::consts::TAU * probe * f64::from(i) / f64::from(rate)).sin() as f32;
            let y = filter.process(x);
            if i > rate / 2 {
                input_energy += f64::from(x).powi(2);
                output_energy += f64::from(y).powi(2);
            }
        }
        10.0 * (output_energy / input_energy).log10()
    }

    #[test]
    fn peaking_gain_tracks_the_requested_frequency_at_assist_and_music_rates() {
        for rate in [16_000, 22_050, 24_000, 44_100, 48_000] {
            let db = response(Kind::Peak, rate, 1000.0, 1000.0);
            assert!((db - 6.0).abs() < 0.02, "{rate}: {db}");
        }
    }

    #[test]
    fn shelves_have_the_requested_low_and_high_frequency_gain() {
        assert!((response(Kind::LowShelf, 48_000, 120.0, 10.0) - 6.0).abs() < 0.03);
        assert!((response(Kind::HighShelf, 48_000, 8000.0, 23_000.0) - 6.0).abs() < 0.03);
    }

    #[test]
    fn coefficients_match_pipewire_0_3_48_reference() {
        // Generated by its biquad_set C function; values are numeric reference data.
        let cases = [
            (
                Kind::Peak,
                48_000,
                1000.0,
                0.7,
                6.0,
                [
                    1.061_623_81,
                    -1.860_114_93,
                    0.814_541_936,
                    -1.860_114_93,
                    0.876_165_748,
                ],
            ),
            (
                Kind::LowShelf,
                48_000,
                120.0,
                0.7,
                6.0,
                [
                    1.003_862_62,
                    -1.981_223_23,
                    0.977_705_836,
                    -1.981_309_29,
                    0.981_482_387,
                ],
            ),
            (
                Kind::HighShelf,
                48_000,
                8000.0,
                0.7,
                -4.0,
                [
                    0.739_665_747,
                    -0.367_563_397,
                    0.159_448_624,
                    -0.737_999_022,
                    0.269_549_966,
                ],
            ),
            (
                Kind::Peak,
                44_100,
                2289.0,
                1.57,
                6.1,
                [
                    1.068_236_11,
                    -1.767_633_08,
                    0.797_752_857,
                    -1.767_633_08,
                    0.865_988_97,
                ],
            ),
        ];
        for (kind, rate, freq, q, gain, expected) in cases {
            let c = Coefficients::new(kind, rate, freq, q, gain).unwrap();
            for (got, want) in [c.b[0], c.b[1], c.b[2], c.a[0], c.a[1]].into_iter().zip(expected) {
                assert!((f64::from(got) - want).abs() < 1e-7, "{kind:?}: {got} != {want}");
            }
        }
    }

    #[test]
    fn zero_gain_is_an_exact_bypass() {
        for kind in [Kind::Peak, Kind::LowShelf, Kind::HighShelf] {
            let mut filter = Biquad::new(kind, 44_100, 1000.0, 0.7, 0.0).unwrap();
            for x in [-1.0, 0.3, 0.0, 0.99] {
                assert_eq!(filter.process(x), x);
            }
        }
    }

    #[test]
    fn rejects_invalid_parameters_and_handles_bands_above_nyquist() {
        for (rate, freq, q, gain) in [
            (0, 1000.0, 1.0, 0.0),
            (44_100, f64::NAN, 1.0, 0.0),
            (44_100, 1000.0, 0.0, 0.0),
            (44_100, 1000.0, 1.0, f64::INFINITY),
        ] {
            assert!(Biquad::new(Kind::Peak, rate, freq, q, gain).is_err());
        }
        let mut filter = Biquad::new(Kind::HighShelf, 16_000, 10_000.0, 0.7, 12.0).unwrap();
        for i in 0..16_000 {
            assert!(filter.process(if i == 0 { 1.0 } else { 0.0 }).is_finite());
        }
    }
}

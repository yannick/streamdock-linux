//! Bauer stereo crossfeed; equations by Boris Mikhaylov (MIT).
//! <https://bs2b.sourceforge.net/> ; attribution/license in LICENSE-BS2B.txt.
use std::io;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefficients {
    low: [f32; 2],
    high: [f32; 3],
    gain: f32,
}

impl Coefficients {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "H15-67: bounded crossfeed coefficients compiled in f64, processed in f32"
    )]
    pub fn new(rate: u32, cutoff: f64, feed_db: f64) -> io::Result<Self> {
        if !(2000..=384_000).contains(&rate)
            || !cutoff.is_finite()
            || !(300.0..=2000.0).contains(&cutoff)
            || !feed_db.is_finite()
            || !(1.0..=15.0).contains(&feed_db)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid crossfeed parameters",
            ));
        }
        let low_db = -5.0 * feed_db / 6.0 - 3.0;
        let high_db = feed_db / 6.0 - 3.0;
        let low_gain = 10.0_f64.powf(low_db / 20.0);
        let high_gain = 1.0 - 10.0_f64.powf(high_db / 20.0);
        let high_cutoff = cutoff * 2.0_f64.powf((low_db - 20.0 * high_gain.log10()) / 12.0);
        let decay = |freq: f64| (-std::f64::consts::TAU * freq / f64::from(rate)).exp();
        let low_decay = decay(cutoff);
        let high_decay = decay(high_cutoff);
        Ok(Self {
            low: [(low_gain * (1.0 - low_decay)) as f32, low_decay as f32],
            high: [
                (1.0 - high_gain * (1.0 - high_decay)) as f32,
                -high_decay as f32,
                high_decay as f32,
            ],
            gain: (1.0 / (1.0 - high_gain + low_gain)) as f32,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Crossfeed {
    coefficients: Coefficients,
    low: [f32; 2],
    high: [f32; 2],
    previous: [f32; 2],
}

impl Crossfeed {
    #[cfg(test)]
    fn new(rate: u32, cutoff: f64, feed_db: f64) -> io::Result<Self> {
        Ok(Self::from_coefficients(Coefficients::new(rate, cutoff, feed_db)?))
    }

    pub(crate) fn from_coefficients(coefficients: Coefficients) -> Self {
        Self {
            coefficients,
            low: [0.0; 2],
            high: [0.0; 2],
            previous: [0.0; 2],
        }
    }

    pub(crate) fn process(&mut self, input: [f32; 2]) -> [f32; 2] {
        let c = self.coefficients;
        for (i, value) in input.iter().enumerate() {
            self.low[i] = c.low[0] * value + c.low[1] * self.low[i];
            self.high[i] = c.high[0] * value + c.high[1] * self.previous[i] + c.high[2] * self.high[i];
            self.previous[i] = *value;
        }
        [
            (self.high[0] + self.low[1]) * c.gain,
            (self.high[1] + self.low[0]) * c.gain,
        ]
    }

    pub(crate) fn set_coefficients(&mut self, coefficients: Coefficients) {
        self.coefficients = coefficients;
    }

    pub(crate) fn reset(&mut self) {
        self.low = [0.0; 2];
        self.high = [0.0; 2];
        self.previous = [0.0; 2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_bs2b_3_1_0_default_left_impulse() {
        // libbs2b.so.0, 44_100 Hz, 700 Hz / 4.5 dB, bs2b_cross_feed_f.
        let expected = [
            0.787_519_276_142_120_4,
            0.035_434_115_678_071_976,
            -0.021_250_758_320_093_155,
            0.032_070_670_276_880_264,
            -0.018_442_686_647_176_743,
            0.029_026_489_704_847_336,
            -0.016_005_672_514_438_63,
            0.026_271_265_000_104_904,
            -0.013_890_684_582_293_034,
            0.023_777_568_712_830_544,
            -0.012_055_171_653_628_35,
            0.021_520_577_371_120_453,
            -0.010_462_203_063_070_774,
            0.019_477_821_886_539_46,
            -0.009_079_729_206_860_065,
            0.017_628_965_899_348_26,
        ];
        let mut crossfeed = Crossfeed::new(44_100, 700.0, 4.5).unwrap();
        for (i, pair) in expected.as_chunks::<2>().0.iter().enumerate() {
            let output = crossfeed.process([if i == 0 { 1.0 } else { 0.0 }, 0.0]);
            for (got, want) in output.into_iter().zip(pair) {
                assert!((f64::from(got) - want).abs() < 1e-6, "{i}: {got} != {want}");
            }
        }
    }

    #[test]
    fn mono_stereo_signal_has_unity_dc_gain_at_assist_rates() {
        for rate in [16_000, 22_050, 24_000, 44_100, 48_000] {
            let mut c = Crossfeed::new(rate, 700.0, 4.5).unwrap();
            let mut y = [0.0; 2];
            for _ in 0..rate {
                y = c.process([0.25; 2]);
            }
            assert!((y[0] - 0.25).abs() < 1e-5 && (y[1] - 0.25).abs() < 1e-5);
        }
    }

    #[test]
    fn reset_clears_history() {
        let mut c = Crossfeed::new(44_100, 700.0, 4.5).unwrap();
        c.process([1.0, 0.0]);
        c.reset();
        assert_eq!(c.process([0.0; 2]), [0.0; 2]);
    }
}

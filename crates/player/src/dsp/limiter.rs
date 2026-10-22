//! Independent linked-channel lookahead limiter (H15-67), not a SWH implementation port.
use std::{collections::VecDeque, io};

// H15-67 review: sample-peak limiting, like the existing SWH stage.
// Four-phase true-peak detection cost ~6% of a core on the RV1106 and was rejected.
// The default -1 dB ceiling provides headroom, but is not a true-peak guarantee.
const RELEASE_SECONDS: f32 = 0.2;

#[derive(Debug, Clone)]
pub(crate) struct Limiter {
    delay: Vec<f32>,
    channels: usize,
    at: usize,
    frame: u64,
    peaks: VecDeque<(u64, f32)>,
    gain: f32,
    attack: Vec<f32>,
    attack_sum: f64,
    wraps_until_sum: u16,
    release: f32,
    threshold: f32,
}
impl Limiter {
    pub(crate) fn new(rate: u32, channels: usize, ceiling_db: f32) -> io::Result<Self> {
        if !(2000..=384_000).contains(&rate)
            || !(1..=32).contains(&channels)
            || !ceiling_db.is_finite()
            || !(-20.0..=0.0).contains(&ceiling_db)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid limiter format or ceiling",
            ));
        }
        let frames = usize::try_from(rate / 200).expect("bounded rate").max(1);
        #[allow(
            clippy::cast_precision_loss,
            reason = "H15-67: audio sample rate has sufficient precision in f32"
        )]
        let release = (-1.0 / (RELEASE_SECONDS * rate as f32)).exp();
        Ok(Self {
            delay: vec![0.0; frames * channels],
            channels,
            at: 0,
            frame: 0,
            peaks: VecDeque::with_capacity(frames + 2),
            gain: 1.0,
            attack: vec![1.0; frames],
            attack_sum: f64::from(rate / 200),
            wraps_until_sum: 1000,
            release,
            threshold: 10.0_f32.powf(ceiling_db / 20.0),
        })
    }
    pub(crate) fn delay_frames(&self) -> usize {
        self.delay.len() / self.channels
    }
    pub(crate) fn process(&mut self, frame: &mut [f32]) {
        debug_assert_eq!(frame.len(), self.channels);
        let peak = frame.iter().fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        while self.peaks.back().is_some_and(|(_, value)| *value <= peak) {
            self.peaks.pop_back();
        }
        debug_assert!(self.peaks.len() < self.peaks.capacity());
        self.peaks.push_back((self.frame, peak));
        // Keep the full lookahead window until its oldest delayed frame plays.
        let first = self
            .frame
            .saturating_sub(u64::try_from(self.delay_frames()).expect("bounded delay"));
        while self.peaks.front().is_some_and(|(index, _)| *index < first) {
            self.peaks.pop_front();
        }
        let max = self.peaks.front().map_or(0.0, |(_, value)| *value);
        let target = (self.threshold / max.max(1e-20)).min(1.0);
        // The peak stays in the max window for the full delay. Averaging those
        // targets ramps attack over that delay, reaching the required gain by the
        // time its sample plays, even when another larger peak replaces it.
        self.attack_sum += f64::from(target) - f64::from(self.attack[self.at]);
        self.attack[self.at] = target;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            reason = "H15-67: bounded mean gain and lookahead length"
        )]
        let attack = (self.attack_sum / self.attack.len() as f64) as f32;
        self.gain = attack.min(1.0 + (self.gain - 1.0) * self.release);
        let offset = self.at * self.channels;
        for (sample, delayed) in frame
            .iter_mut()
            .zip(&mut self.delay[offset..offset + self.channels])
        {
            let input = *sample;
            *sample = *delayed * self.gain;
            *delayed = input;
        }
        self.at += 1;
        if self.at == self.delay_frames() {
            self.at = 0;
            self.wraps_until_sum -= 1;
            if self.wraps_until_sum == 0 {
                self.attack_sum = self.attack.iter().map(|gain| f64::from(*gain)).sum();
                self.wraps_until_sum = 1000;
            }
        }
        self.frame += 1;
    }
    pub(crate) fn set_ceiling(&mut self, ceiling_db: f32) {
        self.threshold = 10.0_f32.powf(ceiling_db / 20.0);
    }
    pub(crate) fn reset(&mut self) {
        self.delay.fill(0.0);
        self.at = 0;
        self.frame = 0;
        self.peaks.clear();
        self.gain = 1.0;
        self.attack.fill(1.0);
        self.wraps_until_sum = 1000;
        #[allow(clippy::cast_precision_loss, reason = "H15-67: bounded lookahead length")]
        {
            self.attack_sum = self.attack.len() as f64;
        }
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
    fn delays_frames_and_keeps_normal_level_unchanged() {
        let mut l = Limiter::new(44_100, 2, -1.0).unwrap();
        let delay = l.delay_frames();
        for _ in 0..delay {
            let mut f = [0.1, -0.05];
            l.process(&mut f);
            assert_eq!(f, [0.0; 2]);
        }
        let mut f = [0.1, -0.05];
        l.process(&mut f);
        assert!((f[0] - 0.1).abs() < 1e-6 && (f[1] + 0.05).abs() < 1e-6);
    }

    #[test]
    fn limits_overload_and_preserves_channel_ratio() {
        let mut l = Limiter::new(48_000, 2, -1.0).unwrap();
        let limit = 10.0_f32.powf(-1.0 / 20.0);
        for i in 0..48000 {
            let x = if i % 503 == 0 {
                8.0
            } else {
                (i as f32 * 0.1).sin() * 2.0
            };
            let mut f = [x, -x * 0.5];
            l.process(&mut f);
            assert!(f[0].abs() <= limit + 1e-6 && f[1].abs() <= limit + 1e-6);
            assert!((f[1] + f[0] * 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn sample_ceiling_holds_at_music_and_assist_rates() {
        for rate in [16_000, 22_050, 24_000, 44_100, 48_000] {
            let mut limiter = Limiter::new(rate, 1, -1.0).unwrap();
            let ceiling = 10.0_f32.powf(-1.0 / 20.0);
            for i in 0..rate {
                let mut frame = [if i % 997 == 0 {
                    8.0
                } else {
                    (i as f32 * 1.3).sin() * 2.0
                }];
                limiter.process(&mut frame);
                assert!(frame[0].abs() <= ceiling + 1e-6, "rate {rate}");
            }
        }
    }

    #[test]
    fn releases_smoothly_after_a_transient_without_pumping_normal_audio() {
        let rate = 48_000;
        let mut l = Limiter::new(rate, 1, -1.0).unwrap();
        let mut f = [8.0];
        l.process(&mut f);
        let mut previous = 0.0;
        let mut at_200ms = 0.0;
        for i in 1..rate {
            let mut f = [0.1];
            l.process(&mut f);
            if usize::try_from(i).unwrap() > l.delay_frames() + 30 {
                assert!(f[0] + 1e-6 >= previous, "release went backwards");
                previous = f[0];
            }
            if i == rate / 5 {
                at_200ms = f[0];
            }
        }
        assert!(at_200ms > 0.05 && at_200ms < 0.09, "{at_200ms}");
        assert!(previous > 0.099);
    }

    #[test]
    fn attack_ramps_before_full_scale_step_without_a_gain_discontinuity() {
        let mut limiter = Limiter::new(44_100, 1, -1.0).unwrap();
        let ceiling = 10.0_f32.powf(-1.0 / 20.0);
        for _ in 0..1000 {
            limiter.process(&mut [0.0]);
        }
        let mut previous = 1.0_f32;
        for _ in 0..1000 {
            let mut frame = [1.0];
            limiter.process(&mut frame);
            assert!(frame[0] <= ceiling + 1e-6);
            let change_db = (20.0 * (limiter.gain / previous).log10()).abs();
            assert!(change_db <= 0.5, "gain step {change_db} dB");
            previous = limiter.gain;
        }
    }

    #[test]
    fn attack_sum_is_rebuilt_periodically_and_exact_after_reset() {
        let mut limiter = Limiter::new(2000, 1, -1.0).unwrap();
        limiter.attack_sum += 1e-8;
        for _ in 0..limiter.delay_frames() * 1000 {
            limiter.process(&mut [0.0]);
        }
        let exact: f64 = limiter.attack.iter().map(|gain| f64::from(*gain)).sum();
        assert_eq!(limiter.attack_sum, exact);
        limiter.process(&mut [8.0]);
        limiter.reset();
        assert_eq!(limiter.attack_sum, 10.0);
        assert_eq!(limiter.wraps_until_sum, 1000);
    }

    #[test]
    fn reset_drops_delayed_audio_and_restores_gain() {
        let mut l = Limiter::new(16_000, 1, -1.0).unwrap();
        l.process(&mut [8.0]);
        l.reset();
        for _ in 0..200 {
            let mut f = [0.0];
            l.process(&mut f);
            assert_eq!(f, [0.0]);
        }
    }
}

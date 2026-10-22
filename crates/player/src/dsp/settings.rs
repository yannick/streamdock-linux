use super::{biquad, crossfeed};
use std::io;

/// Three tone filters plus ten correction filters, independently applied to each channel.
pub const FILTERS: usize = 13;

/// One RBJ filter; shelves use slope 1 as PipeWire does, while peaks use `q`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Filter {
    pub kind: biquad::Kind,
    pub frequency: f64,
    pub q: f64,
    pub gain_db: f64,
}
impl Default for Filter {
    fn default() -> Self {
        Self {
            kind: biquad::Kind::Peak,
            frequency: 1000.0,
            q: 1.0,
            gain_db: 0.0,
        }
    }
}

/// Bauer crossfeed cutoff and feeding level (lower dB means stronger crossfeed).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossfeedSettings {
    pub cutoff: f64,
    pub feed_db: f64,
}
impl Default for CrossfeedSettings {
    fn default() -> Self {
        Self {
            cutoff: 700.0,
            feed_db: 4.5,
        }
    }
}

/// Pure settings independent of daemon/state/UI types.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub filters: [Filter; FILTERS],
    pub crossfeed: Option<CrossfeedSettings>,
    pub preamp_db: f64,
    /// -1 left only, +1 right only; mono has no pan and channels beyond the first pair stay unity.
    pub balance: f32,
    pub ceiling_db: f32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            filters: [Filter::default(); FILTERS],
            crossfeed: None,
            preamp_db: 0.0,
            balance: 0.0,
            ceiling_db: -1.0,
        }
    }
}

/// Immutable, versioned coefficients for one stream rate. Compile outside the processing block.
/// Caller versions must change whenever settings change; apply only between complete blocks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Compiled {
    pub(crate) rate: u32,
    pub(crate) version: u64,
    pub(crate) filters: [biquad::Coefficients; FILTERS],
    pub(crate) crossfeed: Option<crossfeed::Coefficients>,
    pub(crate) preamp: f32,
    pub(crate) balance: [f32; 2],
    pub(crate) ceiling_db: f32,
}
impl Compiled {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "H15-67: bounded preamp gain processed in f32"
    )]
    pub fn new(rate: u32, version: u64, settings: Settings) -> io::Result<Self> {
        if !(2000..=384_000).contains(&rate)
            || !settings.balance.is_finite()
            || !(-1.0..=1.0).contains(&settings.balance)
            || !settings.preamp_db.is_finite()
            || !(-20.0..=20.0).contains(&settings.preamp_db)
            || !settings.ceiling_db.is_finite()
            || !(-20.0..=0.0).contains(&settings.ceiling_db)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid DSP settings",
            ));
        }
        let mut filters = [biquad::Coefficients::IDENTITY; FILTERS];
        for (compiled, filter) in filters.iter_mut().zip(settings.filters) {
            *compiled =
                biquad::Coefficients::new(filter.kind, rate, filter.frequency, filter.q, filter.gain_db)?;
        }
        let crossfeed = settings
            .crossfeed
            .map(|c| crossfeed::Coefficients::new(rate, c.cutoff, c.feed_db))
            .transpose()?;
        Ok(Self {
            rate,
            version,
            filters,
            crossfeed,
            preamp: 10.0_f64.powf(settings.preamp_db / 20.0) as f32,
            balance: [
                (1.0 - settings.balance).min(1.0),
                (1.0 + settings.balance).min(1.0),
            ],
            ceiling_db: settings.ceiling_db,
        })
    }
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.rate
    }
}

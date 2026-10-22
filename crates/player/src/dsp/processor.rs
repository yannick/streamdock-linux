use super::{Compiled, FILTERS, biquad::Biquad, crossfeed::Crossfeed, limiter::Limiter};
use std::io;

/// Stateful interleaved f32 DSP. No steady-block allocation or blocking synchronization.
/// Caller atomically selects an immutable `Compiled` snapshot before a block and calls `update`.
#[derive(Debug, Clone)]
pub struct Processor {
    channels: usize,
    config: Compiled,
    filters: Vec<[Biquad; FILTERS]>,
    crossfeed: Option<Crossfeed>,
    limiter: Limiter,
}
impl Processor {
    pub fn new(channels: usize, config: &Compiled) -> io::Result<Self> {
        let limiter = Limiter::new(config.rate, channels, config.ceiling_db)?;
        let filters = (0..channels)
            .map(|_| std::array::from_fn(|i| Biquad::from_coefficients(config.filters[i])))
            .collect();
        Ok(Self {
            channels,
            config: *config,
            filters,
            crossfeed: config.crossfeed.map(Crossfeed::from_coefficients),
            limiter,
        })
    }
    /// Apply a precompiled version between blocks; format changes require a new processor.
    pub fn update(&mut self, config: &Compiled) -> io::Result<()> {
        if config.rate != self.config.rate {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "DSP stream rate changed",
            ));
        }
        if config.version == self.config.version {
            return Ok(());
        }
        for filters in &mut self.filters {
            for (filter, coefficients) in filters.iter_mut().zip(config.filters) {
                filter.set_coefficients(coefficients);
            }
        }
        match (&mut self.crossfeed, config.crossfeed) {
            (Some(state), Some(coefficients)) => state.set_coefficients(coefficients),
            (_, value) => self.crossfeed = value.map(Crossfeed::from_coefficients),
        }
        self.limiter.set_ceiling(config.ceiling_db);
        self.config = *config;
        Ok(())
    }
    /// Process complete frames in place. Malformed samples become silence; extreme finite inputs
    /// are bounded before filters so they cannot overflow the state or poison following samples.
    pub fn process(&mut self, samples: &mut [f32]) -> io::Result<()> {
        if !samples.len().is_multiple_of(self.channels) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "incomplete DSP frame",
            ));
        }
        if !samples
            .iter()
            .all(|sample| sample.is_finite() && sample.abs() <= 32.0)
        {
            for sample in samples.iter_mut() {
                *sample = if sample.is_finite() {
                    sample.clamp(-32.0, 32.0)
                } else {
                    0.0
                };
            }
        }
        for (channel, filters) in self.filters.iter_mut().enumerate() {
            for filter in filters {
                filter.process_block(samples, channel, self.channels);
            }
        }
        for frame in samples.chunks_exact_mut(self.channels) {
            if self.channels == 2
                && let Some(crossfeed) = &mut self.crossfeed
            {
                let pair = crossfeed.process([frame[0], frame[1]]);
                frame.copy_from_slice(&pair);
            }
            for sample in frame.iter_mut() {
                *sample *= self.config.preamp;
            }
            self.limiter.process(frame);
            if self.channels >= 2 {
                frame[0] *= self.config.balance[0];
                frame[1] *= self.config.balance[1];
            }
        }
        Ok(())
    }
    #[must_use]
    pub fn delay_frames(&self) -> usize {
        self.limiter.delay_frames()
    }
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.config.version
    }
    /// Discontinuity/seek reset; ordinary same-format track boundaries need not reset history.
    pub fn reset(&mut self) {
        for filters in &mut self.filters {
            for filter in filters {
                filter.reset();
            }
        }
        if let Some(crossfeed) = &mut self.crossfeed {
            crossfeed.reset();
        }
        self.limiter.reset();
    }
}

//! Isolate the actual engine kernels without adding benchmark switches to the public DSP API.
#[allow(
    dead_code,
    reason = "H15-67: benchmark uses a subset of the production kernel API"
)]
#[path = "../../src/dsp/biquad.rs"]
mod biquad;
#[allow(
    dead_code,
    reason = "H15-67: benchmark uses a subset of the production kernel API"
)]
#[path = "../../src/dsp/crossfeed.rs"]
mod crossfeed;
#[allow(
    dead_code,
    reason = "H15-67: benchmark uses a subset of the production kernel API"
)]
#[path = "../../src/dsp/limiter.rs"]
mod limiter;

use player::dsp::{FilterKind, Settings};
use std::io;
pub struct Ablation {
    filters: Vec<Vec<biquad::Biquad>>,
    crossfeed: crossfeed::Crossfeed,
    limiter: limiter::Limiter,
    stage: u8,
}
impl Ablation {
    pub fn new(rate: u32, settings: &Settings, stage: u8) -> io::Result<Self> {
        let filters = (0..2)
            .map(|_| {
                settings
                    .filters
                    .iter()
                    .map(|f| {
                        let kind = match f.kind {
                            FilterKind::Peak => biquad::Kind::Peak,
                            FilterKind::LowShelf => biquad::Kind::LowShelf,
                            FilterKind::HighShelf => biquad::Kind::HighShelf,
                        };
                        Ok(biquad::Biquad::from_coefficients(biquad::Coefficients::new(
                            kind,
                            rate,
                            f.frequency,
                            f.q,
                            f.gain_db,
                        )?))
                    })
                    .collect::<io::Result<Vec<_>>>()
            })
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Self {
            filters,
            crossfeed: crossfeed::Crossfeed::from_coefficients(crossfeed::Coefficients::new(
                rate, 700.0, 4.5,
            )?),
            limiter: limiter::Limiter::new(rate, 2, -1.0)?,
            stage,
        })
    }
    pub fn process(&mut self, samples: &mut [f32]) {
        if self.stage >= 3 && !samples.iter().all(|x| x.is_finite() && x.abs() <= 32.0) {
            for x in samples.iter_mut() {
                *x = if x.is_finite() { x.clamp(-32.0, 32.0) } else { 0.0 };
            }
        }
        for (channel, filters) in self.filters.iter_mut().enumerate() {
            for filter in filters {
                filter.process_block(samples, channel, 2);
            }
        }
        if self.stage == 0 {
            return;
        }
        for frame in samples.as_chunks_mut::<2>().0 {
            let pair = self.crossfeed.process(*frame);
            *frame = pair;
            if self.stage >= 2 {
                self.limiter.process(frame);
            }
        }
    }
}

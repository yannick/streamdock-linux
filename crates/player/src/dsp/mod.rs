//! Pure audio DSP (H15-67), independent of daemon settings and PipeWire.
mod biquad;
mod crossfeed;
mod limiter;

mod processor;
mod settings;
#[cfg(test)]
mod tests;
pub use biquad::Kind as FilterKind;
pub use processor::Processor;
pub use settings::{Compiled, CrossfeedSettings, FILTERS, Filter, Settings};
mod device;
#[cfg(test)]
mod device_tests;
pub use device::{Controls, DspDevice};

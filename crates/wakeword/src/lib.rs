//! Wake word detection (H15-68) with microWakeWord models, in pure Rust: the audio
//! [`frontend`] the models are trained on, an int8 streaming [`model`] interpreter for their op set
//! (results equal TensorFlow Lite Micro's), and the [`detector`] that turns probabilities into
//! "heard".

pub mod builtin;
pub mod detector;
pub mod frontend;
pub mod model;

mod filterbank_tables;
mod flatbuffer;
mod kernels;

pub use detector::{Detector, Score};
pub use model::Model;

/// Why a model can't be loaded or run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("not a valid model file ({0})")]
    Malformed(&'static str),
    #[error("unsupported model: {0}")]
    Unsupported(&'static str),
    #[error("bad input: {0}")]
    Input(&'static str),
}

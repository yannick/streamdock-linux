//! Fixture helpers: the WAVs (16 kHz mono 16-bit, Piper TTS) and the reference outputs that
//! `pymicro-features` / `pymicro-wakeword` produced for them (H15-68).

#![allow(dead_code, reason = "each test file uses part of it")]

use std::path::PathBuf;

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The samples of a canonical 44-byte-header PCM WAV.
pub fn wav(name: &str) -> Vec<i16> {
    let bytes = std::fs::read(fixture(&format!("{name}.wav"))).unwrap();
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[36..40], b"data");
    bytes[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect()
}

/// Whitespace-separated numbers, one row per line.
pub fn rows(name: &str) -> Vec<Vec<i64>> {
    std::fs::read_to_string(fixture(name))
        .unwrap()
        .lines()
        .map(|l| l.split_whitespace().map(|v| v.parse().unwrap()).collect())
        .collect()
}

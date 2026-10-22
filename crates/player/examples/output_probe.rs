//! H15-62: repeatable isolated pw-cat probe. Stop the Lyrion player and mute the selected output
//! first. Run as streamdockd with group sound and `PIPEWIRE_RUNTIME_DIR=/run/user/1001`.
//! Usage: `output_probe` <DSP node id> <stereo f32le file, 44100 Hz> <chunk ms> <latency ms>
//!                     <f32|s16> <seconds>
//! The input is looped; generate it from a test signal or decode the approved benchmark track.
//! Measures the pipe/format path only; final validation must use the real Player too.

use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn wav_header(bits: u16, float: bool) -> [u8; 44] {
    let block = 2 * bits / 8;
    let mut h = [0; 44];
    h[..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&(if float { 3u16 } else { 1u16 }).to_le_bytes());
    h[22..24].copy_from_slice(&2u16.to_le_bytes());
    h[24..28].copy_from_slice(&44_100u32.to_le_bytes());
    h[28..32].copy_from_slice(&(44_100 * u32::from(block)).to_le_bytes());
    h[32..34].copy_from_slice(&block.to_le_bytes());
    h[34..36].copy_from_slice(&bits.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    h
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "H15-62: explicit saturating PCM quantization"
)]
fn encode(samples: &[f32], float: bool, bytes: &mut Vec<u8>) {
    bytes.clear();
    for sample in samples {
        if float {
            bytes.extend_from_slice(&sample.to_le_bytes());
        } else {
            bytes.extend_from_slice(&((*sample * 32_768.0) as i16).to_le_bytes());
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if !(7..=8).contains(&args.len()) {
        return Err(
            "usage: output_probe NODE F32LE_FILE CHUNK_MS LATENCY_MS f32|s16 SECONDS [RESAMPLE_QUALITY]"
                .into(),
        );
    }
    let quality: u8 = args.get(7).map_or(Ok(4), |q| q.parse())?;
    if quality > 15 {
        return Err("resampler quality must be 0..15".into());
    }
    let node: u32 = args[1].parse()?;
    let chunk_ms: usize = args[3].parse()?;
    let latency_ms: u32 = args[4].parse()?;
    let float = match args[5].as_str() {
        "f32" => true,
        "s16" => false,
        _ => return Err("format must be f32 or s16".into()),
    };
    let seconds: u64 = args[6].parse()?;
    if !(1..=1000).contains(&chunk_ms) || seconds == 0 || latency_ms == 0 {
        return Err("chunk must be 1..1000 ms, seconds and latency must be positive".into());
    }
    let input = std::fs::read(&args[2])?;
    if input.is_empty() || !input.len().is_multiple_of(8) {
        return Err("input must contain complete stereo f32 frames".into());
    }
    let samples: Vec<f32> = input
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    let mut child = Command::new("pw-cat")
        .args(["--playback", "--media-role", "Music", "--quality"])
        .arg(quality.to_string())
        .arg(format!("--target={node}"))
        .arg(format!("--latency={latency_ms}ms"))
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()?;
    let mut pipe = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("missing pipe"))?;
    pipe.write_all(&wav_header(if float { 32 } else { 16 }, float))?;
    let count = chunk_ms * 44_100 / 1000 * 2;
    let mut block = vec![0.0; count];
    let mut bytes = Vec::with_capacity(count * 4);
    let start = Instant::now();
    let mut at = 0;
    let mut writes = 0u64;
    let result: io::Result<()> = (|| {
        while start.elapsed() < Duration::from_secs(seconds) {
            for sample in &mut block {
                *sample = samples[at];
                at = (at + 1) % samples.len();
            }
            encode(&block, float, &mut bytes);
            pipe.write_all(&bytes)?;
            writes += 1;
        }
        Ok(())
    })();
    drop(pipe);
    let status = child.wait()?;
    result?;
    if !status.success() {
        return Err(format!("pw-cat exited {status}").into());
    }
    println!(
        "chunk_ms={chunk_ms} latency_ms={latency_ms} format={} writes={writes} wall={:.3}",
        args[5],
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

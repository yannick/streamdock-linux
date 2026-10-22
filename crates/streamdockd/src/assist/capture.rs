//! Where the spoken request comes from (H15-43): 16 kHz mono 16-bit little-endian PCM, as Home
//! Assistant's assist pipeline expects it. A microphone through PipeWire (`pw-record`), or a WAV
//! file for tests and for the board until it has a microphone (`test_input`).

use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::BoxFuture;
use tokio::io::AsyncReadExt as _;

/// The assist pipeline's input format.
pub const RATE: u32 = 16_000;
/// Samples per block: 20 ms.
const BLOCK: usize = 320;

/// A source of 16 kHz mono s16le audio.
pub trait Capture: Send {
    /// The next block (whole samples, little-endian); `None` at the end of the source.
    fn next(&mut self) -> BoxFuture<'_, io::Result<Option<Bytes>>>;

    /// Whether releasing the key ends the request (a microphone), or the source plays to its end
    /// (a file: the sentence is never cut, however briefly the key was held).
    fn ends_on_release(&self) -> bool;
}

/// A microphone through PipeWire: `pw-record` at 16 kHz mono s16 to stdout.
pub struct PwRecord {
    child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    /// A WAV header in front of the samples is skipped (older `pw-record` writes one to stdout).
    header: HeaderSkip,
    buf: Vec<u8>,
}

impl std::fmt::Debug for PwRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PwRecord").finish_non_exhaustive()
    }
}

impl PwRecord {
    /// Starts `pw-record`; `env` points it at the PipeWire instance (the board's user `sound`),
    /// `target` is a source node name or id (default: PipeWire's default source).
    pub fn start(env: &[(String, String)], target: Option<&str>) -> io::Result<Self> {
        let mut command = tokio::process::Command::new("pw-record");
        command
            .args([
                "--rate",
                "16000",
                "--channels",
                "1",
                "--format",
                "s16",
                "--media-role",
                "Communication",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .envs(env.iter().map(|(k, v)| (k, v)));
        if let Some(target) = target {
            command.arg(format!("--target={target}"));
        }
        let mut child = command.arg("-").spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("pw-record without stdout"))?;
        Ok(Self {
            child,
            stdout,
            header: HeaderSkip::default(),
            buf: vec![0; BLOCK * 2],
        })
    }
}

impl Capture for PwRecord {
    fn next(&mut self) -> BoxFuture<'_, io::Result<Option<Bytes>>> {
        Box::pin(async move {
            loop {
                let n = self.stdout.read(&mut self.buf).await?;
                if n == 0 {
                    let status = self.child.wait().await?;
                    if status.success() {
                        return Ok(None);
                    }
                    return Err(io::Error::other(format!("pw-record exited with {status}")));
                }
                let audio = self.header.feed(&self.buf[..n]);
                if !audio.is_empty() {
                    return Ok(Some(Bytes::copy_from_slice(audio)));
                }
            }
        })
    }

    fn ends_on_release(&self) -> bool {
        true
    }
}

/// Drops a leading WAV header (up to the `data` chunk) from a byte stream, if there is one.
#[derive(Debug, Default)]
struct HeaderSkip {
    done: bool,
    head: Vec<u8>,
}

impl HeaderSkip {
    fn feed<'a>(&'a mut self, bytes: &'a [u8]) -> &'a [u8] {
        if self.done {
            return bytes;
        }
        self.head.extend_from_slice(bytes);
        if self.head.len() < 4 {
            return &[];
        }
        if &self.head[..4] != b"RIFF" {
            self.done = true;
            return &self.head;
        }
        // RIFF....WAVE, then chunks; the samples start after the `data` chunk header.
        let mut at = 12;
        while at + 8 <= self.head.len() {
            let len = u32::from_le_bytes([
                self.head[at + 4],
                self.head[at + 5],
                self.head[at + 6],
                self.head[at + 7],
            ]) as usize;
            if &self.head[at..at + 4] == b"data" {
                self.done = true;
                return &self.head[at + 8..];
            }
            at += 8 + len + (len & 1);
        }
        &[]
    }
}

/// A WAV file as the microphone, played at real-time pace.
#[derive(Debug)]
pub struct WavFile {
    samples: Vec<i16>,
    at: usize,
    pace: tokio::time::Interval,
}

impl WavFile {
    /// Reads a 16-bit PCM WAV (any rate and channel count: mixed to mono, resampled to 16 kHz).
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        let samples = wav_to_16k_mono(&bytes)?;
        let mut pace = tokio::time::interval(Duration::from_millis(20));
        pace.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
        Ok(Self { samples, at: 0, pace })
    }
}

impl Capture for WavFile {
    fn next(&mut self) -> BoxFuture<'_, io::Result<Option<Bytes>>> {
        Box::pin(async move {
            if self.at >= self.samples.len() {
                return Ok(None);
            }
            self.pace.tick().await;
            let end = (self.at + BLOCK).min(self.samples.len());
            let block: Vec<u8> = self.samples[self.at..end]
                .iter()
                .flat_map(|s| s.to_le_bytes())
                .collect();
            self.at = end;
            Ok(Some(Bytes::from(block)))
        })
    }

    fn ends_on_release(&self) -> bool {
        false
    }
}

/// A 16-bit PCM WAV as 16 kHz mono samples (channels averaged, linear resampling).
pub fn wav_to_16k_mono(bytes: &[u8]) -> anyhow::Result<Vec<i16>> {
    anyhow::ensure!(
        bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "not a WAV file"
    );
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let (mut format, mut data) = (None, None);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let len = u32_at(at + 4) as usize;
        let body = at + 8;
        let end = (body + len).min(bytes.len());
        match &bytes[at..at + 4] {
            b"fmt " if len >= 16 && end - body >= 16 => {
                format = Some((
                    u16_at(body),
                    u16_at(body + 2),
                    u32_at(body + 4),
                    u16_at(body + 14),
                ));
            }
            b"data" => data = Some(&bytes[body..end]),
            _ => {}
        }
        at = body + len + (len & 1);
    }
    let (tag, channels, rate, bits) = format.ok_or_else(|| anyhow::anyhow!("WAV without fmt chunk"))?;
    let data = data.ok_or_else(|| anyhow::anyhow!("WAV without data chunk"))?;
    anyhow::ensure!(
        (tag == 1 || tag == 0xFFFE) && bits == 16 && channels > 0 && rate > 0,
        "the test input must be 16-bit PCM WAV (got format {tag}, {bits} bit, {channels} channels)"
    );
    let channels = usize::from(channels);
    let mono: Vec<f32> = data
        .as_chunks::<2>()
        .0
        .chunks_exact(channels)
        .map(|frame| {
            let sum: f32 = frame.iter().map(|&s| f32::from(i16::from_le_bytes(s))).sum();
            #[allow(clippy::cast_precision_loss)] // H15-43: a channel count
            let n = channels as f32;
            sum / n
        })
        .collect();
    Ok(resample(&mono, rate, RATE))
}

/// Linear resampling; good enough for speech recognition.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<i16> {
    if input.is_empty() {
        return Vec::new();
    }
    let step = f64::from(from) / f64::from(to);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "H15-43: sample positions and values"
    )]
    {
        let out_len = (input.len() as f64 / step).floor() as usize;
        (0..out_len)
            .map(|i| {
                let pos = i as f64 * step;
                let j = pos as usize;
                let frac = (pos - j as f64) as f32;
                let a = input[j];
                let b = input.get(j + 1).copied().unwrap_or(a);
                (a + (b - a) * frac).round().clamp(-32_768.0, 32_767.0) as i16
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut v = b"RIFF\0\0\0\0WAVE".to_vec();
        v.extend_from_slice(b"fmt \x10\0\0\0\x01\0");
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * 2 * u32::from(channels)).to_le_bytes());
        v.extend_from_slice(&(2 * channels).to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"LIST\x02\0\0\0xy");
        v.extend_from_slice(b"data");
        v.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        v.extend_from_slice(&data);
        v
    }

    #[test]
    fn wav_is_mixed_and_resampled() {
        // 48 kHz stereo, 4800 frames = 0.1 s → 1600 samples at 16 kHz.
        let frames: Vec<i16> = (0..4800).flat_map(|_| [1000, 3000]).collect();
        let out = wav_to_16k_mono(&wav(48_000, 2, &frames)).unwrap();
        assert_eq!(out.len(), 1600);
        assert!(out.iter().all(|&s| s == 2000));
        // 16 kHz mono passes unchanged.
        let same: Vec<i16> = (0..100).collect();
        assert_eq!(wav_to_16k_mono(&wav(16_000, 1, &same)).unwrap(), same);
    }

    #[test]
    fn bad_wavs_are_errors() {
        assert!(wav_to_16k_mono(b"nope").is_err());
        let mut eight_bit = wav(16_000, 1, &[0; 4]);
        eight_bit[34] = 8;
        assert!(wav_to_16k_mono(&eight_bit).is_err());
    }

    #[test]
    fn header_skip() {
        let mut skip = HeaderSkip::default();
        let file = wav(16_000, 1, &[1, 2, 3]);
        let (a, b) = file.split_at(20);
        assert_eq!(skip.feed(a), [0u8; 0]);
        assert_eq!(skip.feed(b), [1, 0, 2, 0, 3, 0]);
        assert_eq!(skip.feed(&[9, 9]), [9, 9]);
        let mut raw = HeaderSkip::default();
        assert_eq!(raw.feed(&[5, 0, 6, 0]), [5, 0, 6, 0]);
    }

    #[tokio::test]
    async fn wav_file_streams_blocks() {
        let dir = std::env::temp_dir().join(format!("assist-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("in.wav");
        std::fs::write(&path, wav(16_000, 1, &vec![7; 700])).unwrap();
        let mut file = WavFile::open(&path).unwrap();
        assert!(!file.ends_on_release());
        let mut total = 0;
        while let Some(block) = file.next().await.unwrap() {
            total += block.len();
        }
        assert_eq!(total, 1400);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

//! Decoding one track: its byte stream to frames in the sink.

use std::io::{self, Read};

use slimproto::Format;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

use super::TrackId;
use super::pcm::{self, PcmSpec};
use super::source::{Cancel, Cancelled, StreamSource};
use crate::{Closed, FrameSink};

/// How a track ended.
#[derive(Debug, PartialEq)]
pub(super) enum Outcome {
    /// The whole stream was decoded.
    Done,
    /// Stopped or flushed (by the handle or the sink): nothing to report.
    Cancelled,
    Failed(String),
}

impl From<Closed> for Outcome {
    fn from(_: Closed) -> Self {
        Self::Cancelled
    }
}

/// Buffers reused from track to track, so the per-packet path does not allocate once they have
/// grown to the largest packet.
#[derive(Debug, Default)]
pub(super) struct Scratch {
    samples: Vec<f32>,
    bytes: Vec<u8>,
}

/// Raw PCM is read in blocks of about this many bytes.
const PCM_BLOCK: usize = 32 * 1024;

/// Writes frames of one track to the sink, announcing the track before its first frame.
struct Out<'a> {
    id: TrackId,
    gain: f32,
    cancel: &'a Cancel,
    sink: &'a mut dyn FrameSink,
    format: Option<(u32, u16)>,
}

impl Out<'_> {
    fn begin(&mut self, rate: u32, channels: u16) -> Result<(), Outcome> {
        match self.format {
            None => {
                self.sink.begin_track(self.id, rate, channels);
                self.format = Some((rate, channels));
                Ok(())
            }
            Some(f) if f == (rate, channels) => Ok(()),
            Some((r, c)) => Err(Outcome::Failed(format!(
                "format changed within the track: {r} Hz × {c} to {rate} Hz × {channels}"
            ))),
        }
    }

    fn write(&mut self, samples: &[f32]) -> Result<(), Outcome> {
        // Checked here too: a sink that still takes frames must not keep a flushed track going.
        if self.cancel.is_cancelled() {
            return Err(Outcome::Cancelled);
        }
        if samples.is_empty() {
            return Ok(());
        }
        Ok(self.sink.write(samples)?)
    }

    fn finish(self) -> Outcome {
        if self.format.is_none() {
            return Outcome::Failed("stream contains no audio".into());
        }
        self.sink.end_track();
        Outcome::Done
    }
}

/// Decodes `source` as `format` into `sink`.
pub(super) fn decode(
    id: TrackId,
    format: Format,
    gain: f32,
    cancel: &Cancel,
    source: StreamSource<'_>,
    sink: &mut dyn FrameSink,
    scratch: &mut Scratch,
) -> Outcome {
    let out = Out {
        id,
        gain,
        cancel,
        sink,
        format: None,
    };
    let result = match format {
        Format::Pcm { .. } => decode_pcm(format, source, out, scratch),
        Format::Flac => decode_symphonia("flac", source, out, &mut scratch.samples),
        Format::Mp3 => decode_symphonia("mp3", source, out, &mut scratch.samples),
    };
    result.unwrap_or_else(|outcome| outcome)
}

/// A read error: cancellation, or a failure of the stream.
fn io_outcome(e: &io::Error) -> Outcome {
    if e.get_ref()
        .is_some_and(<dyn std::error::Error + Send + Sync>::is::<Cancelled>)
    {
        Outcome::Cancelled
    } else {
        Outcome::Failed(format!("stream: {e}"))
    }
}

fn decode_pcm(
    format: Format,
    mut source: StreamSource<'_>,
    mut out: Out<'_>,
    scratch: &mut Scratch,
) -> Result<Outcome, Outcome> {
    let spec = match PcmSpec::from_strm(format) {
        Some(spec) => spec,
        None => pcm::read_header(&mut source).map_err(|e| io_outcome(&e))?,
    };
    spec.validate().map_err(Outcome::Failed)?;
    out.begin(spec.rate, spec.channels)?;

    let frame = spec.frame_bytes();
    let block = (PCM_BLOCK / frame).max(1) * frame;
    let buf = &mut scratch.bytes;
    buf.resize(block, 0);
    let mut reader: Box<dyn Read + '_> = match spec.data_len {
        Some(len) => Box::new(source.take(len)),
        None => Box::new(source),
    };
    let mut filled = 0;
    loop {
        let n = match reader.read(&mut buf[filled..]) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_outcome(&e)),
        };
        filled += n;
        // Whatever whole frames arrived go out now: a slow stream must not wait for a block.
        let whole = filled / frame * frame;
        if whole > 0 {
            scratch.samples.clear();
            pcm::convert(&spec, &buf[..whole], out.gain, &mut scratch.samples);
            out.write(&scratch.samples)?;
            buf.copy_within(whole..filled, 0);
            filled -= whole;
        }
        if n == 0 {
            // A trailing partial frame is dropped.
            return Ok(out.finish());
        }
    }
}

fn symphonia_outcome(e: SymphoniaError) -> Outcome {
    match e {
        SymphoniaError::IoError(e) => io_outcome(&e),
        other => Outcome::Failed(other.to_string()),
    }
}

/// The end of a stream: symphonia's readers report it as an unexpected EOF.
fn is_eof(e: &SymphoniaError) -> bool {
    matches!(e, SymphoniaError::IoError(e) if e.kind() == io::ErrorKind::UnexpectedEof)
}

fn decode_symphonia(
    extension: &str,
    source: StreamSource<'_>,
    mut out: Out<'_>,
    samples: &mut Vec<f32>,
) -> Result<Outcome, Outcome> {
    let mss = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension(extension);
    let mut reader = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(symphonia_outcome)?;
    let track = reader
        .default_track(TrackType::Audio)
        .ok_or_else(|| Outcome::Failed("no audio track".into()))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| Outcome::Failed("no audio codec parameters".into()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(symphonia_outcome)?;

    loop {
        let packet = match reader.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(e) if is_eof(&e) => break,
            Err(e) => return Err(symphonia_outcome(e)),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(buf) => buf,
            // A damaged packet: skip it, as players do.
            Err(SymphoniaError::DecodeError(msg)) => {
                tracing::debug!(msg, "skipping undecodable packet");
                continue;
            }
            Err(e) => return Err(symphonia_outcome(e)),
        };
        let spec = buf.spec();
        let channels = u16::try_from(spec.channels().count()).unwrap_or(u16::MAX);
        out.begin(spec.rate(), channels)?;
        samples.resize(buf.samples_interleaved(), 0.0);
        buf.copy_to_slice_interleaved(&mut samples[..]);
        if (out.gain - 1.0).abs() > f32::EPSILON {
            for s in samples.iter_mut() {
                *s *= out.gain;
            }
        }
        out.write(samples)?;
    }
    Ok(out.finish())
}

//! Decoder tests (H15-56): fixtures from `tests/fixtures/gen.sh` (synthetic tones) and PCM / WAV /
//! AIFF streams built here.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use player::{Closed, Decoder, DecoderConfig, FrameSink, TrackId};
use slimproto::{Format, PlaybackEvent, Track};
use tokio::sync::mpsc;

const FLAC: &[u8] = include_bytes!("fixtures/tone.flac");
const MP3: &[u8] = include_bytes!("fixtures/tone.mp3");
/// Frames in the fixtures: 0.5 s at 44.1 kHz.
const FIXTURE_FRAMES: usize = 22_050;

#[derive(Debug, Clone, PartialEq)]
enum Entry {
    Begin(TrackId, u32, u16),
    End,
}

#[derive(Debug, Default)]
struct Log {
    marks: Vec<Entry>,
    samples: Vec<f32>,
    /// Sample count when each mark was recorded.
    at: Vec<usize>,
    /// `write` returns `Closed` from now on.
    closed: bool,
}

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Log>>);

impl Sink {
    fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.0.lock().unwrap()
    }
}

impl FrameSink for Sink {
    fn begin_track(&mut self, track: TrackId, rate: u32, channels: u16) {
        let mut log = self.log();
        let at = log.samples.len();
        log.marks.push(Entry::Begin(track, rate, channels));
        log.at.push(at);
    }

    fn write(&mut self, frames: &[f32]) -> Result<(), Closed> {
        let mut log = self.log();
        if log.closed {
            return Err(Closed);
        }
        log.samples.extend_from_slice(frames);
        Ok(())
    }

    fn end_track(&mut self) {
        let mut log = self.log();
        let at = log.samples.len();
        log.marks.push(Entry::End);
        log.at.push(at);
    }
}

fn track(format: Format, replay_gain: f32) -> Track {
    Track {
        format,
        replay_gain,
        autostart: true,
        output_threshold: Duration::from_secs(1),
        transition: slimproto::Transition::default(),
    }
}

fn pcm(rate: u32, bits: u8, channels: u8, big_endian: bool) -> Format {
    Format::Pcm {
        sample_rate: Some(rate),
        sample_size: Some(bits),
        channels: Some(channels),
        big_endian: Some(big_endian),
    }
}

const FROM_HEADER: Format = Format::Pcm {
    sample_rate: None,
    sample_size: None,
    channels: None,
    big_endian: None,
};

/// The whole body, in chunks of `chunk` bytes, then the end of the stream.
fn body(data: &[u8], chunk: usize) -> mpsc::Receiver<Bytes> {
    let chunks: Vec<_> = data.chunks(chunk).map(Bytes::copy_from_slice).collect();
    let (tx, rx) = mpsc::channel(chunks.len().max(1));
    for c in chunks {
        tx.try_send(c).unwrap();
    }
    rx
}

/// Waits until the decoder reported `n` events.
fn events(decoder: &Decoder, n: usize) -> Vec<PlaybackEvent> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut all = Vec::new();
    while all.len() < n {
        assert!(Instant::now() < deadline, "timed out, events so far: {all:?}");
        all.extend(decoder.take_events());
        std::thread::sleep(Duration::from_millis(5));
    }
    all
}

fn wait_idle(decoder: &Decoder) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while decoder.is_busy() {
        assert!(Instant::now() < deadline, "decoder still busy");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Decodes one track to the end and returns the log.
fn decode_one(format: Format, gain: f32, data: &[u8]) -> (TrackId, Log, Vec<PlaybackEvent>) {
    let sink = Sink::default();
    let decoder = Decoder::spawn(sink.clone(), DecoderConfig::default()).unwrap();
    let id = decoder.start(track(format, gain), body(data, 1000));
    let ev = events(&decoder, 1);
    wait_idle(&decoder);
    drop(decoder);
    let log = std::mem::take(&mut *sink.log());
    (id, log, ev)
}

#[test]
fn flac_fixture() {
    let (id, log, ev) = decode_one(Format::Flac, 1.0, FLAC);
    assert_eq!(ev, [PlaybackEvent::DecodeDone]);
    assert_eq!(log.marks, [Entry::Begin(id, 44_100, 2), Entry::End]);
    assert_eq!(log.samples.len(), FIXTURE_FRAMES * 2);
    let peak = log.samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.1 && peak <= 1.0, "peak {peak}");
}

#[test]
fn mp3_fixture_is_trimmed_gapless() {
    let (id, log, ev) = decode_one(Format::Mp3, 1.0, MP3);
    assert_eq!(ev, [PlaybackEvent::DecodeDone]);
    assert_eq!(log.marks, [Entry::Begin(id, 44_100, 2), Entry::End]);
    // The LAME header's encoder delay and padding are cut off.
    assert_eq!(log.samples.len(), FIXTURE_FRAMES * 2);
}

#[test]
fn replay_gain_is_applied() {
    let (_, full, _) = decode_one(Format::Flac, 1.0, FLAC);
    let (_, half, _) = decode_one(Format::Flac, 0.5, FLAC);
    assert_eq!(full.samples.len(), half.samples.len());
    for (a, b) in full.samples.iter().zip(&half.samples) {
        assert!((a * 0.5 - b).abs() < 1e-6);
    }
}

#[test]
fn raw_pcm_layouts() {
    // Two stereo frames with the values 0.5, -0.5, 0.25, -1.0 in each layout.
    let cases: [(u8, bool, Vec<u8>); 5] = [
        (8, false, vec![0x40, 0xC0, 0x20, 0x80]),
        (16, false, vec![0, 0x40, 0, 0xC0, 0, 0x20, 0, 0x80]),
        (16, true, vec![0x40, 0, 0xC0, 0, 0x20, 0, 0x80, 0]),
        (24, true, vec![0x40, 0, 0, 0xC0, 0, 0, 0x20, 0, 0, 0x80, 0, 0]),
        (
            32,
            false,
            vec![0, 0, 0, 0x40, 0, 0, 0, 0xC0, 0, 0, 0, 0x20, 0, 0, 0, 0x80],
        ),
    ];
    for (bits, big, data) in cases {
        let (id, log, ev) = decode_one(pcm(48_000, bits, 2, big), 1.0, &data);
        assert_eq!(ev, [PlaybackEvent::DecodeDone], "{bits} bit");
        assert_eq!(log.marks, [Entry::Begin(id, 48_000, 2), Entry::End]);
        assert_eq!(log.samples, [0.5, -0.5, 0.25, -1.0], "{bits} bit big {big}");
    }
}

#[test]
fn raw_pcm_split_frames_across_chunks() {
    // 16-bit stereo, 1000 frames, delivered in 7-byte chunks (never frame-aligned).
    let data: Vec<u8> = (0..4000u32).map(|i| (i % 251) as u8).collect();
    let sink = Sink::default();
    let decoder = Decoder::spawn(sink.clone(), DecoderConfig::default()).unwrap();
    let _ = decoder.start(track(pcm(44_100, 16, 2, false), 1.0), body(&data, 7));
    assert_eq!(events(&decoder, 1), [PlaybackEvent::DecodeDone]);
    let log = sink.log();
    assert_eq!(log.samples.len(), 2000);
    let expected = f32::from(i16::from_le_bytes([data[2], data[3]])) / 32_768.0;
    assert!((log.samples[1] - expected).abs() < 1e-7);
}

fn wav(bits: u16, channels: u16, rate: u32, data: &[u8], data_len: u32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF\0\0\0\0WAVE");
    v.extend_from_slice(b"LIST\x04\0\0\0INFO");
    v.extend_from_slice(b"fmt \x10\0\0\0\x01\0");
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    let align = bits / 8 * channels;
    v.extend_from_slice(&(rate * u32::from(align)).to_le_bytes());
    v.extend_from_slice(&align.to_le_bytes());
    v.extend_from_slice(&bits.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    v.extend_from_slice(data);
    v
}

#[test]
fn wav_header() {
    let samples = [0x00, 0x40, 0x00, 0xC0];
    let mut stream = wav(16, 1, 22_050, &samples, 4);
    // A chunk after the data must not be played.
    stream.extend_from_slice(b"id3 \x02\0\0\0xx");
    let (id, log, ev) = decode_one(FROM_HEADER, 1.0, &stream);
    assert_eq!(ev, [PlaybackEvent::DecodeDone]);
    assert_eq!(log.marks, [Entry::Begin(id, 22_050, 1), Entry::End]);
    assert_eq!(log.samples, [0.5, -0.5]);
}

#[test]
fn wav_header_streamed_without_length() {
    // 8-bit WAV is unsigned.
    let stream = wav(8, 2, 8_000, &[0xC0, 0x40, 0x80, 0x00], u32::MAX);
    let (_, log, ev) = decode_one(FROM_HEADER, 1.0, &stream);
    assert_eq!(ev, [PlaybackEvent::DecodeDone]);
    assert_eq!(log.samples, [0.5, -0.5, 0.0, -1.0]);
}

fn aiff(bits: u16, channels: u16, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"FORM\0\0\0\0AIFF");
    v.extend_from_slice(b"COMM\0\0\0\x12");
    v.extend_from_slice(&channels.to_be_bytes());
    let frames = data.len() / usize::from(bits / 8 * channels);
    v.extend_from_slice(&u32::try_from(frames).unwrap().to_be_bytes());
    v.extend_from_slice(&bits.to_be_bytes());
    // 44100 as an 80-bit extended float.
    v.extend_from_slice(&[0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]);
    v.extend_from_slice(b"SSND");
    v.extend_from_slice(&(u32::try_from(data.len()).unwrap() + 8).to_be_bytes());
    v.extend_from_slice(&[0; 8]);
    v.extend_from_slice(data);
    v
}

#[test]
fn aiff_header() {
    let stream = aiff(24, 2, &[0x40, 0, 0, 0xC0, 0, 0, 0x20, 0, 0, 0x80, 0, 0]);
    let (id, log, ev) = decode_one(FROM_HEADER, 1.0, &stream);
    assert_eq!(ev, [PlaybackEvent::DecodeDone]);
    assert_eq!(log.marks, [Entry::Begin(id, 44_100, 2), Entry::End]);
    assert_eq!(log.samples, [0.5, -0.5, 0.25, -1.0]);
}

#[test]
fn errors_are_reported() {
    let (_, log, ev) = decode_one(Format::Flac, 1.0, b"definitely not a flac stream");
    assert!(matches!(&ev[..], [PlaybackEvent::DecodeError(_)]), "{ev:?}");
    assert_eq!(log.marks, []);

    let (_, _, ev) = decode_one(FROM_HEADER, 1.0, b"no header here at all");
    assert!(matches!(&ev[..], [PlaybackEvent::DecodeError(_)]), "{ev:?}");

    let (_, _, ev) = decode_one(Format::Mp3, 1.0, b"");
    assert!(matches!(&ev[..], [PlaybackEvent::DecodeError(_)]), "{ev:?}");
}

#[test]
fn gapless_tracks_follow_each_other() {
    let sink = Sink::default();
    let decoder = Decoder::spawn(sink.clone(), DecoderConfig::default()).unwrap();
    let first = decoder.start(track(Format::Flac, 1.0), body(FLAC, 4096));
    let second = decoder.start(track(Format::Mp3, 1.0), body(MP3, 4096));
    assert!(first < second);
    let ev = events(&decoder, 2);
    assert_eq!(ev, [PlaybackEvent::DecodeDone, PlaybackEvent::DecodeDone]);
    let log = sink.log();
    assert_eq!(
        log.marks,
        [
            Entry::Begin(first, 44_100, 2),
            Entry::End,
            Entry::Begin(second, 44_100, 2),
            Entry::End
        ]
    );
    let n = FIXTURE_FRAMES * 2;
    assert_eq!(log.at, [0, n, n, 2 * n]);
}

#[test]
fn stop_cancels_a_waiting_track_and_the_queue() {
    let sink = Sink::default();
    let decoder = Decoder::spawn(sink.clone(), DecoderConfig::default()).unwrap();
    // The body stays open: the decoder waits for more data.
    let (tx, rx) = mpsc::channel(4);
    tx.try_send(Bytes::from_static(&[0; 400])).unwrap();
    let _ = decoder.start(track(pcm(44_100, 16, 2, false), 1.0), rx);
    let _ = decoder.start(track(Format::Flac, 1.0), body(FLAC, 4096));
    let deadline = Instant::now() + Duration::from_secs(5);
    while sink.log().samples.len() < 200 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    decoder.stop();
    wait_idle(&decoder);
    assert_eq!(decoder.take_events(), []);
    assert_eq!(sink.log().marks.len(), 1, "queued track was dropped");
    drop(tx);

    // The decoder still takes new tracks.
    let next = decoder.start(track(Format::Flac, 1.0), body(FLAC, 4096));
    assert_eq!(events(&decoder, 1), [PlaybackEvent::DecodeDone]);
    assert_eq!(sink.log().marks[1], Entry::Begin(next, 44_100, 2));
}

#[test]
fn flush_next_keeps_the_playing_track() {
    let sink = Sink::default();
    let decoder = Decoder::spawn(sink.clone(), DecoderConfig::default()).unwrap();
    let (tx, rx) = mpsc::channel(4);
    tx.try_send(Bytes::from_static(&[0; 400])).unwrap();
    let playing = decoder.start(track(pcm(44_100, 16, 2, false), 1.0), rx);
    let _ = decoder.start(track(Format::Flac, 1.0), body(FLAC, 4096));
    decoder.flush_next(Some(playing));
    // The playing track goes on and ends normally; the next one is gone.
    tx.try_send(Bytes::from_static(&[0; 400])).unwrap();
    drop(tx);
    assert_eq!(events(&decoder, 1), [PlaybackEvent::DecodeDone]);
    wait_idle(&decoder);
    let log = sink.log();
    assert_eq!(log.marks, [Entry::Begin(playing, 44_100, 2), Entry::End]);
    assert_eq!(log.samples.len(), 400);
}

#[test]
fn flush_next_cancels_the_next_track_while_it_decodes() {
    let sink = Sink::default();
    let decoder = Decoder::spawn(sink.clone(), DecoderConfig::default()).unwrap();
    let playing = decoder.start(track(Format::Flac, 1.0), body(FLAC, 4096));
    assert_eq!(events(&decoder, 1), [PlaybackEvent::DecodeDone]);
    let (tx, rx) = mpsc::channel(4);
    tx.try_send(Bytes::from_static(&[0; 400])).unwrap();
    let next = decoder.start(track(pcm(44_100, 16, 2, false), 1.0), rx);
    let deadline = Instant::now() + Duration::from_secs(5);
    while sink.log().marks.len() < 3 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    decoder.flush_next(Some(playing));
    wait_idle(&decoder);
    assert_eq!(decoder.take_events(), []);
    // Begun, never ended: the output drops its frames.
    assert_eq!(sink.log().marks.last(), Some(&Entry::Begin(next, 44_100, 2)));
    drop(tx);
}

#[test]
fn closed_sink_abandons_the_track_silently() {
    let sink = Sink::default();
    sink.log().closed = true;
    let decoder = Decoder::spawn(sink.clone(), DecoderConfig::default()).unwrap();
    let _ = decoder.start(track(Format::Flac, 1.0), body(FLAC, 4096));
    wait_idle(&decoder);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(decoder.take_events(), []);
    assert_eq!(sink.log().marks.len(), 1, "begun, never ended");
}

/// A sink that blocks every write until released, to see the input buffer fill.
struct Blocking(Arc<(Mutex<bool>, std::sync::Condvar)>);

impl FrameSink for Blocking {
    fn begin_track(&mut self, _: TrackId, _: u32, _: u16) {}

    fn write(&mut self, _: &[f32]) -> Result<(), Closed> {
        let (open, cv) = &*self.0;
        let mut open = open.lock().unwrap();
        while !*open {
            open = cv.wait(open).unwrap();
        }
        Ok(())
    }
}

#[test]
fn input_fill_while_the_sink_applies_back_pressure() {
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let config = DecoderConfig { input_buffer: 8192 };
    let decoder = Decoder::spawn(Blocking(Arc::clone(&gate)), config).unwrap();
    let _ = decoder.start(
        track(pcm(44_100, 16, 2, false), 1.0),
        body(&vec![0; 100_000], 1000),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while decoder.input_fill().0 < 4096 {
        assert!(Instant::now() < deadline, "fill {:?}", decoder.input_fill());
        std::thread::sleep(Duration::from_millis(5));
    }
    let (fill, size) = decoder.input_fill();
    assert_eq!(size, 8192);
    assert!(fill <= size);
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    assert_eq!(events(&decoder, 1), [PlaybackEvent::DecodeDone]);
    assert_eq!(decoder.input_fill().0, 0);
}

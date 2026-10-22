//! Real Slimproto -> HTTP -> Decoder -> Output -> fake Device. Nothing is scripted in Playback.

use std::sync::Arc;
use std::time::{Duration, Instant};

use player::Player;
use tokio::sync::Notify;

use crate::support::device::{FakeDevice, Log};
use crate::support::lms::{Body, Rig, TIMEOUT};
use crate::support::signals;

const FIXTURE_FRAMES: usize = 22_050;

fn player(pace: Duration) -> (Arc<Player>, FakeDevice) {
    let device = FakeDevice::paced(pace);
    let player = Player::new(device.clone(), Duration::from_secs(10), Box::new(|_, _, _| {})).unwrap();
    (player, device)
}

async fn device_wait(device: &FakeDevice, condition: impl Fn(&Log) -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !device.inspect(&condition) {
        assert!(Instant::now() < deadline, "no expected output device state");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

async fn play_format(codec: u8, header: bool, bytes: Vec<u8>, frames: usize, constant: Option<f32>) {
    // Match a 441-frame write to a real 44.1 kHz device: a 10x-speed consumer can
    // outrun a legitimately scheduled decoder/EOF under concurrent CI load.
    let (playback, device) = player(Duration::from_millis(10));
    let (rig, mut lms) = Rig::new(playback, vec![bytes.into()]).await;
    lms.helo(false).await;
    if header {
        lms.start_wav(rig.http_port).await;
    } else {
        lms.start(codec, rig.http_port).await;
    }
    lms.wait(b"STMs").await;
    lms.wait(b"STMd").await;
    lms.wait(b"STMu").await;
    device_wait(&device, |log| log.samples.len() == frames * 2).await;
    device.inspect(|log| {
        assert_eq!(log.formats, vec![(44_100, 2)]);
        assert!(log.samples.iter().all(|s| s.is_finite()));
        assert!(log.samples.iter().any(|s| s.abs() > 0.01));
        if let Some(expected) = constant {
            assert!(log.samples.iter().all(|s| (*s - expected).abs() < f32::EPSILON));
        }
    });
    assert!(
        !lms.history.iter().any(|f| matches!(&f.code(), b"STMn" | b"STMo")),
        "codec {} header {header}: {:?}",
        char::from(codec),
        lms.history
            .iter()
            .map(crate::support::lms::Frame::code)
            .collect::<Vec<_>>()
    );
    lms.command(b'q').await;
    lms.wait(b"STMf").await;
}

#[tokio::test]
async fn flac_mp3_raw_pcm_and_wav_reach_the_device_with_exact_frame_counts() {
    play_format(b'p', false, signals::pcm(4410, 4096), 4410, Some(0.125)).await;
    play_format(b'p', true, signals::wav(4410, 8192), 4410, Some(0.25)).await;
    play_format(b'f', false, signals::FLAC.to_vec(), FIXTURE_FRAMES, None).await;
    play_format(b'm', false, signals::MP3.to_vec(), FIXTURE_FRAMES, None).await;
}

#[tokio::test]
async fn flac_to_mp3_is_gapless_without_reopening_or_intermediate_end() {
    let (playback, device) = player(Duration::from_millis(5));
    let (rig, mut lms) = Rig::new(
        playback,
        vec![signals::FLAC.to_vec().into(), signals::MP3.to_vec().into()],
    )
    .await;
    lms.helo(false).await;
    lms.start(b'f', rig.http_port).await;
    lms.wait(b"STMd").await;
    lms.start(b'm', rig.http_port).await;
    lms.wait(b"STMd").await;
    lms.wait(b"STMs").await;
    lms.wait(b"STMs").await;
    lms.wait(b"STMu").await;
    device_wait(&device, |log| log.samples.len() == FIXTURE_FRAMES * 4).await;
    assert_eq!(device.inspect(|log| log.formats.clone()), vec![(44_100, 2)]);
    assert!(
        device.inspect(|log| log
            .silence_at
            .iter()
            .all(|at| *at == 0 || *at == FIXTURE_FRAMES * 4)),
        "silence was inserted between decoded audio frames"
    );
    let codes: Vec<_> = lms.history.iter().map(crate::support::lms::Frame::code).collect();
    assert!(!codes.contains(b"STMo"), "output underrun: {codes:?}");
    let starts: Vec<_> = codes
        .iter()
        .enumerate()
        .filter(|(_, c)| *c == b"STMs")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(starts.len(), 2, "{codes:?}");
    assert!(
        codes.iter().position(|c| c == b"STMu").unwrap() > starts[1],
        "{codes:?}"
    );
    lms.command(b'q').await;
    lms.wait(b"STMf").await;
}

#[tokio::test]
async fn pause_flush_next_resume_and_stop_keep_exact_current_track_audio() {
    let (playback, device) = player(Duration::from_millis(5));
    let frames = 3 * 44_100;
    let (rig, mut lms) = Rig::new(
        playback,
        vec![
            signals::pcm(frames, 4096).into(),
            signals::FLAC.to_vec().into(),
            signals::pcm(4410, 8192).into(),
        ],
    )
    .await;
    lms.helo(false).await;
    lms.start(b'p', rig.http_port).await;
    lms.wait(b"STMs").await;
    lms.wait(b"STMd").await;
    lms.command(b'p').await;
    lms.wait(b"STMp").await;
    device_wait(&device, |log| log.silence_writes >= 3).await;
    let count = device.inspect(|log| log.samples.len());
    lms.command(b't').await;
    let elapsed = lms.wait(b"STMt").await.elapsed_ms();
    tokio::time::sleep(Duration::from_millis(150)).await;
    lms.command(b't').await;
    let paused_elapsed = lms.wait(b"STMt").await.elapsed_ms();
    // H15-93: a queued timer can precede the final 441-frame (10 ms at 44.1 kHz)
    // accounting update at pause. No new audio may reach the device (checked exactly below).
    assert!(
        paused_elapsed.abs_diff(elapsed) <= 10,
        "paused elapsed changed by more than one output chunk: {elapsed} -> {paused_elapsed}"
    );
    assert_eq!(device.inspect(|log| log.samples.len()), count);

    lms.start(b'f', rig.http_port).await;
    lms.wait(b"STMd").await;
    lms.command(b'f').await;
    lms.wait(b"STMf").await;
    lms.command(b'u').await;
    lms.wait(b"STMr").await;
    lms.wait(b"STMu").await;
    device_wait(&device, |log| log.samples.len() == frames * 2).await;
    assert!(device.inspect(|log| log.samples.iter().all(|s| (*s - 0.125).abs() < f32::EPSILON)));
    assert_eq!(lms.history.iter().filter(|f| f.code() == *b"STMs").count(), 1);

    lms.command(b'q').await;
    lms.wait(b"STMf").await;
    lms.start(b'p', rig.http_port).await;
    lms.wait(b"STMs").await;
    lms.wait(b"STMd").await;
    lms.wait(b"STMu").await;
    device_wait(&device, |log| log.samples.len() == (frames + 4410) * 2).await;
    lms.command(b'q').await;
    lms.wait(b"STMf").await;
}

#[tokio::test]
async fn control_reconnect_mid_track_preserves_decoder_http_and_exact_audio() {
    let (playback, device) = player(Duration::from_millis(5));
    let frames = 3 * 44_100;
    let bytes = signals::pcm(frames, 4096);
    let gate = Arc::new(Notify::new());
    let body = Body {
        bytes: bytes.clone(),
        gate: Some(Arc::clone(&gate)),
    };
    let (rig, mut lms) = Rig::new(playback, vec![body]).await;
    lms.helo(false).await;
    lms.start(b'p', rig.http_port).await;
    lms.wait(b"STMs").await;
    drop(lms);
    let mut lms = rig.accept().await;
    let helo = lms.helo(true).await;
    assert_eq!(
        u64::from_be_bytes(helo.payload[26..34].try_into().unwrap()),
        u64::try_from(bytes.len() / 2).unwrap()
    );
    gate.notify_one();
    lms.wait(b"STMd").await;
    lms.wait(b"STMu").await;
    device_wait(&device, |log| log.samples.len() == frames * 2).await;
    assert_eq!(device.inspect(|log| log.formats.clone()), vec![(44_100, 2)]);
    assert!(
        !lms.history
            .iter()
            .any(|f| matches!(&f.code(), b"STMn" | b"STMo" | b"STMs"))
    );
    lms.command(b'q').await;
    lms.wait(b"STMf").await;
}

#[test]
fn dropping_a_buffered_player_cancels_the_decoder_and_closes_output() {
    use bytes::Bytes;
    use slimproto::{Format, Playback as _, Track};
    use tokio::sync::mpsc;

    let device = FakeDevice::default();
    let playback = Player::new(device.clone(), Duration::from_millis(10), Box::new(|_, _, _| {})).unwrap();
    let (tx, rx) = mpsc::channel(1);
    tx.try_send(Bytes::from(signals::pcm(44_100, 4096))).unwrap();
    drop(tx);
    playback.start(
        Track {
            format: Format::Pcm {
                sample_rate: Some(44_100),
                sample_size: Some(16),
                channels: Some(2),
                big_endian: Some(false),
            },
            replay_gain: 1.0,
            autostart: false,
            output_threshold: Duration::from_secs(1),
            transition: slimproto::Transition::default(),
        },
        rx,
    );
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let status = playback.status();
        if status.output_fill == status.output_size {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "decoder never filled the output buffer"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let shutdown = std::thread::spawn(move || {
        drop(playback);
        done_tx.send(()).unwrap();
    });
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("dropping Player deadlocked on a decoder blocked in the full output buffer");
    shutdown.join().unwrap();
    device.wait(|log| log.closed);
}

#[tokio::test]
async fn real_played_frames_drive_growing_timers_and_stop_flushes_pending_audio() {
    let (playback, device) = player(Duration::from_millis(5));
    let (rig, mut lms) = Rig::new(playback, vec![signals::pcm(8 * 44_100, 4096).into()]).await;
    lms.helo(false).await;
    lms.start(b'p', rig.http_port).await;
    lms.wait(b"STMs").await;
    let first = lms.wait(b"STMt").await.elapsed_ms();
    let second = lms.wait(b"STMt").await.elapsed_ms();
    assert!(
        first > 0 && second > first,
        "non-growing audible position: {first} -> {second}"
    );
    lms.command(b'q').await;
    lms.wait(b"STMf").await;
    device_wait(&device, |log| log.silence_writes >= 3).await;
    let count = device.inspect(|log| log.samples.len());
    device_wait(&device, |log| log.silence_writes >= 6).await;
    assert_eq!(device.inspect(|log| log.samples.len()), count);
    assert!(count < 8 * 44_100 * 2, "stop did not discard the buffered tail");
}

#[tokio::test]
async fn native_dsp_processes_protocol_audio_and_live_balance_without_losing_the_tail() {
    use player::dsp::{Controls, DspDevice, Settings};
    let device = FakeDevice::paced(Duration::from_millis(10));
    let controls = Arc::new(Controls::new(Settings {
        balance: 1.0,
        ..Settings::default()
    }));
    let playback = Player::new(
        DspDevice::new(device.clone(), Some(controls.clone())),
        Duration::from_secs(10),
        Box::new(|_, _, _| {}),
    )
    .unwrap();
    let (rig, mut lms) = Rig::new(
        playback,
        vec![signals::pcm(4410, 4096).into(), signals::pcm(4410, 4096).into()],
    )
    .await;
    lms.helo(false).await;
    lms.start(b'p', rig.http_port).await;
    lms.wait(b"STMs").await;
    lms.wait(b"STMd").await;
    lms.wait(b"STMu").await;
    device_wait(&device, |log| {
        log.samples
            .iter()
            .filter(|s| (**s - 0.125).abs() < f32::EPSILON)
            .count()
            == 4410
    })
    .await;
    device.inspect(|log| {
        assert_eq!(log.formats, [(44_100, 2)]);
        assert!(log.samples.as_chunks::<2>().0.iter().all(|pair| pair[0] == 0.0));
    });
    controls.publish(Settings {
        balance: -1.0,
        ..Settings::default()
    });
    // The next track uses the same open output and reads the new snapshot between blocks.
    lms.start(b'p', rig.http_port).await;
    lms.wait(b"STMs").await;
    lms.wait(b"STMd").await;
    lms.wait(b"STMu").await;
    device_wait(&device, |log| {
        log.samples
            .as_chunks::<2>()
            .0
            .iter()
            .filter(|pair| (pair[0] - 0.125).abs() < f32::EPSILON)
            .count()
            == 4410
    })
    .await;
    assert_eq!(device.inspect(|log| log.formats.clone()), [(44_100, 2)]);
    lms.command(b'q').await;
    lms.wait(b"STMf").await;
}

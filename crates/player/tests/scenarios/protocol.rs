use std::sync::Arc;

use slimproto::{Format, PlaybackEvent};
use tokio::sync::Notify;

use crate::support::lms::{Body, Rig};
use crate::support::recording::{Call, Recording};
use crate::support::signals;

#[tokio::test]
async fn pcm_and_wav_stream_bytes_commands_and_timers_reach_playback() {
    let raw = signals::pcm(4410, 4096);
    let wav = signals::wav(4410, -4096);
    let playback = Arc::new(Recording::default());
    let (rig, mut lms) = Rig::new(playback.clone(), vec![raw.clone().into(), wav.clone().into()]).await;
    lms.helo(false).await;
    assert!(rig.state.borrow().connected);

    lms.start(b'p', rig.http_port).await;
    assert_eq!(playback.body(0, raw.len()).await, raw);
    playback.emit(PlaybackEvent::TrackStarted);
    lms.wait(b"STMs").await;
    let first = lms.wait(b"STMt").await.elapsed_ms();
    let second = lms.wait(b"STMt").await.elapsed_ms();
    assert!(second > first, "elapsed did not grow: {first} -> {second}");

    lms.command(b'p').await;
    lms.wait(b"STMp").await;
    lms.command(b'u').await;
    lms.wait(b"STMr").await;
    playback.emit(PlaybackEvent::DecodeDone);
    lms.wait(b"STMd").await;
    lms.start_wav(rig.http_port).await;
    assert_eq!(playback.body(1, wav.len()).await, wav);
    playback.emit(PlaybackEvent::TrackStarted);
    lms.wait(b"STMs").await;
    playback.emit(PlaybackEvent::DecodeDone);
    lms.wait(b"STMd").await;
    playback.emit(PlaybackEvent::PlaybackDone);
    lms.wait(b"STMu").await;
    lms.command(b'f').await;
    lms.wait(b"STMf").await;
    lms.command(b'q').await;
    lms.wait(b"STMf").await;

    let calls = playback.calls();
    assert!(calls.contains(&Call::Pause));
    assert!(calls.contains(&Call::Unpause));
    assert!(calls.contains(&Call::Flush));
    assert_eq!(calls.last(), Some(&Call::Stop));
    assert!(calls.iter().any(
        |call| matches!(call, Call::Start(track) if track.format == Format::Pcm {
            sample_rate: Some(44_100), sample_size: Some(16), channels: Some(2), big_endian: Some(false)
        })
    ));
}

#[tokio::test]
async fn reconnect_mid_stream_keeps_http_body_and_playback_alive() {
    let raw = signals::pcm(44_100, 4096);
    let gate = Arc::new(Notify::new());
    let playback = Arc::new(Recording::default());
    let body = Body {
        bytes: raw.clone(),
        gate: Some(Arc::clone(&gate)),
    };
    let (rig, mut lms) = Rig::new(playback.clone(), vec![body]).await;
    lms.helo(false).await;
    lms.start(b'p', rig.http_port).await;
    let half = playback.body(0, raw.len() / 2).await;
    assert_eq!(half, raw[..raw.len() / 2]);
    playback.emit(PlaybackEvent::TrackStarted);
    lms.wait(b"STMs").await;
    drop(lms);
    let mut reconnected = rig.accept().await;
    let helo = reconnected.helo(true).await;
    let received = u64::from_be_bytes(helo.payload[26..34].try_into().unwrap());
    assert_eq!(received, u64::try_from(half.len()).unwrap());
    gate.notify_one();
    assert_eq!(playback.body(0, raw.len()).await, raw);
    reconnected.wait(b"DSCO").await;
    playback.emit(PlaybackEvent::DecodeDone);
    reconnected.wait(b"STMd").await;
    playback.emit(PlaybackEvent::PlaybackDone);
    reconnected.wait(b"STMu").await;
    let calls = playback.calls();
    assert_eq!(calls.iter().filter(|c| matches!(c, Call::Start(_))).count(), 1);
    assert!(!calls.contains(&Call::Stop), "reconnect must not stop audio");
}

#[tokio::test]
async fn generated_flac_and_mp3_bodies_keep_their_codec_and_end_events() {
    let playback = Arc::new(Recording::default());
    let (rig, mut lms) = Rig::new(
        playback.clone(),
        vec![signals::FLAC.to_vec().into(), signals::MP3.to_vec().into()],
    )
    .await;
    lms.helo(false).await;
    for (index, (codec, expected, body)) in [
        (b'f', Format::Flac, signals::FLAC),
        (b'm', Format::Mp3, signals::MP3),
    ]
    .into_iter()
    .enumerate()
    {
        lms.start(codec, rig.http_port).await;
        assert_eq!(playback.body(index, body.len()).await, body);
        let calls = playback.calls();
        let last = calls.last().unwrap();
        assert!(matches!(last, Call::Start(track) if track.format == expected));
        playback.emit(PlaybackEvent::TrackStarted);
        lms.wait(b"STMs").await;
        playback.emit(PlaybackEvent::DecodeDone);
        lms.wait(b"STMd").await;
        playback.emit(PlaybackEvent::PlaybackDone);
        lms.wait(b"STMu").await;
    }
}

//! The player against a fake Lyrion server over local sockets (H15-55): HELO, the name query, two
//! tracks (`strm s → STMf STMc RESP … DSCO`, `STMs`, `STMd`, then the next `strm s` and `STMu`),
//! volume and the `strm t` echo. Playback is a fake that records calls and emits scripted events.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use slimproto::{Config, Playback, PlaybackEvent, PlaybackStatus, PlayerState, Track};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Start(Track),
    Volume(f32, f32, u32),
    Stop,
}

#[derive(Default)]
struct FakePlayback {
    calls: Mutex<Vec<Call>>,
    events: Mutex<Vec<PlaybackEvent>>,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakePlayback {
    fn emit(&self, event: PlaybackEvent) {
        self.events.lock().unwrap().push(event);
    }
}

impl Playback for FakePlayback {
    fn start(&self, track: Track, mut source: mpsc::Receiver<Bytes>) {
        self.calls.lock().unwrap().push(Call::Start(track));
        let bodies = Arc::clone(&self.bodies);
        let index = {
            let mut b = bodies.lock().unwrap();
            b.push(Vec::new());
            b.len() - 1
        };
        tokio::spawn(async move {
            while let Some(chunk) = source.recv().await {
                bodies.lock().unwrap()[index].extend_from_slice(&chunk);
            }
        });
    }
    fn pause(&self) {}
    fn unpause(&self, _at: Option<Instant>) {}
    fn stop(&self) {
        self.calls.lock().unwrap().push(Call::Stop);
    }
    fn flush_next(&self) {}
    fn skip(&self, _ahead: Duration) {}
    fn volume(&self, left: f32, right: f32, percent: u32) {
        self.calls
            .lock()
            .unwrap()
            .push(Call::Volume(left, right, percent));
    }
    fn enable(&self, _on: bool) {}
    fn status(&self) -> PlaybackStatus {
        PlaybackStatus {
            elapsed: Duration::from_millis(1234),
            output_size: 1000,
            output_fill: 500,
            events: std::mem::take(&mut *self.events.lock().unwrap()),
            ..Default::default()
        }
    }
}

/// The fake server's side of the slimproto connection.
struct Lms {
    socket: TcpStream,
}

impl Lms {
    /// Sends a server frame: u16 length (opcode included), opcode, body.
    async fn send(&mut self, opcode: &[u8; 4], body: &[u8]) {
        let mut frame = u16::try_from(4 + body.len()).unwrap().to_be_bytes().to_vec();
        frame.extend_from_slice(opcode);
        frame.extend_from_slice(body);
        self.socket.write_all(&frame).await.unwrap();
    }

    /// The next client frame: opcode and payload.
    async fn recv(&mut self) -> ([u8; 4], Vec<u8>) {
        let read = async {
            let mut header = [0u8; 8];
            self.socket.read_exact(&mut header).await.unwrap();
            let len = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
            let mut payload = vec![0; len];
            self.socket.read_exact(&mut payload).await.unwrap();
            (header[..4].try_into().unwrap(), payload)
        };
        tokio::time::timeout(Duration::from_secs(5), read)
            .await
            .expect("no frame from the player")
    }

    /// The next STAT event code, skipping heartbeats (`STMt`) unless `timers`.
    async fn stat(&mut self, timers: bool) -> ([u8; 4], Vec<u8>) {
        loop {
            let (op, p) = self.recv().await;
            if &op != b"STAT" {
                return (op, p);
            }
            if timers || &p[..4] != b"STMt" {
                return (p[..4].try_into().unwrap(), p);
            }
        }
    }
}

fn strm(command: u8, timestamp: [u8; 4], http_port: u16, request: &[u8]) -> Vec<u8> {
    let mut b = vec![
        command, b'1', b'f', b'?', b'?', b'?', b'?', 1, b'0', 0, b'0', 0, 10, 0,
    ];
    b.extend_from_slice(&timestamp);
    b.extend_from_slice(&http_port.to_be_bytes());
    b.extend_from_slice(&u32::from(Ipv4Addr::LOCALHOST).to_be_bytes());
    b.extend_from_slice(request);
    b
}

const REQUEST: &[u8] = b"GET /stream.flac?player=02:01:02:03:04:05 HTTP/1.0\r\n\r\n";

/// `strm s`, then what the player answers up to the end of the stream.
async fn start_track(lms: &mut Lms, http: u16) {
    lms.send(b"strm", &strm(b's', [0; 4], http, REQUEST)).await;
    assert_eq!(&lms.stat(false).await.0, b"STMf");
    assert_eq!(&lms.stat(false).await.0, b"STMc");
    assert_eq!(&lms.stat(false).await.0, b"RESP");
    assert_eq!(&lms.stat(false).await.0, b"DSCO");
}

/// An HTTP source that answers each connection with a FLAC-ish body, then closes.
async fn http_source(bodies: &'static [&'static [u8]]) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        for body in bodies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 256];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.0 200 OK\r\nContent-Type: audio/flac\r\n\r\n")
                .await
                .unwrap();
            socket.write_all(body).await.unwrap();
        }
    });
    port
}

#[tokio::test]
async fn plays_two_tracks_against_a_fake_server() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let SocketAddr::V4(lms_addr) = listener.local_addr().unwrap() else {
        unreachable!()
    };
    let http = http_source(&[b"fLaC-track-one", b"fLaC-track-two"]).await;
    let playback = Arc::new(FakePlayback::default());
    let (state_tx, state_rx) = watch::channel(PlayerState::default());
    let config = Config {
        name: "dock".into(),
        mac: [0x02, 1, 2, 3, 4, 5],
        server: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, lms_addr.port())),
        max_sample_rate: 192_000,
        firmware: "test".into(),
        codecs: Vec::new(),
    };
    let player = tokio::spawn(slimproto::run(
        config,
        Arc::clone(&playback) as Arc<dyn Playback>,
        state_tx,
    ));
    let (socket, _) = listener.accept().await.unwrap();
    let mut lms = Lms { socket };

    // HELO, then the name.
    let (op, helo) = lms.recv().await;
    assert_eq!(&op, b"HELO");
    assert_eq!(&helo[2..8], &[0x02, 1, 2, 3, 4, 5]);
    assert!(String::from_utf8_lossy(&helo[36..]).contains("Model=squeezelite"));
    lms.send(b"setd", &[0]).await;
    assert_eq!(lms.recv().await, (*b"SETD", b"\0dock\0".to_vec()));
    assert!(state_rx.borrow().connected);

    // Track one.
    lms.send(
        b"strm",
        &strm(
            b's',
            [0; 4],
            http,
            b"GET /stream.flac?player=02:01:02:03:04:05 HTTP/1.0\r\n\r\n",
        ),
    )
    .await;
    assert_eq!(&lms.stat(false).await.0, b"STMf");
    assert_eq!(&lms.stat(false).await.0, b"STMc");
    let (op, headers) = lms.stat(false).await;
    assert_eq!(&op, b"RESP");
    assert!(headers.starts_with(b"HTTP/1.0 200 OK"));
    let (op, reason) = lms.stat(false).await;
    assert_eq!((&op, reason.as_slice()), (b"DSCO", &[0u8][..]), "end of stream");
    playback.emit(PlaybackEvent::TrackStarted);
    let (code, stms) = lms.stat(false).await;
    assert_eq!(&code, b"STMs");
    assert_eq!(
        &stms[43..47],
        &1234u32.to_be_bytes(),
        "elapsed ms from the playback"
    );
    assert_eq!(
        &stms[15..23],
        &14u64.to_be_bytes(),
        "bytes received of this stream"
    );
    playback.emit(PlaybackEvent::DecodeDone);
    assert_eq!(&lms.stat(false).await.0, b"STMd");

    // strm t is echoed, timestamp bytes unchanged.
    lms.send(b"strm", &strm(b't', [9, 8, 7, 6], 0, b"")).await;
    let (code, echo) = lms.stat(true).await;
    assert_eq!(&code, b"STMt");
    assert_eq!(&echo[47..51], &[9, 8, 7, 6]);

    // Track two (gapless: no stop in between), then the end.
    start_track(&mut lms, http).await;
    playback.emit(PlaybackEvent::TrackStarted);
    assert_eq!(&lms.stat(false).await.0, b"STMs");
    playback.emit(PlaybackEvent::PlaybackDone);
    playback.emit(PlaybackEvent::PlaybackDone);
    assert_eq!(&lms.stat(false).await.0, b"STMu", "once, even if reported twice");

    // Volume: half gain left, unity right.
    let mut audg = vec![0u8; 8];
    audg.extend_from_slice(&[1, 255]);
    audg.extend_from_slice(&0x8000u32.to_be_bytes());
    audg.extend_from_slice(&0x1_0000u32.to_be_bytes());
    lms.send(b"audg", &audg).await;
    lms.send(b"strm", &strm(b'q', [0; 4], 0, b"")).await;
    assert_eq!(&lms.stat(false).await.0, b"STMf");

    let calls = playback.calls.lock().unwrap().clone();
    let starts: Vec<_> = calls
        .iter()
        .filter_map(|c| if let Call::Start(t) = c { Some(t) } else { None })
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0].format, slimproto::Format::Flac);
    assert_eq!(starts[0].output_threshold, Duration::from_secs(1));
    assert!(starts[0].autostart);
    assert!(calls.contains(&Call::Volume(0.5, 1.0, 100)));
    assert_eq!(calls.last(), Some(&Call::Stop));
    assert_eq!(
        *playback.bodies.lock().unwrap(),
        vec![b"fLaC-track-one".to_vec(), b"fLaC-track-two".to_vec()]
    );
    player.abort();
}

#[tokio::test]
async fn reconnects_with_the_reconnect_flag_and_takes_a_new_name() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (state_tx, mut state_rx) = watch::channel(PlayerState::default());
    let config = Config {
        name: "dock".into(),
        mac: [0x02, 0, 0, 0, 0, 1],
        server: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)),
        max_sample_rate: 48_000,
        firmware: "test".into(),
        codecs: Vec::new(),
    };
    let player = tokio::spawn(slimproto::run(
        config,
        Arc::new(FakePlayback::default()),
        state_tx,
    ));

    let (socket, _) = listener.accept().await.unwrap();
    let mut lms = Lms { socket };
    let (_, helo) = lms.recv().await;
    assert_eq!(&helo[24..26], &[0, 0], "first connect");
    lms.send(b"setd", b"\0Kitchen\0").await;
    assert_eq!(
        lms.recv().await,
        (*b"SETD", b"\0Kitchen\0".to_vec()),
        "a rename is confirmed"
    );
    state_rx.wait_for(|s| s.name == "Kitchen").await.unwrap();
    drop(lms);

    let (socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut lms = Lms { socket };
    let (_, helo) = lms.recv().await;
    assert_eq!(&helo[24..26], &[0x40, 0], "reconnect flag");
    lms.send(b"setd", &[0]).await;
    assert_eq!(
        lms.recv().await,
        (*b"SETD", b"\0Kitchen\0".to_vec()),
        "keeps the new name"
    );
    player.abort();
}

#[tokio::test]
async fn a_rename_from_our_side_goes_to_the_server() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (state_tx, mut state_rx) = watch::channel(PlayerState::default());
    let (names_tx, names_rx) = watch::channel("dock".to_owned());
    let config = Config {
        name: "dock".into(),
        mac: [0x02, 0, 0, 0, 0, 2],
        server: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)),
        max_sample_rate: 48_000,
        firmware: "test".into(),
        codecs: Vec::new(),
    };
    let player = tokio::spawn(slimproto::run_named(
        config,
        Arc::new(FakePlayback::default()),
        state_tx,
        names_rx,
    ));
    let (socket, _) = listener.accept().await.unwrap();
    let mut lms = Lms { socket };
    let _helo = lms.recv().await;
    names_tx.send("Office".into()).unwrap();
    loop {
        let (opcode, data) = lms.recv().await;
        if &opcode == b"SETD" {
            assert_eq!(data, b"\0Office\0".to_vec());
            break;
        }
    }
    state_rx.wait_for(|s| s.name == "Office").await.unwrap();
    player.abort();
}

//! A real loopback Slimproto connection and HTTP stream, with bounded waits and owned tasks.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use slimproto::{Config, Playback, PlayerState};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

pub const TIMEOUT: Duration = Duration::from_secs(5);
pub const MAC: [u8; 6] = [2, 0, 0, 0, 0, 58];

pub struct Body {
    pub bytes: Vec<u8>,
    pub gate: Option<Arc<Notify>>,
}

impl From<Vec<u8>> for Body {
    fn from(bytes: Vec<u8>) -> Self {
        Self { bytes, gate: None }
    }
}

pub struct Rig {
    listener: TcpListener,
    pub http_port: u16,
    pub state: watch::Receiver<PlayerState>,
    player: JoinHandle<()>,
    http: JoinHandle<()>,
}

impl Rig {
    pub async fn new(playback: Arc<dyn Playback>, bodies: Vec<Body>) -> (Self, Lms) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_port = http_listener.local_addr().unwrap().port();
        let http = tokio::spawn(serve_http(http_listener, bodies));
        let (state_tx, state) = watch::channel(PlayerState::default());
        let config = Config {
            name: "H15-58 isolated test".into(),
            mac: MAC,
            server: Some(SocketAddrV4::new(
                Ipv4Addr::LOCALHOST,
                listener.local_addr().unwrap().port(),
            )),
            max_sample_rate: 192_000,
            firmware: "test".into(),
            codecs: Vec::new(),
        };
        let player = tokio::spawn(slimproto::run(config, playback, state_tx));
        let rig = Self {
            listener,
            http_port,
            state,
            player,
            http,
        };
        let lms = rig.accept().await;
        (rig, lms)
    }

    pub async fn accept(&self) -> Lms {
        let (socket, _) = tokio::time::timeout(TIMEOUT, self.listener.accept())
            .await
            .unwrap()
            .unwrap();
        socket.set_nodelay(true).unwrap();
        Lms {
            socket,
            pending: VecDeque::new(),
            history: Vec::new(),
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.player.abort();
        self.http.abort();
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub opcode: [u8; 4],
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn code(&self) -> [u8; 4] {
        if self.opcode == *b"STAT" {
            self.payload[..4].try_into().unwrap()
        } else {
            self.opcode
        }
    }

    pub fn elapsed_ms(&self) -> u32 {
        assert_eq!(self.opcode, *b"STAT");
        u32::from_be_bytes(self.payload[43..47].try_into().unwrap())
    }
}

pub struct Lms {
    socket: TcpStream,
    pending: VecDeque<Frame>,
    pub history: Vec<Frame>,
}

impl Lms {
    pub async fn send(&mut self, opcode: &[u8; 4], body: &[u8]) {
        let mut frame = u16::try_from(4 + body.len()).unwrap().to_be_bytes().to_vec();
        frame.extend_from_slice(opcode);
        frame.extend_from_slice(body);
        self.socket.write_all(&frame).await.unwrap();
    }

    pub async fn wait(&mut self, code: &[u8; 4]) -> Frame {
        tokio::time::timeout(TIMEOUT, async {
            loop {
                if let Some(index) = self.pending.iter().position(|f| f.code() == *code) {
                    return self.pending.remove(index).unwrap();
                }
                let frame = self.recv().await;
                self.history.push(frame.clone());
                self.pending.push_back(frame);
            }
        })
        .await
        .unwrap_or_else(|_| panic!("missing {code:?}; received {:?}", self.history))
    }

    async fn recv(&mut self) -> Frame {
        let mut header = [0; 8];
        self.socket.read_exact(&mut header).await.unwrap();
        let len = u32::from_be_bytes(header[4..].try_into().unwrap());
        assert!(len < 1024 * 1024, "unexpectedly large client frame");
        let mut payload = vec![0; usize::try_from(len).unwrap()];
        self.socket.read_exact(&mut payload).await.unwrap();
        Frame {
            opcode: header[..4].try_into().unwrap(),
            payload,
        }
    }

    pub async fn helo(&mut self, reconnect: bool) -> Frame {
        let frame = self.wait(b"HELO").await;
        assert_eq!(&frame.payload[2..8], &MAC);
        assert_eq!(
            &frame.payload[24..26],
            if reconnect { &[0x40, 0] } else { &[0, 0] }
        );
        let caps = String::from_utf8_lossy(&frame.payload[36..]);
        assert!(caps.contains("flc,mp3,pcm"), "{caps}");
        frame
    }

    pub async fn command(&mut self, command: u8) {
        self.send(b"strm", &strm(command, b'p', 0)).await;
    }

    pub async fn start(&mut self, format: u8, http_port: u16) {
        self.start_packet(&strm(b's', format, http_port)).await;
    }

    pub async fn start_wav(&mut self, http_port: u16) {
        let mut packet = strm(b's', b'p', http_port);
        packet[3..7].fill(b'?');
        self.start_packet(&packet).await;
    }

    async fn start_packet(&mut self, packet: &[u8]) {
        self.send(b"strm", packet).await;
        self.wait(b"STMf").await;
        self.wait(b"STMc").await;
        let response = self.wait(b"RESP").await;
        assert!(response.payload.starts_with(b"HTTP/1.0 200 OK"));
    }
}

fn strm(command: u8, format: u8, port: u16) -> Vec<u8> {
    // 16-bit, 44.1 kHz, stereo, little-endian; autostart, threshold 0.1 s.
    let mut body = vec![
        command, b'1', format, b'1', b'3', b'2', b'1', 1, b'0', 0, b'0', 0, 1, 0,
    ];
    body.extend_from_slice(&[0; 4]);
    body.extend_from_slice(&port.to_be_bytes());
    body.extend_from_slice(&u32::from(Ipv4Addr::LOCALHOST).to_be_bytes());
    body.extend_from_slice(b"GET /generated HTTP/1.0\r\n\r\n");
    body
}

async fn serve_http(listener: TcpListener, bodies: Vec<Body>) {
    for body in bodies {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            request.push(socket.read_u8().await.unwrap());
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
            assert!(request.len() < 4096);
        }
        assert_eq!(request, b"GET /generated HTTP/1.0\r\n\r\n");
        socket
            .write_all(b"HTTP/1.0 200 OK\r\nContent-Type: application/octet-stream\r\n\r\n")
            .await
            .unwrap();
        let split = body.bytes.len() / 2;
        socket.write_all(&body.bytes[..split]).await.unwrap();
        if let Some(gate) = body.gate {
            gate.notified().await;
        }
        // Split sample/header boundaries so the decoder cannot assume one receive per frame.
        for chunk in body.bytes[split..].chunks(173) {
            if socket.write_all(chunk).await.is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    }
}

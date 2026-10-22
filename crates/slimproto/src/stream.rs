//! The audio stream of one track (`docs/slimproto.md` §6, "The HTTP fetch"): connect to the source
//! `strm s` names, send its HTTP request verbatim, hand the response headers to the server (`RESP`)
//! and the body to the decoder, and report how the connection ended (`DSCO`).
//!
//! Direct streams (autostart 2/3) wait for the server's `cont` before the body flows; it carries the
//! ICY metadata interval, and the metadata blocks are cut out of the audio (and sent as `META`).

use std::net::SocketAddrV4;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

use crate::messages::Disconnect;

/// Connecting to the source gives up after this long (squeezelite: 10 s).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Response headers longer than this end the stream (`DSCO 1`).
const MAX_HEADERS: usize = 16 * 1024;
const READ_CHUNK: usize = 16 * 1024;

/// What a stream reports to the connection task; `id` tells this stream from later ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// The response headers, for `RESP`.
    Headers { id: u64, headers: Bytes },
    /// An ICY metadata block, for `META`.
    Metadata { id: u64, block: Bytes },
    /// The connection ended, for `DSCO`; not sent when we closed it ourselves.
    Ended { id: u64, reason: Disconnect },
}

/// A running stream; dropping it closes the connection without a report.
#[derive(Debug)]
pub struct Stream {
    pub id: u64,
    task: tokio::task::JoinHandle<()>,
    /// The `cont` that releases a direct stream's body (with its ICY interval).
    cont: Option<oneshot::Sender<u32>>,
}

impl Stream {
    /// Opens a stream in the background. `bytes_received` counts the body for `STAT`;
    /// `wait_for_cont` holds the body until [`Stream::cont`] (direct streams).
    #[must_use]
    pub fn open(
        id: u64,
        source: SocketAddrV4,
        request: Bytes,
        wait_for_cont: bool,
        body: mpsc::Sender<Bytes>,
        events: mpsc::Sender<StreamEvent>,
        bytes_received: Arc<AtomicU64>,
    ) -> Self {
        let (cont_tx, cont_rx) = oneshot::channel();
        let fetch = Fetch {
            id,
            events,
            body,
            bytes_received,
        };
        let cont = wait_for_cont.then_some(cont_rx);
        let task = tokio::spawn(async move {
            if let Some(reason) = fetch.run(source, &request, cont).await {
                let _ = fetch.events.send(StreamEvent::Ended { id, reason }).await;
            }
        });
        Self {
            id,
            task,
            cont: wait_for_cont.then_some(cont_tx),
        }
    }

    /// The server's `cont`: let the body flow, cutting ICY metadata every `metaint` bytes (0: none).
    pub fn cont(&mut self, metaint: u32) {
        if let Some(tx) = self.cont.take() {
            let _ = tx.send(metaint);
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Fetch {
    id: u64,
    events: mpsc::Sender<StreamEvent>,
    body: mpsc::Sender<Bytes>,
    bytes_received: Arc<AtomicU64>,
}

impl Fetch {
    /// Runs the stream; returns why it ended, or `None` when the decoder went away (we stopped).
    async fn run(
        &self,
        source: SocketAddrV4,
        request: &[u8],
        cont: Option<oneshot::Receiver<u32>>,
    ) -> Option<Disconnect> {
        let Ok(Ok(mut socket)) = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(source)).await
        else {
            return Some(Disconnect::Unreachable);
        };
        if socket.write_all(request).await.is_err() {
            return Some(Disconnect::LocalError);
        }
        let (headers, rest) = match read_headers(&mut socket).await {
            Ok(h) => h,
            Err(reason) => return Some(reason),
        };
        let _ = self
            .events
            .send(StreamEvent::Headers { id: self.id, headers })
            .await;
        let metaint = match cont {
            Some(rx) => rx.await.ok()?,
            None => 0,
        };
        let mut icy = Icy::new(metaint);
        self.forward(&mut icy, rest).await?;
        let mut buf = BytesMut::with_capacity(READ_CHUNK);
        loop {
            buf.reserve(READ_CHUNK);
            match socket.read_buf(&mut buf).await {
                Ok(0) => return Some(Disconnect::Closed),
                Ok(_) => self.forward(&mut icy, buf.split().freeze()).await?,
                Err(_) => return Some(Disconnect::RemoteReset),
            }
        }
    }

    /// Counts and passes on body bytes; `None` when the decoder is gone.
    async fn forward(&self, icy: &mut Icy, data: Bytes) -> Option<()> {
        if data.is_empty() {
            return Some(());
        }
        self.bytes_received
            .fetch_add(data.len() as u64, Ordering::Relaxed);
        let (audio, metadata) = icy.split(data);
        for block in metadata {
            let _ = self
                .events
                .send(StreamEvent::Metadata { id: self.id, block })
                .await;
        }
        for chunk in audio {
            self.body.send(chunk).await.ok()?;
        }
        Some(())
    }
}

/// Reads up to and including the empty line; returns the headers and the body bytes read with them.
async fn read_headers(socket: &mut TcpStream) -> Result<(Bytes, Bytes), Disconnect> {
    let mut buf = BytesMut::with_capacity(4096);
    loop {
        if let Some(end) = header_end(&buf) {
            let mut headers = buf;
            let rest = headers.split_off(end);
            return Ok((headers.freeze(), rest.freeze()));
        }
        if buf.len() > MAX_HEADERS {
            return Err(Disconnect::LocalError);
        }
        match socket.read_buf(&mut buf).await {
            Ok(0) => return Err(Disconnect::Closed),
            Ok(_) => {}
            Err(_) => return Err(Disconnect::RemoteReset),
        }
    }
}

/// The offset just past the `\r\n\r\n` (or `\n\n`) that ends the headers.
fn header_end(buf: &[u8]) -> Option<usize> {
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2);
    match (crlf, lf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Cuts ICY metadata out of a stream: after every `metaint` audio bytes comes one length byte
/// (×16) and that much metadata.
#[derive(Debug)]
struct Icy {
    metaint: usize,
    /// Audio bytes left before the next length byte.
    audio_left: usize,
    /// Metadata bytes still to collect for the current block.
    meta_left: usize,
    meta: BytesMut,
}

impl Icy {
    fn new(metaint: u32) -> Self {
        let metaint = usize::try_from(metaint).unwrap_or(0);
        Self {
            metaint,
            audio_left: metaint,
            meta_left: 0,
            meta: BytesMut::new(),
        }
    }

    /// Splits `data` into audio pieces and complete, non-empty metadata blocks.
    fn split(&mut self, mut data: Bytes) -> (Vec<Bytes>, Vec<Bytes>) {
        if self.metaint == 0 {
            return (vec![data], Vec::new());
        }
        let (mut audio, mut blocks) = (Vec::new(), Vec::new());
        while !data.is_empty() {
            if self.meta_left > 0 {
                let n = self.meta_left.min(data.len());
                self.meta.extend_from_slice(&data.split_to(n));
                self.meta_left -= n;
                if self.meta_left == 0 {
                    self.audio_left = self.metaint;
                    let block = self.meta.split().freeze();
                    if block.iter().any(|&b| b != 0) {
                        blocks.push(block);
                    }
                }
            } else if self.audio_left > 0 {
                let n = self.audio_left.min(data.len());
                audio.push(data.split_to(n));
                self.audio_left -= n;
            } else {
                let len = usize::from(data.split_to(1)[0]) * 16;
                if len == 0 {
                    self.audio_left = self.metaint;
                } else {
                    self.meta_left = len;
                }
            }
        }
        (audio, blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn finds_the_end_of_the_headers() {
        assert_eq!(header_end(b"HTTP/1.0 200 OK\r\n\r\nbody"), Some(19));
        assert_eq!(header_end(b"ICY 200 OK\n\nbody"), Some(12));
        assert_eq!(header_end(b"HTTP/1.0 200 OK\r\nContent-Type: au"), None);
    }

    #[test]
    fn cuts_icy_metadata_across_chunks() {
        let mut icy = Icy::new(4);
        // 4 audio bytes, length 1 (16 bytes of metadata), 4 audio, length 0, 2 audio.
        let mut stream = b"abcd\x01StreamTitle='x';\0\0".to_vec();
        stream.truncate(4 + 1 + 16);
        stream.extend_from_slice(b"efgh\0ij");
        let (mut audio, mut meta) = (Vec::new(), Vec::new());
        for chunk in stream.chunks(3) {
            let (a, m) = icy.split(Bytes::copy_from_slice(chunk));
            audio.extend(a.iter().flat_map(|b| b.to_vec()));
            meta.extend(m);
        }
        assert_eq!(audio, b"abcdefghij");
        assert_eq!(meta.len(), 1);
        assert!(meta[0].starts_with(b"StreamTitle='x';"));
    }

    async fn serve(response: &'static [u8]) -> (SocketAddrV4, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let std::net::SocketAddr::V4(addr) = listener.local_addr().unwrap() else {
            unreachable!()
        };
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 64];
            let n = socket.read(&mut request).await.unwrap();
            request.truncate(n);
            socket.write_all(response).await.unwrap();
            request
        });
        (addr, server)
    }

    #[tokio::test]
    async fn sends_the_request_verbatim_and_reports_headers_body_and_eof() {
        let (addr, server) = serve(b"HTTP/1.0 200 OK\r\nContent-Type: audio/flac\r\n\r\nfLaC1234").await;
        let (body_tx, mut body_rx) = mpsc::channel(8);
        let (ev_tx, mut ev_rx) = mpsc::channel(8);
        let received = Arc::new(AtomicU64::new(0));
        let _stream = Stream::open(
            7,
            addr,
            Bytes::from_static(b"GET /x HTTP/1.0\r\n\r\n"),
            false,
            body_tx,
            ev_tx,
            Arc::clone(&received),
        );
        assert_eq!(server.await.unwrap(), b"GET /x HTTP/1.0\r\n\r\n");
        let Some(StreamEvent::Headers { id: 7, headers }) = ev_rx.recv().await else {
            panic!("no headers")
        };
        assert!(headers.ends_with(b"audio/flac\r\n\r\n"));
        let mut body = Vec::new();
        while let Some(chunk) = body_rx.recv().await {
            body.extend_from_slice(&chunk);
        }
        assert_eq!(body, b"fLaC1234");
        assert_eq!(
            ev_rx.recv().await,
            Some(StreamEvent::Ended {
                id: 7,
                reason: Disconnect::Closed
            })
        );
        assert_eq!(received.load(Ordering::Relaxed), 8);
    }

    #[tokio::test]
    async fn an_unreachable_source_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let std::net::SocketAddr::V4(addr) = listener.local_addr().unwrap() else {
            unreachable!()
        };
        drop(listener);
        let (body_tx, _body_rx) = mpsc::channel(8);
        let (ev_tx, mut ev_rx) = mpsc::channel(8);
        let _stream = Stream::open(1, addr, Bytes::new(), false, body_tx, ev_tx, Arc::default());
        assert_eq!(
            ev_rx.recv().await,
            Some(StreamEvent::Ended {
                id: 1,
                reason: Disconnect::Unreachable
            })
        );
    }

    #[tokio::test]
    async fn a_direct_stream_waits_for_cont() {
        let (addr, _server) = serve(b"ICY 200 OK\r\nicy-metaint: 2\r\n\r\nab\0cd").await;
        let (body_tx, mut body_rx) = mpsc::channel(8);
        let (ev_tx, mut ev_rx) = mpsc::channel(8);
        let mut stream = Stream::open(
            2,
            addr,
            Bytes::from_static(b"GET / HTTP/1.0\r\n\r\n"),
            true,
            body_tx,
            ev_tx,
            Arc::default(),
        );
        assert!(matches!(ev_rx.recv().await, Some(StreamEvent::Headers { .. })));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), body_rx.recv())
                .await
                .is_err(),
            "no body before cont"
        );
        stream.cont(2);
        let mut body = Vec::new();
        while let Some(chunk) = body_rx.recv().await {
            body.extend_from_slice(&chunk);
        }
        assert_eq!(body, b"abcd", "the empty metadata block is cut out");
    }
}

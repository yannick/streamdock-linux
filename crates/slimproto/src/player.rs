//! The player's connection to the server (`docs/slimproto.md` §2-§6): find the server, connect,
//! HELO, then act on the server's commands and report `STAT` every 100 ms. Reconnects keep the
//! audio going: the stream and the [`Playback`] survive a lost connection, and the next HELO says
//! it is a reconnect with the bytes received so far.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

use crate::discovery;
use crate::messages::{
    ClientMessage, Helo, PORT, ServerMessage, Stat, StatEvent, Strm, StrmCommand, pcm_rate, pcm_sample_size,
};
use crate::playback::{Format, Playback, PlaybackEvent, PlaybackStatus, Track, Transition, TransitionKind};
use crate::stream::{Stream, StreamEvent};

/// The server must send something this often (it polls with `strm t` every ~5 s).
const SILENCE_TIMEOUT: Duration = Duration::from_secs(35);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const STATUS_EVERY: Duration = Duration::from_millis(100);
/// While playing, `STMt` at least this often.
const TIMER_EVERY: Duration = Duration::from_secs(1);
const RETRY_AFTER: Duration = Duration::from_secs(5);
/// Failed connects before looking for another server (without a configured one).
const REDISCOVER_AFTER: u32 = 5;
/// An HTTP request in `strm s` longer than this is refused (squeezelite: 4095).
const MAX_REQUEST: usize = 4095;

/// How the player presents itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The name in Lyrion; the server may rename it (see [`PlayerState::name`]).
    pub name: String,
    /// The player id in Lyrion: stable and unique (see [`mac_from_seed`]).
    pub mac: [u8; 6],
    /// The server, or `None` to find one by discovery.
    pub server: Option<SocketAddrV4>,
    /// Highest sample rate the output takes; Lyrion resamples above it.
    pub max_sample_rate: u32,
    /// e.g. "0.1.0".
    pub firmware: String,
    /// The formats Lyrion may send, in preference order; it transcodes everything else to the
    /// first it can. Empty = all of [`Codec::ALL`].
    pub codecs: Vec<Codec>,
}

/// A format the player decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    Flac,
    Mp3,
    Pcm,
}

impl Codec {
    pub const ALL: [Self; 3] = [Self::Flac, Self::Mp3, Self::Pcm];

    /// The name in HELO's capabilities.
    #[must_use]
    pub fn helo_name(self) -> &'static str {
        match self {
            Self::Flac => "flc",
            Self::Mp3 => "mp3",
            Self::Pcm => "pcm",
        }
    }

    /// "flac", "mp3" or "pcm", as in the config.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
            Self::Pcm => "pcm",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }
}

impl Config {
    /// HELO's capabilities: squeezelite's model (LMS's full feature set for it), our codecs in
    /// preference order.
    #[must_use]
    pub fn capabilities(&self) -> String {
        let codecs: Vec<&str> = if self.codecs.is_empty() {
            Codec::ALL.iter().map(|c| c.helo_name()).collect()
        } else {
            self.codecs.iter().map(|c| c.helo_name()).collect()
        };
        format!(
            "Model=squeezelite,ModelName=StreamDock,AccuratePlayPoints=1,HasDigitalOut=1,Balance=1,Firmware={},MaxSampleRate={},{}",
            self.firmware,
            self.max_sample_rate,
            codecs.join(",")
        )
    }
}

/// A locally administered unicast MAC derived from `seed` (e.g. the machine id), so it is stable
/// and never collides with a real network card or a squeezelite on the same host.
#[must_use]
pub fn mac_from_seed(seed: &[u8]) -> [u8; 6] {
    // FNV-1a, 64 bit: stable across versions and platforms.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in seed {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let b = hash.to_be_bytes();
    [(b[0] & 0xfc) | 0x02, b[1], b[2], b[3], b[4], b[5]]
}

/// What the UI shows about the player.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerState {
    /// The server we are connected to.
    pub server: Option<SocketAddrV4>,
    pub connected: bool,
    /// The current name (changes when renamed in Lyrion: persist it).
    pub name: String,
    /// A track is playing (started and not finished).
    pub playing: bool,
    pub paused: bool,
    /// The last problem, e.g. "no Lyrion server found".
    pub problem: String,
    /// The playing track's format, e.g. "FLAC" or "PCM 44.1 kHz 16 bit"; empty when stopped.
    pub format: String,
    /// Output and decoder buffer fill in percent, in steps of 10 (so the state changes rarely).
    pub output_fill: u8,
    pub decode_fill: u8,
    /// Output underruns since the player started.
    pub underruns: u32,
    /// The playing audio's sample rate as decoded (e.g. 48000 when Lyrion resampled); 0 if unknown.
    pub sample_rate: u32,
}

/// `filled` of `size` in percent, rounded down to a step of 10.
fn fill_step(filled: u32, size: u32) -> u8 {
    if size == 0 {
        return 0;
    }
    let percent = (u64::from(filled.min(size)) * 100 / u64::from(size)) / 10 * 10;
    u8::try_from(percent).unwrap_or(100)
}

/// "FLAC", "MP3", "PCM 44.1 kHz 16 bit".
fn format_label(format: &Format) -> String {
    match format {
        Format::Flac => "FLAC".to_owned(),
        Format::Mp3 => "MP3".to_owned(),
        Format::Pcm {
            sample_rate,
            sample_size,
            ..
        } => {
            let rate = sample_rate.map(|r| format!(" {} kHz", f64::from(r) / 1000.0));
            let bits = sample_size.map(|b| format!(" {b} bit"));
            format!("PCM{}{}", rate.unwrap_or_default(), bits.unwrap_or_default())
        }
    }
}

/// Monotonic milliseconds for `jiffies` (wraps at 2³²).
fn jiffies() -> u32 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "jiffies wrap at 2^32 by definition"
    )]
    let ms = START.elapsed().as_millis() as u32;
    ms
}

/// Runs the player until the task is dropped.
pub async fn run(config: Config, playback: Arc<dyn Playback>, state: watch::Sender<PlayerState>) {
    let mut player = Player::new(config, playback, state, None);
    player.run().await;
}

/// [`run`], renaming the player whenever `names` changes: the new name goes to the server at once
/// (`setd` 0), which renames it in Lyrion.
pub async fn run_named(
    config: Config,
    playback: Arc<dyn Playback>,
    state: watch::Sender<PlayerState>,
    names: watch::Receiver<String>,
) {
    let mut player = Player::new(config, playback, state, Some(names));
    player.run().await;
}

/// The next name from `names`; pending forever once there is none or its sender is gone.
async fn next_name(names: &mut Option<watch::Receiver<String>>) -> String {
    if let Some(rx) = names {
        if rx.changed().await.is_ok() {
            return rx.borrow_and_update().clone();
        }
        *names = None;
    }
    std::future::pending().await
}

struct Player {
    config: Config,
    playback: Arc<dyn Playback>,
    state: watch::Sender<PlayerState>,
    /// Survives reconnects: the audio keeps playing while we reconnect.
    track: TrackState,
    stream_events: (mpsc::Sender<StreamEvent>, mpsc::Receiver<StreamEvent>),
    reconnect: bool,
    /// The last status the cycle read (without its events) and when: replies to commands use it,
    /// since `Playback::status` is only called from the cycle.
    last_status: (PlaybackStatus, Instant),
    /// Renames from our side (see [`run_named`]).
    names: Option<watch::Receiver<String>>,
}

/// The current track and stream, across connections.
#[derive(Debug, Default)]
struct TrackState {
    stream: Option<Stream>,
    next_stream_id: u64,
    bytes_received: Arc<AtomicU64>,
    /// A track started (`strm s`) whose first sample hasn't played yet, and its format label.
    pending: bool,
    pending_format: String,
    playing: bool,
    paused: bool,
    sent: SentOnce,
    last_timer: Option<Instant>,
}

/// `STMo`, `STMu`, `STMl` go out once per track.
#[derive(Debug, Default, Clone, Copy)]
struct SentOnce {
    output_underrun: bool,
    underrun: bool,
    loaded: bool,
}

/// How a connection ended.
enum Ended {
    /// Lost or timed out: reconnect to the same server.
    Lost(String),
    /// `serv`: connect to this server instead.
    Switch(Ipv4Addr),
}

impl Player {
    fn new(
        config: Config,
        playback: Arc<dyn Playback>,
        state: watch::Sender<PlayerState>,
        names: Option<watch::Receiver<String>>,
    ) -> Self {
        state.send_modify(|s| s.name.clone_from(&config.name));
        Self {
            config,
            playback,
            state,
            track: TrackState::default(),
            stream_events: mpsc::channel(32),
            reconnect: false,
            last_status: (PlaybackStatus::default(), Instant::now()),
            names,
        }
    }

    async fn run(&mut self) {
        // A server we were sent to (`serv`) or reconnect to; else the configured one or discovery.
        let mut server = None;
        let mut failures = 0u32;
        loop {
            let target = match server.or(self.config.server) {
                Some(s) => s,
                None => self.find_server().await,
            };
            if let Ok(Ok(socket)) = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(target)).await {
                failures = 0;
                server = Some(self.connected(socket, target).await);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            failures += 1;
            self.set_state(|s| s.problem = format!("cannot connect to Lyrion at {target}"));
            // Rediscover after a few failures; a `serv` target that doesn't answer falls back too.
            if failures > REDISCOVER_AFTER || self.config.server.is_some() {
                server = None;
            }
            tokio::time::sleep(RETRY_AFTER).await;
        }
    }

    /// Runs one connection; returns where to connect next.
    async fn connected(&mut self, socket: TcpStream, target: SocketAddrV4) -> SocketAddrV4 {
        let _ = socket.set_nodelay(true);
        self.set_state(|s| {
            s.server = Some(target);
            s.connected = true;
            s.problem.clear();
        });
        tracing::info!(server = %target, "connected to Lyrion");
        let ended = self.session(socket).await;
        self.set_state(|s| s.connected = false);
        self.reconnect = true;
        match ended {
            Ended::Switch(ip) => SocketAddrV4::new(ip, PORT),
            Ended::Lost(why) => {
                tracing::warn!(server = %target, why, "lost the connection to Lyrion; reconnecting");
                target
            }
        }
    }

    /// Discovery until a server answers.
    async fn find_server(&self) -> SocketAddrV4 {
        loop {
            match discovery::discover(Duration::from_secs(5)).await {
                Ok(servers) if !servers.is_empty() => {
                    tracing::info!(server = %servers[0].addr, name = servers[0].name, "found Lyrion");
                    return servers[0].addr;
                }
                Ok(_) => self.set_state(|s| s.problem = "no Lyrion server found on the network".into()),
                Err(e) => self.set_state(|s| s.problem = format!("discovery failed: {e}")),
            }
            tokio::time::sleep(RETRY_AFTER).await;
        }
    }

    fn set_state(&self, change: impl FnOnce(&mut PlayerState)) {
        self.state.send_modify(change);
    }

    /// One connection: HELO, then commands, stream events and the status cycle until it ends.
    async fn session(&mut self, socket: TcpStream) -> Ended {
        let server_ip = match socket.peer_addr() {
            Ok(std::net::SocketAddr::V4(a)) => *a.ip(),
            _ => Ipv4Addr::UNSPECIFIED,
        };
        let (mut reader, mut writer) = socket.into_split();
        let mut conn = Conn {
            writer: &mut writer,
            server_ip,
        };
        let helo = ClientMessage::Helo(Helo {
            mac: self.config.mac,
            reconnect: self.reconnect,
            bytes_received: self.track.bytes_received.load(Ordering::Relaxed),
            capabilities: self.config.capabilities(),
        });
        if let Err(e) = conn.send(&helo).await {
            return Ended::Lost(e.to_string());
        }
        let mut ticker = tokio::time::interval(STATUS_EVERY);
        loop {
            tokio::select! {
                frame = tokio::time::timeout(SILENCE_TIMEOUT, read_frame(&mut reader)) => {
                    let frame = match frame {
                        Ok(Ok(f)) => f,
                        Ok(Err(e)) => return Ended::Lost(e.to_string()),
                        Err(_) => return Ended::Lost("no data from the server for 35 s".into()),
                    };
                    match self.on_frame(&mut conn, &frame).await {
                        Ok(None) => {}
                        Ok(Some(ended)) => return ended,
                        Err(e) => return Ended::Lost(e.to_string()),
                    }
                }
                Some(event) = self.stream_events.1.recv() => {
                    if let Err(e) = self.on_stream_event(&mut conn, event).await {
                        return Ended::Lost(e.to_string());
                    }
                }
                _ = ticker.tick() => {
                    if let Err(e) = self.status_cycle(&mut conn).await {
                        return Ended::Lost(e.to_string());
                    }
                }
                name = next_name(&mut self.names) => {
                    if let Err(e) = self.rename(&mut conn, name).await {
                        return Ended::Lost(e.to_string());
                    }
                }
            }
        }
    }

    async fn on_frame(&mut self, conn: &mut Conn<'_>, frame: &[u8]) -> std::io::Result<Option<Ended>> {
        let message = match ServerMessage::parse(frame) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(error = %e, "ignoring a bad frame from Lyrion");
                return Ok(None);
            }
        };
        tracing::debug!(?message, "from Lyrion");
        match message {
            ServerMessage::Strm(strm) => self.on_strm(conn, strm).await?,
            ServerMessage::Audg { adjust, left, right } => self.on_volume(adjust, left, right),
            ServerMessage::Aude { spdif, .. } => self.playback.enable(spdif),
            ServerMessage::Setd { id: 0, data } => self.on_name(conn, data).await?,
            ServerMessage::Serv { ip, .. } => return Ok(Some(Ended::Switch(ip))),
            ServerMessage::Cont { metaint, .. } => {
                if let Some(stream) = self.track.stream.as_mut() {
                    stream.cont(metaint);
                }
            }
            ServerMessage::Setd { .. } | ServerMessage::Codc(_) | ServerMessage::Other(_) => {}
        }
        Ok(None)
    }

    async fn on_strm(&mut self, conn: &mut Conn<'_>, strm: Strm) -> std::io::Result<()> {
        match strm.command {
            StrmCommand::Start => self.start(conn, strm).await,
            StrmCommand::Status => conn.stat(self.stat(StatEvent::Timer, strm.gain_or_time)).await,
            StrmCommand::Stop => {
                self.playback.stop();
                self.set_state(|s| s.format.clear());
                self.track.stream = None;
                self.track.pending = false;
                self.track.playing = false;
                self.track.paused = false;
                self.sync_state();
                conn.stat(self.stat(StatEvent::Flushed, [0; 4])).await
            }
            StrmCommand::Flush => {
                self.playback.flush_next();
                let flushed = self.track.stream.take().is_some() || self.track.pending;
                self.track.pending = false;
                if flushed {
                    conn.stat(self.stat(StatEvent::Flushed, [0; 4])).await?;
                }
                Ok(())
            }
            StrmCommand::Pause => self.pause(conn, strm.value()).await,
            StrmCommand::Unpause => self.unpause(conn, strm.value()).await,
            StrmCommand::Skip => {
                self.playback.skip(Duration::from_millis(u64::from(strm.value())));
                Ok(())
            }
            StrmCommand::Other(c) => {
                tracing::debug!(command = %char::from(c), "ignoring an unknown strm command");
                Ok(())
            }
        }
    }

    /// `strm s` (§4): `STMf`, open the stream, `STMc`.
    async fn start(&mut self, conn: &mut Conn<'_>, strm: Strm) -> std::io::Result<()> {
        conn.stat(self.stat(StatEvent::Flushed, [0; 4])).await?;
        if strm.request.len() > MAX_REQUEST {
            return Ok(());
        }
        let Some(track) = track_of(&strm) else {
            tracing::warn!(format = %char::from(strm.format), "Lyrion sent a format we can't play");
            return conn.stat(self.stat(StatEvent::NotSupported, [0; 4])).await;
        };
        self.track.pending_format = format_label(&track.format);
        let source_ip = if strm.server_ip.is_unspecified() {
            conn.server_ip
        } else {
            strm.server_ip
        };
        let (body_tx, body_rx) = mpsc::channel(64);
        self.track.next_stream_id += 1;
        self.track.bytes_received = Arc::new(AtomicU64::new(0));
        self.track.stream = Some(Stream::open(
            self.track.next_stream_id,
            SocketAddrV4::new(source_ip, strm.server_port),
            strm.request.clone(),
            strm.autostart >= b'2',
            body_tx,
            self.stream_events.0.clone(),
            Arc::clone(&self.track.bytes_received),
        ));
        self.playback.start(track, body_rx);
        self.track.pending = true;
        self.track.sent = SentOnce::default();
        conn.stat(self.stat(StatEvent::Connected, [0; 4])).await
    }

    async fn pause(&mut self, conn: &mut Conn<'_>, interval_ms: u32) -> std::io::Result<()> {
        if interval_ms != 0 {
            // A short silence for sync groups: not supported, the player plays on.
            return Ok(());
        }
        self.playback.pause();
        self.track.paused = true;
        self.sync_state();
        conn.stat(self.stat(StatEvent::Paused, [0; 4])).await
    }

    async fn unpause(&mut self, conn: &mut Conn<'_>, at_jiffies: u32) -> std::io::Result<()> {
        self.playback.unpause(start_time(at_jiffies, jiffies()));
        self.track.paused = false;
        self.sync_state();
        conn.stat(self.stat(StatEvent::Resumed, [0; 4])).await
    }

    fn on_volume(&self, adjust: bool, left: u32, right: u32) {
        if !adjust {
            // Fixed volume in Lyrion: the player's own volume stays as it is.
            return;
        }
        let (l, r) = (gain(left), gain(right));
        self.playback.volume(l, r, percent_of_gain(l.max(r)));
    }

    /// A rename from our side: tell the server, which renames the player in Lyrion.
    async fn rename(&mut self, conn: &mut Conn<'_>, name: String) -> std::io::Result<()> {
        let name = name.trim().to_owned();
        if name.is_empty() || name == self.config.name {
            return Ok(());
        }
        tracing::info!(name, "renaming the player");
        self.config.name.clone_from(&name);
        self.set_state(|s| s.name = name);
        conn.send(&ClientMessage::name(&self.config.name)).await
    }

    /// `setd` id 0: a query (answer with the name) or a rename (store, confirm).
    async fn on_name(&mut self, conn: &mut Conn<'_>, data: Option<Bytes>) -> std::io::Result<()> {
        if let Some(data) = data {
            let name = String::from_utf8_lossy(&data)
                .trim_end_matches('\0')
                .trim()
                .to_owned();
            if !name.is_empty() {
                tracing::info!(name, "renamed in Lyrion");
                self.config.name.clone_from(&name);
                self.set_state(|s| s.name = name);
            }
        }
        conn.send(&ClientMessage::name(&self.config.name)).await
    }

    async fn on_stream_event(&mut self, conn: &mut Conn<'_>, event: StreamEvent) -> std::io::Result<()> {
        let current = self.track.stream.as_ref().map(|s| s.id);
        match event {
            StreamEvent::Headers { id, headers } if Some(id) == current => {
                conn.send(&ClientMessage::Resp(headers)).await
            }
            StreamEvent::Metadata { id, block } if Some(id) == current => {
                conn.send(&ClientMessage::Meta(block)).await
            }
            StreamEvent::Ended { id, reason } if Some(id) == current => {
                conn.send(&ClientMessage::Dsco(reason)).await
            }
            _ => Ok(()), // from a stream we already closed
        }
    }

    /// Every 100 ms: the playback's events as STAT, and `STMt` every second while playing.
    async fn status_cycle(&mut self, conn: &mut Conn<'_>) -> std::io::Result<()> {
        let mut status = self.playback.status();
        let events = std::mem::take(&mut status.events);
        self.last_status = (status.clone(), Instant::now());
        self.buffer_state(&status, &events);
        for event in &events {
            if let Some(stat) = self.track.on_playback(event) {
                conn.stat(stat_of(stat, &status, &self.track, [0; 4])).await?;
            }
        }
        self.sync_state();
        let due = self.track.last_timer.is_none_or(|t| t.elapsed() >= TIMER_EVERY);
        if self.track.playing && !self.track.paused && due {
            self.track.last_timer = Some(Instant::now());
            conn.stat(stat_of(StatEvent::Timer, &status, &self.track, [0; 4]))
                .await?;
        }
        Ok(())
    }

    /// Buffer fill, underruns and the playing track's format, for the UI.
    fn buffer_state(&self, status: &PlaybackStatus, events: &[PlaybackEvent]) {
        let output_fill = fill_step(status.output_fill, status.output_size);
        let decode_fill = fill_step(status.decode_fill, status.decode_size);
        let underruns = u32::try_from(
            events
                .iter()
                .filter(|e| **e == PlaybackEvent::OutputUnderrun)
                .count(),
        )
        .unwrap_or(u32::MAX);
        let started = events.contains(&PlaybackEvent::TrackStarted);
        let format = &self.track.pending_format;
        let sample_rate = status.sample_rate;
        self.state.send_if_modified(|s| {
            let changed = s.output_fill != output_fill
                || s.decode_fill != decode_fill
                || underruns > 0
                || s.sample_rate != sample_rate;
            s.sample_rate = sample_rate;
            s.output_fill = output_fill;
            s.decode_fill = decode_fill;
            s.underruns = s.underruns.saturating_add(underruns);
            if started && s.format != *format {
                s.format.clone_from(format);
                return true;
            }
            changed
        });
    }

    /// A STAT between cycles: the last status, its elapsed time moved on while playing.
    fn stat(&self, event: StatEvent, timestamp: [u8; 4]) -> Stat {
        let (status, at) = &self.last_status;
        let mut status = status.clone();
        if self.track.playing && !self.track.paused {
            status.elapsed += at.elapsed();
        }
        stat_of(event, &status, &self.track, timestamp)
    }

    fn sync_state(&self) {
        let (playing, paused) = (self.track.playing, self.track.paused);
        self.state.send_if_modified(|s| {
            let changed = s.playing != playing || s.paused != paused;
            s.playing = playing;
            s.paused = paused;
            changed
        });
    }
}

impl TrackState {
    /// The STAT event for a playback event, if one is due (some go out once per track).
    fn on_playback(&mut self, event: &PlaybackEvent) -> Option<StatEvent> {
        match event {
            PlaybackEvent::TrackStarted => {
                self.pending = false;
                self.playing = true;
                self.last_timer = Some(Instant::now());
                Some(StatEvent::TrackStarted)
            }
            PlaybackEvent::DecodeDone => Some(StatEvent::DecodeDone),
            PlaybackEvent::OutputUnderrun => (!std::mem::replace(&mut self.sent.output_underrun, true))
                .then_some(StatEvent::OutputUnderrun),
            PlaybackEvent::PlaybackDone => {
                self.playing = false;
                (!std::mem::replace(&mut self.sent.underrun, true)).then_some(StatEvent::Underrun)
            }
            PlaybackEvent::Buffered => {
                (!std::mem::replace(&mut self.sent.loaded, true)).then_some(StatEvent::Loaded)
            }
            PlaybackEvent::DecodeError(why) => {
                tracing::warn!(why, "decoder error; Lyrion skips the track");
                Some(StatEvent::NotSupported)
            }
        }
    }
}

fn stat_of(event: StatEvent, status: &PlaybackStatus, track: &TrackState, timestamp: [u8; 4]) -> Stat {
    Stat {
        event,
        stream_buffer_size: status.decode_size,
        stream_buffer_fullness: status.decode_fill,
        bytes_received: track.bytes_received.load(Ordering::Relaxed),
        jiffies: jiffies(),
        output_buffer_size: status.output_size,
        output_buffer_fullness: status.output_fill,
        elapsed_ms: u32::try_from(status.elapsed.as_millis()).unwrap_or(u32::MAX),
        server_timestamp: timestamp,
    }
}

/// The track `strm s` describes; `None` for a format we don't decode.
fn track_of(strm: &Strm) -> Option<Track> {
    let format = match strm.format {
        b'p' => Format::Pcm {
            sample_rate: pcm_rate(strm.pcm_sample_rate),
            sample_size: pcm_sample_size(strm.pcm_sample_size),
            channels: match strm.pcm_channels {
                b'1' => Some(1),
                b'2' => Some(2),
                _ => None,
            },
            big_endian: match strm.pcm_endianness {
                b'0' => Some(true),
                b'1' => Some(false),
                _ => None,
            },
        },
        b'f' if strm.pcm_sample_size != b'o' => Format::Flac,
        b'm' => Format::Mp3,
        _ => return None,
    };
    let replay_gain = match strm.value() {
        0 => 1.0,
        g => gain(g),
    };
    Some(Track {
        format,
        replay_gain,
        autostart: matches!(strm.autostart, b'1' | b'3'),
        output_threshold: Duration::from_millis(u64::from(strm.output_threshold) * 100),
        transition: transition_of(strm),
    })
}

/// `strm`'s transition: type as an ASCII digit, period in seconds.
fn transition_of(strm: &Strm) -> Transition {
    let kind = match strm.transition_type {
        b'1' => TransitionKind::Crossfade,
        b'2' => TransitionKind::FadeIn,
        b'3' => TransitionKind::FadeOut,
        b'4' => TransitionKind::FadeInOut,
        _ => TransitionKind::None,
    };
    Transition {
        kind,
        duration: Duration::from_secs(u64::from(strm.transition_period)),
    }
}

/// A 16.16 fixed-point gain as a linear factor.
#[allow(clippy::cast_precision_loss, reason = "gains are far below 2^24")]
fn gain(fixed: u32) -> f32 {
    fixed as f32 / 65536.0
}

/// `audg`'s 16.16 gain for Lyrion volumes, measured on our server (Lyrion 9.1, squeezelite player
/// class, H15-55): not linear in dB below 30 %.
const VOLUME_CURVE: [(u32, u32); 14] = [
    (1, 16),
    (5, 31),
    (10, 72),
    (20, 395),
    (25, 926),
    (30, 1230),
    (40, 2048),
    (50, 3840),
    (60, 6656),
    (70, 12032),
    (75, 15872),
    (80, 20992),
    (90, 37120),
    (100, 65536),
];

/// Lyrion's volume (0-100) for a linear gain, interpolated in dB between the measured points.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "percent is clamped to 0..=100; gains are far below 2^24"
)]
fn percent_of_gain(linear: f32) -> u32 {
    if linear <= 0.0 {
        return 0;
    }
    let db = |fixed: u32| 20.0 * (fixed as f32 / 65536.0).log10();
    let target = 20.0 * linear.log10();
    let mut below = (0u32, f32::NEG_INFINITY);
    for (percent, fixed) in VOLUME_CURVE {
        let point = (percent, db(fixed));
        if target <= point.1 {
            if below.1.is_infinite() {
                return percent;
            }
            let t = (target - below.1) / (point.1 - below.1);
            return (below.0 as f32 + t * (point.0 - below.0) as f32).round() as u32;
        }
        below = point;
    }
    100
}

/// `strm u`'s start time: `None` = now; ignored when more than 10 s ahead (squeezelite).
fn start_time(at_jiffies: u32, now_jiffies: u32) -> Option<Instant> {
    if at_jiffies == 0 {
        return None;
    }
    let ahead = at_jiffies.wrapping_sub(now_jiffies);
    (ahead <= 10_000).then(|| Instant::now() + Duration::from_millis(u64::from(ahead)))
}

/// The write half of a connection, and the server's address (for streams from "IP 0").
struct Conn<'a> {
    writer: &'a mut tokio::net::tcp::OwnedWriteHalf,
    server_ip: Ipv4Addr,
}

impl Conn<'_> {
    async fn send(&mut self, message: &ClientMessage) -> std::io::Result<()> {
        write_message(self.writer, message).await
    }

    async fn stat(&mut self, stat: Stat) -> std::io::Result<()> {
        self.send(&ClientMessage::Stat(stat)).await
    }
}

async fn write_message(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &ClientMessage,
) -> std::io::Result<()> {
    writer.write_all(&message.encode()).await
}

/// Reads one server frame: u16 BE length (including the opcode), then that many bytes.
async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Bytes> {
    let len = usize::from(reader.read_u16().await?);
    let mut frame = BytesMut::zeroed(len);
    reader.read_exact(&mut frame).await?;
    Ok(frame.freeze())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_list_the_chosen_codecs() {
        let mut config = Config {
            name: "dock".into(),
            mac: [2, 0, 0, 0, 0, 1],
            server: None,
            max_sample_rate: 48_000,
            firmware: "1".into(),
            codecs: Vec::new(),
        };
        assert!(config.capabilities().ends_with("MaxSampleRate=48000,flc,mp3,pcm"));
        config.codecs = vec![Codec::Pcm];
        assert!(
            config.capabilities().ends_with("MaxSampleRate=48000,pcm"),
            "PCM only"
        );
    }

    #[test]
    fn buffer_fill_in_steps_of_ten() {
        assert_eq!(fill_step(0, 0), 0);
        assert_eq!(fill_step(59, 100), 50);
        assert_eq!(fill_step(100, 100), 100);
        assert_eq!(fill_step(300, 100), 100);
    }

    #[test]
    fn format_labels() {
        assert_eq!(format_label(&Format::Flac), "FLAC");
        let pcm = Format::Pcm {
            sample_rate: Some(44_100),
            sample_size: Some(16),
            channels: Some(2),
            big_endian: Some(false),
        };
        assert_eq!(format_label(&pcm), "PCM 44.1 kHz 16 bit");
    }

    #[test]
    fn macs_are_stable_local_and_unicast() {
        let a = mac_from_seed(b"machine-id-1");
        assert_eq!(a, mac_from_seed(b"machine-id-1"));
        assert_ne!(a, mac_from_seed(b"machine-id-2"));
        assert_eq!(a[0] & 0x03, 0x02, "locally administered, unicast");
    }

    #[test]
    fn unpause_times() {
        assert_eq!(start_time(0, 500), None, "0 = now");
        assert!(start_time(1500, 500).is_some(), "1 s ahead");
        assert!(
            start_time(20_500, 500).is_none(),
            "more than 10 s ahead is ignored"
        );
        assert!(start_time(5, u32::MAX - 5).is_some(), "across the wrap");
    }

    #[test]
    fn volume_mapping() {
        assert_eq!(percent_of_gain(1.0), 100);
        assert_eq!(percent_of_gain(0.0), 0);
        for (percent, fixed) in VOLUME_CURVE {
            assert_eq!(
                percent_of_gain(gain(fixed)),
                percent,
                "measured point {percent} %"
            );
        }
        assert_eq!(percent_of_gain(gain(1600)), 35, "between 30 % and 40 %");
        assert!((gain(0x1_0000) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn once_per_track_events() {
        let mut t = TrackState::default();
        assert_eq!(
            t.on_playback(&PlaybackEvent::OutputUnderrun),
            Some(StatEvent::OutputUnderrun)
        );
        assert_eq!(t.on_playback(&PlaybackEvent::OutputUnderrun), None);
        assert_eq!(
            t.on_playback(&PlaybackEvent::TrackStarted),
            Some(StatEvent::TrackStarted)
        );
        assert!(t.playing);
        assert_eq!(
            t.on_playback(&PlaybackEvent::PlaybackDone),
            Some(StatEvent::Underrun)
        );
        assert!(!t.playing);
    }
}

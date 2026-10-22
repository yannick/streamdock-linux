//! Slimproto packets (`docs/slimproto.md` §2-§5): what the player sends (8-byte header: opcode +
//! u32 BE length excluding the header) and what the server sends (u16 BE length including the
//! 4-byte opcode). Everything multi-byte is big-endian.

use std::net::Ipv4Addr;

use bytes::{Buf, BufMut, Bytes, BytesMut};

/// The slimproto TCP and discovery UDP port.
pub const PORT: u16 = 3483;

/// Largest server frame we accept (the u16 length field allows no more).
pub const MAX_FRAME: usize = 65535;

// ---------- player → server ----------

/// HELO: sent first on every connection (§3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Helo {
    pub mac: [u8; 6],
    /// This is a reconnect: the server may resume the running stream.
    pub reconnect: bool,
    /// Bytes of the current stream received so far.
    pub bytes_received: u64,
    /// Comma separated capabilities, e.g. `Model=squeezelite,…,flc,mp3,pcm`.
    pub capabilities: String,
}

/// A STAT event code (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatEvent {
    /// `STMc`: stream request accepted.
    Connected,
    /// `STMd`: decoder done, send the next track.
    DecodeDone,
    /// `STMf`: flushed / stopped.
    Flushed,
    /// `STMl`: buffered, waiting for `strm u` (autostart 0).
    Loaded,
    /// `STMn`: not supported / decode error.
    NotSupported,
    /// `STMo`: output underrun while still streaming.
    OutputUnderrun,
    /// `STMp`: paused.
    Paused,
    /// `STMr`: resumed.
    Resumed,
    /// `STMs`: track started.
    TrackStarted,
    /// `STMt`: timer / heartbeat, and the reply to `strm t`.
    Timer,
    /// `STMu`: underrun at the end: playback finished.
    Underrun,
}

impl StatEvent {
    #[must_use]
    pub fn code(self) -> &'static [u8; 4] {
        match self {
            Self::Connected => b"STMc",
            Self::DecodeDone => b"STMd",
            Self::Flushed => b"STMf",
            Self::Loaded => b"STMl",
            Self::NotSupported => b"STMn",
            Self::OutputUnderrun => b"STMo",
            Self::Paused => b"STMp",
            Self::Resumed => b"STMr",
            Self::TrackStarted => b"STMs",
            Self::Timer => b"STMt",
            Self::Underrun => b"STMu",
        }
    }
}

/// STAT: the player's status (§5), 53 bytes of payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub event: StatEvent,
    pub stream_buffer_size: u32,
    pub stream_buffer_fullness: u32,
    pub bytes_received: u64,
    /// Local monotonic clock in ms (wraps).
    pub jiffies: u32,
    pub output_buffer_size: u32,
    pub output_buffer_fullness: u32,
    pub elapsed_ms: u32,
    /// The 4 bytes of the `strm t` being answered, unchanged; zero otherwise.
    pub server_timestamp: [u8; 4],
}

/// Why the audio stream's HTTP connection ended (`DSCO`, §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Disconnect {
    /// End of stream.
    Closed = 0,
    /// A local error (e.g. headers too long).
    LocalError = 1,
    /// The remote end reset the connection.
    RemoteReset = 2,
    /// The stream source could not be reached.
    Unreachable = 3,
    Timeout = 4,
}

/// Everything the player sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    Helo(Helo),
    Stat(Stat),
    /// The audio stream's response headers, status line to the empty line.
    Resp(Bytes),
    /// Stream metadata (ICY block).
    Meta(Bytes),
    Dsco(Disconnect),
    /// A setting: id 0 = the player name.
    Setd {
        id: u8,
        data: Bytes,
    },
    /// The player goes away.
    Bye,
}

impl ClientMessage {
    /// The frame: opcode, payload length (u32 BE, without these 8 bytes), payload.
    ///
    /// # Panics
    ///
    /// If a payload exceeds 4 GiB (they are a few KiB at most).
    #[must_use]
    pub fn encode(&self) -> Bytes {
        let (opcode, payload) = match self {
            Self::Helo(h) => (b"HELO", helo_payload(h)),
            Self::Stat(s) => (b"STAT", stat_payload(s)),
            Self::Resp(b) => (b"RESP", BytesMut::from(&b[..])),
            Self::Meta(b) => (b"META", BytesMut::from(&b[..])),
            Self::Dsco(reason) => (b"DSCO", BytesMut::from(&[*reason as u8][..])),
            Self::Setd { id, data } => {
                let mut p = BytesMut::with_capacity(1 + data.len());
                p.put_u8(*id);
                p.put_slice(data);
                (b"SETD", p)
            }
            Self::Bye => (b"BYE!", BytesMut::from(&[0u8][..])),
        };
        let mut out = BytesMut::with_capacity(8 + payload.len());
        out.put_slice(opcode);
        out.put_u32(u32::try_from(payload.len()).expect("payloads are small"));
        out.put_slice(&payload);
        out.freeze()
    }

    /// `SETD` id 0 with the player name, NUL terminated.
    #[must_use]
    pub fn name(name: &str) -> Self {
        let mut data = BytesMut::with_capacity(name.len() + 1);
        data.put_slice(name.as_bytes());
        data.put_u8(0);
        Self::Setd {
            id: 0,
            data: data.freeze(),
        }
    }
}

fn helo_payload(h: &Helo) -> BytesMut {
    let mut p = BytesMut::with_capacity(36 + h.capabilities.len());
    p.put_u8(12); // device id: squeezeplay (with Model=squeezelite: LMS's squeezelite class)
    p.put_u8(0); // firmware revision
    p.put_slice(&h.mac);
    p.put_slice(&[0; 16]); // UUID
    p.put_u16(if h.reconnect { 0x4000 } else { 0 });
    p.put_u64(h.bytes_received);
    p.put_slice(&[0, 0]); // language
    p.put_slice(h.capabilities.as_bytes());
    p
}

fn stat_payload(s: &Stat) -> BytesMut {
    let mut p = BytesMut::with_capacity(53);
    p.put_slice(s.event.code());
    p.put_u8(0); // num CRLF
    p.put_u8(0); // MAS initialized
    p.put_u8(0); // MAS mode
    p.put_u32(s.stream_buffer_size);
    p.put_u32(s.stream_buffer_fullness);
    p.put_u64(s.bytes_received);
    p.put_u16(0xffff); // signal strength: wired
    p.put_u32(s.jiffies);
    p.put_u32(s.output_buffer_size);
    p.put_u32(s.output_buffer_fullness);
    p.put_u32(s.elapsed_ms / 1000);
    p.put_u16(0); // voltage
    p.put_u32(s.elapsed_ms);
    p.put_slice(&s.server_timestamp);
    p.put_u16(0); // error code
    p
}

// ---------- server → player ----------

/// `strm`'s command byte (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrmCommand {
    Start,
    Pause,
    Unpause,
    Stop,
    Status,
    Flush,
    Skip,
    Other(u8),
}

/// `strm`: stream control (§4). The format and PCM bytes are only meaningful for `s`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Strm {
    pub command: StrmCommand,
    /// ASCII `0`-`3`.
    pub autostart: u8,
    /// ASCII codec id: `p` PCM, `f` FLAC, `m` MP3 …, `?` decided later.
    pub format: u8,
    pub pcm_sample_size: u8,
    pub pcm_sample_rate: u8,
    pub pcm_channels: u8,
    pub pcm_endianness: u8,
    /// KiB to buffer before decoding starts.
    pub threshold: u8,
    pub transition_period: u8,
    pub transition_type: u8,
    pub flags: u8,
    /// Output buffer to fill before playback starts, tenths of a second.
    pub output_threshold: u8,
    /// `s`: replay gain 16.16; `p`/`a`: interval ms; `u`: start jiffies; `t`: timestamp to echo.
    pub gain_or_time: [u8; 4],
    pub server_port: u16,
    /// `0.0.0.0` = the slimproto server itself.
    pub server_ip: Ipv4Addr,
    /// The HTTP request to send verbatim (with `s`).
    pub request: Bytes,
}

impl Strm {
    /// `gain_or_time` as a number.
    #[must_use]
    pub fn value(&self) -> u32 {
        u32::from_be_bytes(self.gain_or_time)
    }
}

/// Everything the server sends that we act on; anything else is [`ServerMessage::Other`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    Strm(Strm),
    /// Volume: new gains 16.16 (0x10000 = 0 dB); `adjust` false = fixed volume.
    Audg {
        adjust: bool,
        left: u32,
        right: u32,
    },
    /// Outputs on or off (the player's power): S/PDIF enable byte non-zero = on.
    Aude {
        spdif: bool,
        dac: bool,
    },
    /// A setting; `data` `None` = a query. Id 0 = the player name.
    Setd {
        id: u8,
        data: Option<Bytes>,
    },
    /// Switch to another server.
    Serv {
        ip: Ipv4Addr,
        sync_group: Option<String>,
    },
    /// Continue a direct stream (autostart ≥ 2).
    Cont {
        metaint: u32,
        looping: bool,
    },
    /// The codec of a `strm s` with format `?`: format, size, rate, channels, endianness.
    Codc([u8; 5]),
    /// An opcode we ignore (`vers`, `vfdc`, `grfe` …).
    Other([u8; 4]),
}

/// A server frame that can't be parsed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("frame shorter than its opcode ({0} bytes)")]
    NoOpcode(usize),
    #[error("{opcode} body too short: {len} bytes, need {need}")]
    Short { opcode: String, len: usize, need: usize },
}

impl ServerMessage {
    /// Parses one frame's content: opcode + body (the u16 length already stripped).
    pub fn parse(frame: &[u8]) -> Result<Self, ParseError> {
        let Some((&opcode, body)) = frame.split_first_chunk::<4>() else {
            return Err(ParseError::NoOpcode(frame.len()));
        };
        Ok(match &opcode {
            b"strm" => Self::Strm(parse_strm(body)?),
            b"audg" => {
                need(opcode, body, 18)?;
                let mut b = &body[8..];
                let adjust = b.get_u8() != 0;
                b.advance(1); // preamp
                Self::Audg {
                    adjust,
                    left: b.get_u32(),
                    right: b.get_u32(),
                }
            }
            b"aude" => {
                need(opcode, body, 2)?;
                Self::Aude {
                    spdif: body[0] != 0,
                    dac: body[1] != 0,
                }
            }
            b"setd" => {
                need(opcode, body, 1)?;
                let data = (body.len() > 1).then(|| Bytes::copy_from_slice(&body[1..]));
                Self::Setd { id: body[0], data }
            }
            b"serv" => {
                need(opcode, body, 4)?;
                let ip = Ipv4Addr::from(be32(body));
                let sync_group = body.get(4..14).map(|g| String::from_utf8_lossy(g).into_owned());
                Self::Serv { ip, sync_group }
            }
            b"cont" => {
                need(opcode, body, 5)?;
                Self::Cont {
                    metaint: be32(body),
                    looping: body[4] != 0,
                }
            }
            b"codc" => {
                need(opcode, body, 5)?;
                let mut codc = [0; 5];
                codc.copy_from_slice(&body[..5]);
                Self::Codc(codc)
            }
            _ => Self::Other(opcode),
        })
    }
}

/// The first 4 bytes as a u32 BE (callers checked the length).
fn be32(body: &[u8]) -> u32 {
    let mut b = body;
    b.get_u32()
}

fn need(opcode: [u8; 4], body: &[u8], n: usize) -> Result<(), ParseError> {
    if body.len() < n {
        return Err(ParseError::Short {
            opcode: String::from_utf8_lossy(&opcode).into_owned(),
            len: body.len(),
            need: n,
        });
    }
    Ok(())
}

fn parse_strm(body: &[u8]) -> Result<Strm, ParseError> {
    need(*b"strm", body, 24)?;
    let mut b = body;
    let command = match b.get_u8() {
        b's' => StrmCommand::Start,
        b'p' => StrmCommand::Pause,
        b'u' => StrmCommand::Unpause,
        b'q' => StrmCommand::Stop,
        b't' => StrmCommand::Status,
        b'f' => StrmCommand::Flush,
        b'a' => StrmCommand::Skip,
        other => StrmCommand::Other(other),
    };
    Ok(Strm {
        command,
        autostart: b.get_u8(),
        format: b.get_u8(),
        pcm_sample_size: b.get_u8(),
        pcm_sample_rate: b.get_u8(),
        pcm_channels: b.get_u8(),
        pcm_endianness: b.get_u8(),
        threshold: b.get_u8(),
        transition_period: {
            b.advance(1); // spdif enable
            b.get_u8()
        },
        transition_type: b.get_u8(),
        flags: b.get_u8(),
        output_threshold: b.get_u8(),
        gain_or_time: {
            b.advance(1); // slaves
            let mut t = [0; 4];
            b.copy_to_slice(&mut t);
            t
        },
        server_port: b.get_u16(),
        server_ip: Ipv4Addr::from(b.get_u32()),
        request: Bytes::copy_from_slice(b),
    })
}

/// The PCM sample rate for `strm`'s rate byte (`'0'` + index), `None` for `?` (from header).
#[must_use]
pub fn pcm_rate(byte: u8) -> Option<u32> {
    const RATES: [u32; 20] = [
        11025, 22050, 32000, 44100, 48000, 8000, 12000, 16000, 24000, 96000, 88200, 176_400, 192_000,
        352_800, 384_000, 0, 705_600, 768_000, 1_411_200, 1_536_000,
    ];
    let rate = *RATES.get(usize::from(byte.checked_sub(b'0')?))?;
    (rate != 0).then_some(rate)
}

/// Bits per sample for `strm`'s size byte, `None` for `?`.
#[must_use]
pub fn pcm_sample_size(byte: u8) -> Option<u8> {
    match byte {
        b'0' => Some(8),
        b'1' => Some(16),
        b'2' => Some(24),
        b'3' => Some(32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helo_layout() {
        let frame = ClientMessage::Helo(Helo {
            mac: [0x02, 0x11, 0x22, 0x33, 0x44, 0x55],
            reconnect: true,
            bytes_received: 0x0000_0001_0000_0002,
            capabilities: "Model=squeezelite,pcm".into(),
        })
        .encode();
        assert_eq!(&frame[..4], b"HELO");
        assert_eq!(
            &frame[4..8],
            &(36u32 + 21).to_be_bytes(),
            "length excludes the header"
        );
        let p = &frame[8..];
        assert_eq!(p[0], 12, "device id");
        assert_eq!(p[1], 0, "revision");
        assert_eq!(&p[2..8], &[0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
        assert_eq!(&p[8..24], &[0; 16], "UUID");
        assert_eq!(&p[24..26], &[0x40, 0x00], "reconnect bit");
        assert_eq!(
            &p[26..34],
            &[0, 0, 0, 1, 0, 0, 0, 2],
            "bytes received, high then low"
        );
        assert_eq!(&p[34..36], &[0, 0], "language");
        assert_eq!(&p[36..], b"Model=squeezelite,pcm");
    }

    #[test]
    fn stat_layout() {
        let frame = ClientMessage::Stat(Stat {
            event: StatEvent::Timer,
            stream_buffer_size: 0x0020_0000,
            stream_buffer_fullness: 0x1234,
            bytes_received: 5000,
            jiffies: 0xdead_beef,
            output_buffer_size: 3_528_000,
            output_buffer_fullness: 1000,
            elapsed_ms: 61_500,
            server_timestamp: [1, 2, 3, 4],
        })
        .encode();
        assert_eq!(&frame[..8], b"STAT\0\0\0\x35", "53-byte payload");
        let p = &frame[8..];
        assert_eq!(p.len(), 53);
        assert_eq!(&p[0..4], b"STMt");
        assert_eq!(&p[4..7], &[0, 0, 0]);
        assert_eq!(&p[7..11], &0x0020_0000u32.to_be_bytes());
        assert_eq!(&p[11..15], &0x1234u32.to_be_bytes());
        assert_eq!(&p[15..23], &5000u64.to_be_bytes());
        assert_eq!(&p[23..25], &[0xff, 0xff], "wired");
        assert_eq!(&p[25..29], &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(&p[29..33], &3_528_000u32.to_be_bytes());
        assert_eq!(&p[33..37], &1000u32.to_be_bytes());
        assert_eq!(&p[37..41], &61u32.to_be_bytes(), "elapsed seconds");
        assert_eq!(&p[41..43], &[0, 0], "voltage");
        assert_eq!(&p[43..47], &61_500u32.to_be_bytes(), "elapsed ms");
        assert_eq!(&p[47..51], &[1, 2, 3, 4], "timestamp echoed unchanged");
        assert_eq!(&p[51..53], &[0, 0], "error code");
    }

    #[test]
    fn small_client_messages() {
        assert_eq!(
            &ClientMessage::Dsco(Disconnect::Unreachable).encode()[..],
            b"DSCO\0\0\0\x01\x03"
        );
        assert_eq!(
            &ClientMessage::name("luckfox").encode()[..],
            b"SETD\0\0\0\x09\0luckfox\0"
        );
        assert_eq!(&ClientMessage::Bye.encode()[..], b"BYE!\0\0\0\x01\0");
        assert_eq!(
            &ClientMessage::Resp(Bytes::from_static(b"HTTP/1.0 200 OK\r\n\r\n")).encode()[..8],
            b"RESP\0\0\0\x13"
        );
    }

    /// `strm s` as LMS sends it for a local FLAC: autostart 1, format f, threshold 255 KiB,
    /// output threshold 1 s, replay gain 0, port 9000, IP 0, then the HTTP request.
    fn strm_start() -> Vec<u8> {
        let mut f = b"strm".to_vec();
        f.extend_from_slice(b"s1f????");
        f.extend_from_slice(&[255, b'0', 0, b'0', 0, 10, 0]);
        f.extend_from_slice(&[0, 0, 0, 0]); // replay gain
        f.extend_from_slice(&9000u16.to_be_bytes());
        f.extend_from_slice(&[0, 0, 0, 0]);
        f.extend_from_slice(b"GET /stream.mp3?player=02:11:22:33:44:55 HTTP/1.0\r\n\r\n");
        f
    }

    #[test]
    fn parses_strm_start() {
        let frame = strm_start();
        let ServerMessage::Strm(s) = ServerMessage::parse(&frame).unwrap() else {
            panic!("not strm")
        };
        assert_eq!(s.command, StrmCommand::Start);
        assert_eq!(s.autostart, b'1');
        assert_eq!(s.format, b'f');
        assert_eq!(s.threshold, 255);
        assert_eq!(s.output_threshold, 10);
        assert_eq!(s.value(), 0);
        assert_eq!(s.server_port, 9000);
        assert_eq!(s.server_ip, Ipv4Addr::UNSPECIFIED);
        assert!(s.request.starts_with(b"GET /stream.mp3"));
    }

    #[test]
    fn parses_strm_t_and_keeps_the_timestamp_bytes() {
        let mut frame = strm_start();
        frame[4] = b't';
        frame[18..22].copy_from_slice(&[9, 8, 7, 6]);
        frame.truncate(28);
        let ServerMessage::Strm(s) = ServerMessage::parse(&frame).unwrap() else {
            panic!("not strm")
        };
        assert_eq!(s.command, StrmCommand::Status);
        assert_eq!(s.gain_or_time, [9, 8, 7, 6]);
        assert!(s.request.is_empty());
    }

    #[test]
    fn parses_audg_aude_setd_serv() {
        let mut audg = b"audg".to_vec();
        audg.extend_from_slice(&[0, 0, 0, 64, 0, 0, 0, 64, 1, 255]);
        audg.extend_from_slice(&0x8000u32.to_be_bytes());
        audg.extend_from_slice(&0x1_0000u32.to_be_bytes());
        assert_eq!(
            ServerMessage::parse(&audg).unwrap(),
            ServerMessage::Audg {
                adjust: true,
                left: 0x8000,
                right: 0x1_0000
            }
        );
        assert_eq!(
            ServerMessage::parse(b"aude\0\x01").unwrap(),
            ServerMessage::Aude {
                spdif: false,
                dac: true
            }
        );
        assert_eq!(
            ServerMessage::parse(b"setd\0").unwrap(),
            ServerMessage::Setd { id: 0, data: None },
            "a name query"
        );
        assert_eq!(
            ServerMessage::parse(b"setd\0Kitchen\0").unwrap(),
            ServerMessage::Setd {
                id: 0,
                data: Some(Bytes::from_static(b"Kitchen\0"))
            }
        );
        assert_eq!(
            ServerMessage::parse(b"serv\xc0\xa8\x01\x02").unwrap(),
            ServerMessage::Serv {
                ip: Ipv4Addr::new(192, 168, 1, 2),
                sync_group: None
            }
        );
        assert_eq!(
            ServerMessage::parse(b"vers9.0.2").unwrap(),
            ServerMessage::Other(*b"vers")
        );
    }

    #[test]
    fn short_frames_are_errors_not_panics() {
        assert_eq!(ServerMessage::parse(b"st"), Err(ParseError::NoOpcode(2)));
        assert!(ServerMessage::parse(b"strms1").is_err());
        assert!(ServerMessage::parse(b"audg\0\0").is_err());
    }

    #[test]
    fn pcm_tables() {
        assert_eq!(pcm_rate(b'3'), Some(44100));
        assert_eq!(pcm_rate(b'4'), Some(48000));
        assert_eq!(pcm_rate(b'<'), Some(192_000));
        assert_eq!(pcm_rate(b'?'), None);
        assert_eq!(pcm_rate(b'C'), Some(1_536_000));
        assert_eq!(pcm_sample_size(b'1'), Some(16));
        assert_eq!(pcm_sample_size(b'?'), None);
    }
}

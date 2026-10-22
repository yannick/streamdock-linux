//! A2DP and HFP codecs from BlueZ's `MediaTransport1` (H15-49): the `Codec` byte and the
//! `Configuration` bytes the two sides agreed on, decoded into "LDAC 48 kHz, stereo".
//!
//! Layouts from the A2DP spec (SBC, MPEG-2/4 AAC) and BlueZ's `profiles/audio/a2dp-codecs.h`
//! (vendor codecs: 4-byte vendor id and 2-byte codec id, little endian, then the codec's fields).

/// A decoded codec configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Codec {
    /// "SBC", "SBC-XQ", "AAC", "aptX", "aptX HD", "LDAC", "CVSD", "mSBC" …
    pub name: String,
    /// The sample rate in Hz, when the configuration says.
    pub rate: Option<u32>,
    /// e.g. "stereo", "joint stereo", "dual channel", "mono".
    pub channels: String,
    /// More fields, e.g. "bitpool 2–53".
    pub extra: String,
    /// The bandwidth the stream uses (H15-53), e.g. "up to 328 kbit/s"; empty where unknown.
    pub bitrate: String,
}

impl Codec {
    /// Short label for a list row: "LDAC 48 kHz".
    #[must_use]
    pub fn label(&self) -> String {
        match self.rate {
            Some(rate) => format!("{} {}", self.name, khz(rate)),
            None => self.name.clone(),
        }
    }

    /// The configuration in words: "48 kHz, stereo".
    #[must_use]
    pub fn configuration(&self) -> String {
        let mut parts = Vec::new();
        if let Some(rate) = self.rate {
            parts.push(khz(rate));
        }
        if !self.channels.is_empty() {
            parts.push(self.channels.clone());
        }
        if !self.extra.is_empty() {
            parts.push(self.extra.clone());
        }
        parts.join(", ")
    }
}

/// "44.1 kHz", "48 kHz".
fn khz(rate: u32) -> String {
    if rate.is_multiple_of(1000) {
        format!("{} kHz", rate / 1000)
    } else {
        format!("{:.1} kHz", f64::from(rate) / 1000.0)
    }
}

/// Hands-free profiles carry voice codecs (CVSD, mSBC, LC3-SWB) instead of A2DP ones.
const HANDS_FREE: [&str; 4] = ["00001108", "00001112", "0000111e", "0000111f"];

/// Decodes a transport's codec; `uuid` is the transport's profile.
#[must_use]
pub fn decode(codec: u8, config: &[u8], uuid: &str) -> Codec {
    let uuid = uuid.to_ascii_lowercase();
    if HANDS_FREE.iter().any(|p| uuid.starts_with(p)) {
        let (name, rate) = match codec {
            0x01 => ("CVSD", 8000),
            0x02 => ("mSBC", 16000),
            0x03 => ("LC3-SWB", 32000),
            _ => return named(&format!("voice codec {codec}")),
        };
        return Codec {
            name: name.into(),
            rate: Some(rate),
            channels: "mono".into(),
            extra: String::new(),
            // CVSD: 8 kHz × 8 bit; mSBC: 57 bytes per 7.5 ms frame.
            bitrate: match codec {
                0x01 => "64 kbit/s".into(),
                0x02 => "60.8 kbit/s".into(),
                _ => String::new(),
            },
        };
    }
    match codec {
        0x00 => sbc(config),
        0x01 => named("MP3"),
        0x02 => aac(config),
        0x04 => named("ATRAC"),
        0xff => vendor(config),
        other => named(&format!("codec {other}")),
    }
}

fn named(name: &str) -> Codec {
    Codec {
        name: name.into(),
        rate: None,
        channels: String::new(),
        extra: String::new(),
        bitrate: String::new(),
    }
}

/// The rate from a bit field, highest bit first: `rates[0]` is bit 7 of `bits`.
fn rate_of(bits: u8, rates: &[u32]) -> Option<u32> {
    rates
        .iter()
        .enumerate()
        .find(|(i, _)| bits & (0x80 >> i) != 0)
        .map(|(_, r)| *r)
}

/// SBC, aptX and aptX HD's channel nibble: 8 mono, 4 dual channel, 2 stereo, 1 joint stereo.
fn channel_mode(nibble: u8) -> &'static str {
    match nibble {
        n if n & 0x8 != 0 => "mono",
        n if n & 0x4 != 0 => "dual channel",
        n if n & 0x2 != 0 => "stereo",
        n if n & 0x1 != 0 => "joint stereo",
        _ => "",
    }
}

const FOUR_RATES: [u32; 4] = [16000, 32000, 44100, 48000];
const AAC_LOW: [u32; 8] = [8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100];
const AAC_HIGH: [u32; 4] = [48000, 64000, 88200, 96000];
const LDAC_RATES: [u32; 6] = [44100, 48000, 88200, 96000, 176_400, 192_000];

/// SBC: rate and channel mode, block length / subbands / allocation, min and max bitpool.
/// PipeWire's "SBC-XQ" is SBC in dual channel mode at 44.1 or 48 kHz with a high bitpool.
fn sbc(config: &[u8]) -> Codec {
    let [first, second, min, max, ..] = *config else {
        return named("SBC");
    };
    let rate = rate_of(first & 0xf0, &FOUR_RATES);
    let channels = channel_mode(first & 0x0f);
    let xq = channels == "dual channel" && rate.is_some_and(|r| r >= 44100) && max >= 38;
    let bitrate = rate
        .and_then(|rate| sbc_bitrate(rate, channels, second, max))
        .map(|bps| format!("up to {} kbit/s", kbit(bps)))
        .unwrap_or_default();
    Codec {
        name: if xq { "SBC-XQ" } else { "SBC" }.into(),
        rate,
        channels: channels.into(),
        extra: format!("bitpool {min}–{max}"),
        bitrate,
    }
}

/// SBC's bit rate at `bitpool` (A2DP spec 12.9): frame length × frames per second. `second` is
/// the configuration's block length (bits 7–4: 4, 8, 12, 16) and subbands (bits 3–2: 4, 8) byte.
fn sbc_bitrate(rate: u32, channels: &str, second: u8, bitpool: u8) -> Option<u32> {
    let blocks = [4, 8, 12, 16]
        .into_iter()
        .enumerate()
        .find(|(i, _)| second & (0x80 >> i) != 0)?
        .1;
    let subbands = match second & 0x0c {
        0x08 => 4,
        0x04 => 8,
        _ => return None,
    };
    let bitpool = u32::from(bitpool);
    let (nchannels, audio_bits) = match channels {
        "mono" => (1, blocks * bitpool),
        "dual channel" => (2, 2 * blocks * bitpool),
        "stereo" => (2, blocks * bitpool),
        "joint stereo" => (2, subbands + blocks * bitpool),
        _ => return None,
    };
    let frame_bytes = 4 + (4 * subbands * nchannels) / 8 + audio_bits.div_ceil(8);
    Some(8 * frame_bytes * rate / (subbands * blocks))
}

/// "328", "352.8".
fn kbit(bps: u32) -> String {
    let tenths = (bps + 50) / 100;
    if tenths.is_multiple_of(10) {
        format!("{}", tenths / 10)
    } else {
        format!("{}.{}", tenths / 10, tenths % 10)
    }
}

/// MPEG-2/4 AAC: object type, 12 rate bits, channels, VBR flag and bitrate.
fn aac(config: &[u8]) -> Codec {
    let [_, r1, r2, ..] = *config else {
        return named("AAC");
    };
    let rate = rate_of(r1, &AAC_LOW).or_else(|| rate_of(r2 & 0xf0, &AAC_HIGH));
    let channels = match r2 & 0x0c {
        0x08 => "mono",
        0x04 => "stereo",
        _ => "",
    };
    let (extra, bitrate) = match config.get(3..6) {
        Some(&[b3, b4, b5]) => {
            let bitrate = (u32::from(b3 & 0x7f) << 16) | (u32::from(b4) << 8) | u32::from(b5);
            let vbr = if b3 & 0x80 != 0 { "VBR" } else { "" };
            let bitrate = if bitrate > 0 {
                format!("up to {} kbit/s", kbit(bitrate))
            } else {
                String::new()
            };
            (vbr.to_owned(), bitrate)
        }
        _ => (String::new(), String::new()),
    };
    Codec {
        name: "AAC".into(),
        rate,
        channels: channels.into(),
        extra,
        bitrate,
    }
}

/// Vendor codecs: aptX, aptX HD, aptX LL, LDAC, `FastStream`, Opus (`PipeWire`) …
fn vendor(config: &[u8]) -> Codec {
    let (Some(id), Some(codec)) = (config.get(..4), config.get(4..6)) else {
        return named("vendor codec");
    };
    let vendor = u32::from_le_bytes(id.try_into().expect("4 bytes"));
    let codec = u16::from_le_bytes(codec.try_into().expect("2 bytes"));
    let body = &config[6..];
    match (vendor, codec) {
        (0x4f, 0x01) => aptx("aptX", body),
        (0xd7, 0x24) => aptx("aptX HD", body),
        (0x0a, 0x02) => aptx("aptX LL", body),
        (0x0a, 0x01) => named("FastStream"),
        (0x12d, 0xaa) => ldac(body),
        (0x5f1, 0x1005) => named("Opus"),
        _ => named(&format!("vendor codec {vendor:#06x}:{codec:#06x}")),
    }
}

fn aptx(name: &str, body: &[u8]) -> Codec {
    let Some(&first) = body.first() else {
        return named(name);
    };
    let rate = rate_of(first & 0xf0, &FOUR_RATES);
    // A fixed 4:1 compression of 16-bit samples (aptX HD: 24-bit to 6 bits), two channels.
    let bits = if name == "aptX HD" { 6 } else { 4 };
    Codec {
        name: name.into(),
        rate,
        channels: channel_mode(first & 0x0f).into(),
        extra: String::new(),
        bitrate: rate
            .map(|r| format!("{} kbit/s", kbit(r * 2 * bits)))
            .unwrap_or_default(),
    }
}

/// LDAC: a rate byte (0x20 44.1 … 0x01 192 kHz) and a channel byte (4 mono, 2 dual, 1 stereo).
/// The quality mode (990/660/330 kbit/s, adaptive) is the encoder's choice and not in here.
fn ldac(body: &[u8]) -> Codec {
    let [rates, modes, ..] = *body else {
        return named("LDAC");
    };
    let rate = LDAC_RATES
        .iter()
        .enumerate()
        .find(|(i, _)| rates & (0x20 >> i) != 0)
        .map(|(_, r)| *r);
    let channels = match modes {
        m if m & 0x4 != 0 => "mono",
        m if m & 0x2 != 0 => "dual channel",
        m if m & 0x1 != 0 => "stereo",
        _ => "",
    };
    // The encoder picks 990, 660 or 330 kbit/s (909/606/303 at 44.1 kHz); PipeWire adapts it to
    // the link by default (`bluez5.a2dp.ldac.quality = auto`).
    let bitrate = match rate {
        Some(r) if r % 44100 == 0 => "303–909 kbit/s, chosen by the encoder",
        Some(_) => "330–990 kbit/s, chosen by the encoder",
        None => "",
    };
    Codec {
        name: "LDAC".into(),
        rate,
        channels: channels.into(),
        extra: String::new(),
        bitrate: bitrate.into(),
    }
}

/// Bytes as "2d 01 00 00".
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const A2DP_SINK: &str = "0000110b-0000-1000-8000-00805f9b34fb";

    #[test]
    fn ldac_from_the_board() {
        // H15-46: WH-1000XM4 on the Luckfox.
        let c = decode(0xff, &[0x2d, 0x01, 0, 0, 0xaa, 0x00, 0x10, 0x01], A2DP_SINK);
        assert_eq!(c.label(), "LDAC 48 kHz");
        assert_eq!(c.configuration(), "48 kHz, stereo");
        assert_eq!(c.bitrate, "330–990 kbit/s, chosen by the encoder");
    }

    #[test]
    fn sbc_and_sbc_xq() {
        // 44.1 kHz joint stereo, bitpool 2–53: plain SBC (high quality).
        let c = decode(0x00, &[0x21, 0x15, 2, 53], A2DP_SINK);
        assert_eq!(c.label(), "SBC 44.1 kHz");
        assert_eq!(c.configuration(), "44.1 kHz, joint stereo, bitpool 2–53");
        // The well-known 328 kbit/s of SBC "high quality" (16 blocks, 8 subbands, bitpool 53).
        assert_eq!(c.bitrate, "up to 328 kbit/s");
        // 44.1 kHz dual channel, bitpool 2–43: PipeWire's SBC-XQ.
        let xq = decode(0x00, &[0x24, 0x15, 2, 43], A2DP_SINK);
        assert_eq!(xq.label(), "SBC-XQ 44.1 kHz");
        // Dual channel at bitpool 43: two 16-block frames of 172 audio bytes (bitpool 47 gives 551).
        assert_eq!(xq.bitrate, "up to 507.2 kbit/s");
        assert_eq!(decode(0x00, &[], A2DP_SINK).label(), "SBC");
    }

    #[test]
    fn aac() {
        // MPEG-4 AAC LC, 48 kHz, stereo, VBR up to 320 kbit/s.
        let c = decode(0x02, &[0x40, 0x00, 0x84, 0x84, 0xe2, 0x00], A2DP_SINK);
        assert_eq!(c.label(), "AAC 48 kHz");
        assert_eq!(c.configuration(), "48 kHz, stereo, VBR");
        assert_eq!(c.bitrate, "up to 320 kbit/s");
    }

    #[test]
    fn aptx_family() {
        let aptx = decode(0xff, &[0x4f, 0, 0, 0, 0x01, 0, 0x22], A2DP_SINK);
        assert_eq!(aptx.label(), "aptX 44.1 kHz");
        assert_eq!(aptx.bitrate, "352.8 kbit/s");
        let hd = decode(0xff, &[0xd7, 0, 0, 0, 0x24, 0, 0x12, 0, 0, 0, 0], A2DP_SINK);
        assert_eq!(hd.label(), "aptX HD 48 kHz");
        assert_eq!(hd.channels, "stereo");
        assert_eq!(hd.bitrate, "576 kbit/s");
        assert_eq!(
            decode(0xff, &[1, 2, 3, 4, 5, 6], A2DP_SINK).name,
            "vendor codec 0x4030201:0x0605"
        );
    }

    #[test]
    fn hands_free_voice() {
        let hfp = "0000111e-0000-1000-8000-00805f9b34fb";
        assert_eq!(decode(0x02, &[], hfp).label(), "mSBC 16 kHz");
        assert_eq!(decode(0x01, &[], hfp).label(), "CVSD 8 kHz");
    }

    #[test]
    fn hex_bytes() {
        assert_eq!(hex(&[0x2d, 0x01, 0xaa]), "2d 01 aa");
    }
}

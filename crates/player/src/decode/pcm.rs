//! PCM: raw samples with the layout from `strm`, or a WAV / AIFF header when `strm` says "from
//! header". Converted here without symphonia: it is a few lines per sample layout, and a live
//! WAV stream from Lyrion often has no valid data length, which symphonia's readers expect.

use std::io::{self, Read};

use slimproto::Format;

/// The layout of the samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PcmSpec {
    pub rate: u32,
    pub channels: u16,
    /// 8, 16, 24 or 32.
    pub bits: u8,
    pub big_endian: bool,
    /// 8-bit samples are unsigned (WAV); raw and AIFF 8-bit samples are signed.
    pub unsigned_8: bool,
    /// 32-bit IEEE float (WAV format 3, AIFC `fl32`).
    pub float: bool,
    /// Audio bytes after the header, when the header gives a usable length.
    pub data_len: Option<u64>,
}

impl PcmSpec {
    /// The layout `strm` gave, or `None` when it said "from header".
    pub(super) fn from_strm(format: Format) -> Option<Self> {
        let Format::Pcm {
            sample_rate: Some(rate),
            sample_size: Some(bits),
            channels: Some(channels),
            big_endian: Some(big_endian),
        } = format
        else {
            return None;
        };
        Some(Self {
            rate,
            channels: channels.into(),
            bits,
            big_endian,
            unsigned_8: false,
            float: false,
            data_len: None,
        })
    }

    pub(super) fn frame_bytes(&self) -> usize {
        usize::from(self.bits / 8) * usize::from(self.channels)
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        let bits_ok = matches!(self.bits, 8 | 16 | 24 | 32) && (!self.float || self.bits == 32);
        if !bits_ok {
            return Err(format!("unsupported PCM sample size {} bit", self.bits));
        }
        if self.channels == 0 || self.rate == 0 {
            return Err(format!(
                "invalid PCM format: {} Hz, {} channels",
                self.rate, self.channels
            ));
        }
        Ok(())
    }
}

/// Reads a WAV or AIFF header up to the first audio byte.
pub(super) fn read_header(r: &mut impl Read) -> io::Result<PcmSpec> {
    let mut magic = [0; 12];
    r.read_exact(&mut magic)?;
    match (&magic[0..4], &magic[8..12]) {
        (b"RIFF", b"WAVE") => read_wav(r),
        (b"FORM", b"AIFF") => read_aiff(r, false),
        (b"FORM", b"AIFC") => read_aiff(r, true),
        _ => Err(invalid("PCM stream without a WAV or AIFF header")),
    }
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// A chunk header: id and length (`big` for AIFF).
fn chunk(r: &mut impl Read, big: bool) -> io::Result<([u8; 4], u32)> {
    let mut h = [0; 8];
    r.read_exact(&mut h)?;
    let len = [h[4], h[5], h[6], h[7]];
    let len = if big {
        u32::from_be_bytes(len)
    } else {
        u32::from_le_bytes(len)
    };
    Ok(([h[0], h[1], h[2], h[3]], len))
}

/// Skips a chunk body (chunks are padded to even lengths).
fn skip(r: &mut impl Read, len: u32) -> io::Result<()> {
    let padded = u64::from(len) + u64::from(len & 1);
    let skipped = io::copy(&mut r.take(padded), &mut io::sink())?;
    if skipped == padded {
        Ok(())
    } else {
        Err(invalid("PCM header: stream ended inside a chunk"))
    }
}

fn read_body(r: &mut impl Read, len: u32, max: usize) -> io::Result<Vec<u8>> {
    if len as usize > max {
        return Err(invalid(format!("PCM header: chunk of {len} bytes")));
    }
    let mut body = vec![0; len as usize];
    r.read_exact(&mut body)?;
    if len & 1 == 1 {
        r.read_exact(&mut [0])?;
    }
    Ok(body)
}

/// A length of 0 or `u32::MAX` means "unknown" in streamed files.
fn data_len(len: u32) -> Option<u64> {
    (len != 0 && len != u32::MAX).then_some(u64::from(len))
}

fn read_wav(r: &mut impl Read) -> io::Result<PcmSpec> {
    let mut spec: Option<PcmSpec> = None;
    loop {
        let (id, len) = chunk(r, false)?;
        match &id {
            b"fmt " => spec = Some(wav_fmt(&read_body(r, len, 256)?)?),
            b"data" => {
                let mut spec = spec.ok_or_else(|| invalid("WAV: data before fmt"))?;
                spec.data_len = data_len(len);
                return Ok(spec);
            }
            _ => skip(r, len)?,
        }
    }
}

fn wav_fmt(b: &[u8]) -> io::Result<PcmSpec> {
    if b.len() < 16 {
        return Err(invalid("WAV: short fmt chunk"));
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let mut tag = u16_at(0);
    if tag == 0xFFFE && b.len() >= 26 {
        // WAVE_FORMAT_EXTENSIBLE: the sub-format GUID starts with the real tag.
        tag = u16_at(24);
    }
    let float = match tag {
        1 => false,
        3 => true,
        other => return Err(invalid(format!("WAV: unsupported format tag {other:#x}"))),
    };
    let bits = u8::try_from(u16_at(14)).map_err(|_| invalid("WAV: bad sample size"))?;
    Ok(PcmSpec {
        rate: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        channels: u16_at(2),
        bits,
        big_endian: false,
        unsigned_8: true,
        float,
        data_len: None,
    })
}

fn read_aiff(r: &mut impl Read, aifc: bool) -> io::Result<PcmSpec> {
    let mut spec: Option<PcmSpec> = None;
    loop {
        let (id, len) = chunk(r, true)?;
        match &id {
            b"COMM" => spec = Some(aiff_comm(&read_body(r, len, 256)?, aifc)?),
            b"SSND" => {
                let mut spec = spec.ok_or_else(|| invalid("AIFF: SSND before COMM"))?;
                let mut h = [0; 8];
                r.read_exact(&mut h)?;
                let offset = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
                skip_exact(r, offset)?;
                spec.data_len = data_len(len)
                    .and_then(|l| l.checked_sub(8 + u64::from(offset)))
                    .filter(|&l| l > 0);
                return Ok(spec);
            }
            _ => skip(r, len)?,
        }
    }
}

fn skip_exact(r: &mut impl Read, n: u32) -> io::Result<()> {
    let skipped = io::copy(&mut r.take(n.into()), &mut io::sink())?;
    if skipped == u64::from(n) {
        Ok(())
    } else {
        Err(invalid("AIFF: stream ended in SSND offset"))
    }
}

fn aiff_comm(b: &[u8], aifc: bool) -> io::Result<PcmSpec> {
    if b.len() < 18 || (aifc && b.len() < 22) {
        return Err(invalid("AIFF: short COMM chunk"));
    }
    let channels = u16::from_be_bytes([b[0], b[1]]);
    let bits =
        u8::try_from(u16::from_be_bytes([b[6], b[7]])).map_err(|_| invalid("AIFF: bad sample size"))?;
    let rate = extended_to_u32(&b[8..18]).ok_or_else(|| invalid("AIFF: bad sample rate"))?;
    let (big_endian, float) = if aifc {
        match &b[18..22] {
            b"NONE" | b"twos" => (true, false),
            b"sowt" => (false, false),
            b"fl32" | b"FL32" => (true, true),
            other => {
                return Err(invalid(format!(
                    "AIFC: unsupported compression {}",
                    String::from_utf8_lossy(other)
                )));
            }
        }
    } else {
        (true, false)
    };
    // Sample sizes that are not a whole number of bytes are stored in the next whole byte.
    let bits = bits.div_ceil(8) * 8;
    Ok(PcmSpec {
        rate,
        channels,
        bits,
        big_endian,
        unsigned_8: false,
        float,
        data_len: None,
    })
}

/// An 80-bit IEEE 754 extended float (AIFF's sample rate) as a whole number.
fn extended_to_u32(b: &[u8]) -> Option<u32> {
    let exponent = i32::from(u16::from_be_bytes([b[0], b[1]]) & 0x7FFF) - 16383;
    let mantissa = u64::from_be_bytes(b[2..10].try_into().ok()?);
    if !(0..32).contains(&exponent) || b[0] & 0x80 != 0 {
        return None;
    }
    u32::try_from(mantissa >> (63 - exponent)).ok()
}

/// Appends `input` (whole frames) to `out` as `f32` samples times `gain`.
pub(super) fn convert(spec: &PcmSpec, input: &[u8], gain: f32, out: &mut Vec<f32>) {
    const S8: f32 = 1.0 / 128.0;
    const S16: f32 = 1.0 / 32_768.0;
    const S24: f32 = 1.0 / 8_388_608.0;
    const S32: f32 = 1.0 / 2_147_483_648.0;
    #[allow(clippy::cast_precision_loss)] // H15-56: 24/32-bit integers to f32 samples, by design
    let int = |v: i32, scale: f32| v as f32 * scale * gain;
    let be = spec.big_endian;
    match (spec.bits, spec.float) {
        (8, _) if spec.unsigned_8 => {
            out.extend(input.iter().map(|&s| int(i32::from(s) - 128, S8)));
        }
        (8, _) => out.extend(input.iter().map(|&s| int(i32::from(s.cast_signed()), S8))),
        (16, _) => out.extend(input.as_chunks::<2>().0.iter().map(|&s| {
            let v = if be {
                i16::from_be_bytes(s)
            } else {
                i16::from_le_bytes(s)
            };
            int(v.into(), S16)
        })),
        (24, _) => out.extend(input.as_chunks::<3>().0.iter().map(|&[a, b, c]| {
            let v = if be {
                i32::from_be_bytes([a, b, c, 0])
            } else {
                i32::from_le_bytes([0, a, b, c])
            };
            int(v >> 8, S24)
        })),
        (32, true) => out.extend(input.as_chunks::<4>().0.iter().map(|&s| {
            (if be {
                f32::from_be_bytes(s)
            } else {
                f32::from_le_bytes(s)
            }) * gain
        })),
        (32, false) => out.extend(input.as_chunks::<4>().0.iter().map(|&s| {
            let v = if be {
                i32::from_be_bytes(s)
            } else {
                i32::from_le_bytes(s)
            };
            int(v, S32)
        })),
        _ => {}
    }
}

//! The audio frontend microWakeWord models are trained on: TensorFlow Lite Micro's
//! `microfrontend` (window, fixed-point FFT, mel filterbank, noise reduction, PCAN gain control,
//! log scale), ported from its C sources (Apache-2.0) as `pymicro-features` 2.0.2 and `ESPHome`
//! configure it: 30 ms window, 10 ms step, 40 channels from 125 to 7500 Hz.
//!
//! The port keeps the C integer semantics (16-bit wrap-around, promotion rules) so the features
//! are bit for bit those of the reference; `tests/frontend.rs` checks that against fixtures.

/// Input rate in Hz.
pub const SAMPLE_RATE: u32 = 16_000;
/// Samples per feature step (10 ms).
pub const STEP: usize = 160;
/// Feature channels per step.
pub const CHANNELS: usize = 40;
/// One step's features, as the frontend outputs them (log-scaled energies).
pub type Features = [u16; CHANNELS];

/// Window length: 30 ms.
const WINDOW: usize = 480;
const FFT_SIZE: usize = 512;
const WINDOW_BITS: u32 = 12;
/// `MostSignificantBit32(FFT_SIZE)`.
const FFT_SIZE_MSB: u32 = FFT_SIZE.ilog2() + 1;
const FILTERBANK_BITS: u32 = 12;
const NOISE_REDUCTION_BITS: u32 = 14;
const SMOOTHING_BITS: u32 = 10;
const PCAN_SNR_BITS: u32 = 12;
const PCAN_OUTPUT_BITS: u32 = 6;
const WIDE_DYNAMIC_FUNCTION_BITS: u32 = 32;
const LOG_SCALE_SHIFT: u32 = 6;

/// Streaming frontend state: feed it [`STEP`] samples at a time.
#[derive(Debug, Clone)]
pub struct Frontend {
    window: Window,
    fft: Fft,
    filterbank: Filterbank,
    noise: NoiseReduction,
    pcan: Pcan,
    /// |X|² per bin (twice the spectrum's size, as in C: see `Filterbank::energy`).
    energy: Vec<i32>,
}

impl Default for Frontend {
    fn default() -> Self {
        Self::new()
    }
}

impl Frontend {
    #[must_use]
    pub fn new() -> Self {
        Self {
            window: Window::new(),
            fft: Fft::new(),
            filterbank: Filterbank::new(),
            noise: NoiseReduction::new(),
            pcan: Pcan::new(),
            energy: vec![0; 2 * (NCFFT + 1)],
        }
    }

    /// Feeds up to [`STEP`] samples; returns the features once a whole window has arrived (every
    /// step after the first 20 ms) and how many samples it took.
    pub fn process(&mut self, samples: &[i16]) -> (Option<Features>, usize) {
        let (ready, used) = self.window.push(samples);
        if !ready {
            return (None, used);
        }
        let input_shift = 15 - msb32(u32::from(self.window.max_abs.unsigned_abs()));
        let spectrum = self.fft.compute(&self.window.output, input_shift);
        self.filterbank.energy(spectrum, &mut self.energy);
        self.filterbank.accumulate(&self.energy);
        let mut signal = self.filterbank.sqrt(input_shift);
        self.noise.apply(&mut signal);
        self.pcan.apply(&mut signal, &self.noise.estimate);
        let correction_bits = FFT_SIZE_MSB - 1 - (FILTERBANK_BITS / 2);
        (Some(log_scale(&signal, correction_bits)), used)
    }

    /// Back to silence, as after `new`.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

/// `MostSignificantBit32`: 0 for 0, else the 1-based position of the highest set bit.
fn msb32(n: u32) -> u32 {
    32 - n.leading_zeros()
}

fn msb64(n: u64) -> u32 {
    64 - n.leading_zeros()
}

// ---------- Window ----------

#[derive(Debug, Clone)]
struct Window {
    coefficients: [i16; WINDOW],
    input: [i16; WINDOW],
    used: usize,
    output: [i16; WINDOW],
    max_abs: i16,
}

impl Window {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "the C code's float arithmetic, kept as is"
    )]
    fn new() -> Self {
        let mut coefficients = [0; WINDOW];
        let arg = std::f32::consts::PI * 2.0 / WINDOW as f32;
        for (i, c) in coefficients.iter_mut().enumerate() {
            let value = 0.5 - 0.5 * (arg * (i as f32 + 0.5)).cos();
            *c = (value * (1 << WINDOW_BITS) as f32 + 0.5).floor() as i16;
        }
        Self {
            coefficients,
            input: [0; WINDOW],
            used: 0,
            output: [0; WINDOW],
            max_abs: 0,
        }
    }

    /// Takes samples until the window is full; `true` when `output` holds a new window.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "C: the product >> 12 fits 16 bits"
    )]
    fn push(&mut self, samples: &[i16]) -> (bool, usize) {
        let take = (WINDOW - self.used).min(samples.len());
        self.input[self.used..self.used + take].copy_from_slice(&samples[..take]);
        self.used += take;
        if self.used < WINDOW {
            return (false, take);
        }
        let mut max_abs: i16 = 0;
        for ((out, &x), &c) in self.output.iter_mut().zip(&self.input).zip(&self.coefficients) {
            let value = ((i32::from(x) * i32::from(c)) >> WINDOW_BITS) as i16;
            *out = value;
            let magnitude = if value < 0 { value.wrapping_neg() } else { value };
            if magnitude > max_abs {
                max_abs = magnitude;
            }
        }
        self.input.copy_within(STEP.., 0);
        self.used -= STEP;
        self.max_abs = max_abs;
        (true, take)
    }
}

// ---------- FFT: kissfft, FIXED_POINT 16, real input ----------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Cpx {
    r: i16,
    i: i16,
}

/// `sround(x)`: rounds a Q30 product back to Q15, wrapping like the C cast.
#[allow(clippy::cast_possible_truncation, reason = "C: (kiss_fft_scalar) cast")]
fn sround(x: i32) -> i16 {
    (x.wrapping_add(1 << 14) >> 15) as i16
}

fn smul(a: i16, b: i16) -> i32 {
    i32::from(a) * i32::from(b)
}

fn c_mul(a: Cpx, b: Cpx) -> Cpx {
    Cpx {
        r: sround(smul(a.r, b.r).wrapping_sub(smul(a.i, b.i))),
        i: sround(smul(a.r, b.i).wrapping_add(smul(a.i, b.r))),
    }
}

/// `C_FIXDIV(c, div)`: scales down by `div` to keep the fixed-point FFT from overflowing.
fn c_fixdiv(c: Cpx, div: i16) -> Cpx {
    let k = i16::MAX / div;
    Cpx {
        r: sround(smul(c.r, k)),
        i: sround(smul(c.i, k)),
    }
}

fn c_add(a: Cpx, b: Cpx) -> Cpx {
    Cpx {
        r: a.r.wrapping_add(b.r),
        i: a.i.wrapping_add(b.i),
    }
}

fn c_sub(a: Cpx, b: Cpx) -> Cpx {
    Cpx {
        r: a.r.wrapping_sub(b.r),
        i: a.i.wrapping_sub(b.i),
    }
}

/// `floor(.5 + 32767 * cos(phase))`, as kissfft builds its twiddles.
#[allow(clippy::cast_possible_truncation, reason = "within ±32767 by construction")]
fn twiddle(phase: f64) -> Cpx {
    let scale = f64::from(i16::MAX);
    Cpx {
        r: (0.5 + scale * phase.cos()).floor() as i16,
        i: (0.5 + scale * phase.sin()).floor() as i16,
    }
}

/// The complex FFT of half the real length (256 points: four radix-4 stages).
const NCFFT: usize = FFT_SIZE / 2;

#[derive(Debug, Clone)]
struct Fft {
    twiddles: Vec<Cpx>,
    /// (radix, remaining length) per stage, as `kf_factor` makes them.
    factors: Vec<(usize, usize)>,
    super_twiddles: Vec<Cpx>,
    input: [i16; FFT_SIZE],
    /// The real input as complex pairs.
    packed: Vec<Cpx>,
    tmp: Vec<Cpx>,
    output: Vec<Cpx>,
}

impl Fft {
    #[allow(clippy::cast_precision_loss, reason = "small integers")]
    fn new() -> Self {
        let pi = std::f64::consts::PI;
        let twiddles = (0..NCFFT)
            .map(|i| twiddle(-2.0 * pi * i as f64 / NCFFT as f64))
            .collect();
        let super_twiddles = (0..NCFFT / 2)
            .map(|i| twiddle(-pi * ((i + 1) as f64 / NCFFT as f64 + 0.5)))
            .collect();
        Self {
            twiddles,
            factors: factor(NCFFT),
            super_twiddles,
            input: [0; FFT_SIZE],
            packed: vec![Cpx::default(); NCFFT],
            tmp: vec![Cpx::default(); NCFFT],
            output: vec![Cpx::default(); NCFFT + 1],
        }
    }

    /// `FftCompute`: scales the window up by `shift`, zero-pads and runs the real FFT.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "C casts"
    )]
    fn compute(&mut self, window: &[i16; WINDOW], shift: u32) -> &[Cpx] {
        for (dst, &x) in self.input.iter_mut().zip(window) {
            *dst = (u32::from(x.cast_unsigned()) << shift) as u16 as i16;
        }
        self.input[WINDOW..].fill(0);
        for (p, &[r, i]) in self.packed.iter_mut().zip(self.input.as_chunks::<2>().0) {
            *p = Cpx { r, i };
        }
        kf_work(&mut self.tmp, &self.packed, 0, 1, &self.factors, &self.twiddles);
        self.split();
        &self.output
    }

    #[allow(clippy::similar_names, reason = "kissfft's names (fpk, fpnk)")]
    /// `kiss_fftr`'s second half: turns the packed complex FFT into the real spectrum.
    #[allow(clippy::cast_possible_truncation, reason = "C: HALF_OF into int16")]
    fn split(&mut self) {
        let tdc = c_fixdiv(self.tmp[0], 2);
        self.output[0] = Cpx {
            r: tdc.r.wrapping_add(tdc.i),
            i: 0,
        };
        self.output[NCFFT] = Cpx {
            r: tdc.r.wrapping_sub(tdc.i),
            i: 0,
        };
        let half = |a: i16, b: i16| ((i32::from(a) + i32::from(b)) >> 1) as i16;
        let half_sub = |a: i16, b: i16| ((i32::from(a) - i32::from(b)) >> 1) as i16;
        for k in 1..=NCFFT / 2 {
            let fpk = c_fixdiv(self.tmp[k], 2);
            let mirror = self.tmp[NCFFT - k];
            let fpnk = c_fixdiv(
                Cpx {
                    r: mirror.r,
                    i: mirror.i.wrapping_neg(),
                },
                2,
            );
            let f1k = c_add(fpk, fpnk);
            let f2k = c_sub(fpk, fpnk);
            let tw = c_mul(f2k, self.super_twiddles[k - 1]);
            self.output[k] = Cpx {
                r: half(f1k.r, tw.r),
                i: half(f1k.i, tw.i),
            };
            self.output[NCFFT - k] = Cpx {
                r: half_sub(f1k.r, tw.r),
                i: half_sub(tw.i, f1k.i),
            };
        }
    }
}

/// `kf_factor`: powers of 4 first, then 2, then odd primes.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "small integers"
)]
fn factor(mut n: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut p = 4;
    let floor_sqrt = (n as f64).sqrt().floor() as usize;
    loop {
        while !n.is_multiple_of(p) {
            p = match p {
                4 => 2,
                2 => 3,
                _ => p + 2,
            };
            if p > floor_sqrt {
                p = n;
            }
        }
        n /= p;
        out.push((p, n));
        if n <= 1 {
            return out;
        }
    }
}

/// `kf_work`: the recursive decimation in time (`fout` gets `p * m` outputs).
fn kf_work(fout: &mut [Cpx], f: &[Cpx], f_at: usize, fstride: usize, factors: &[(usize, usize)], tw: &[Cpx]) {
    let (p, m) = factors[0];
    if m == 1 {
        for (k, out) in fout.iter_mut().take(p).enumerate() {
            *out = f[f_at + k * fstride];
        }
    } else {
        for k in 0..p {
            kf_work(
                &mut fout[k * m..(k + 1) * m],
                f,
                f_at + k * fstride,
                fstride * p,
                &factors[1..],
                tw,
            );
        }
    }
    match p {
        4 => kf_bfly4(fout, fstride, tw, m),
        _ => unreachable!("the 256-point FFT has only radix-4 stages"),
    }
}

#[allow(clippy::many_single_char_names, reason = "kissfft's names")]
fn kf_bfly4(fout: &mut [Cpx], fstride: usize, tw: &[Cpx], m: usize) {
    for k in 0..m {
        let a = c_fixdiv(fout[k], 4);
        let b = c_fixdiv(fout[k + m], 4);
        let c = c_fixdiv(fout[k + 2 * m], 4);
        let d = c_fixdiv(fout[k + 3 * m], 4);
        let s0 = c_mul(b, tw[k * fstride]);
        let s1 = c_mul(c, tw[k * fstride * 2]);
        let s2 = c_mul(d, tw[k * fstride * 3]);
        let s5 = c_sub(a, s1);
        let a1 = c_add(a, s1);
        let s3 = c_add(s0, s2);
        let s4 = c_sub(s0, s2);
        fout[k + 2 * m] = c_sub(a1, s3);
        fout[k] = c_add(a1, s3);
        fout[k + m] = Cpx {
            r: s5.r.wrapping_add(s4.i),
            i: s5.i.wrapping_sub(s4.r),
        };
        fout[k + 3 * m] = Cpx {
            r: s5.r.wrapping_sub(s4.i),
            i: s5.i.wrapping_add(s4.r),
        };
    }
}

// ---------- Mel filterbank ----------

const CHANNELS_PLUS_1: usize = CHANNELS + 1;
#[cfg(test)]
const INDEX_ALIGNMENT: usize = 2;
#[cfg(test)]
const CHANNEL_BLOCK: usize = 4;

#[derive(Debug, Clone)]
struct Filterbank {
    start: usize,
    end: usize,
    frequency_starts: [usize; CHANNELS_PLUS_1],
    weight_starts: [usize; CHANNELS_PLUS_1],
    widths: [usize; CHANNELS_PLUS_1],
    weights: Vec<i16>,
    unweights: Vec<i16>,
    work: [u64; CHANNELS_PLUS_1],
}

#[cfg(test)]
fn freq_to_mel(freq: f32) -> f32 {
    1127.0 * (freq / 700.0).ln_1p()
}

impl Filterbank {
    /// The layout and weights from [`crate::filterbank_tables`] (see there why they're fixed).
    fn new() -> Self {
        use crate::filterbank_tables as t;
        Self {
            start: t::START,
            end: t::END,
            frequency_starts: t::FREQUENCY_STARTS,
            weight_starts: t::WEIGHT_STARTS,
            widths: t::WIDTHS,
            weights: t::WEIGHTS.to_vec(),
            unweights: t::UNWEIGHTS.to_vec(),
            work: [0; CHANNELS_PLUS_1],
        }
    }

    /// `FilterbankPopulateState`, as the C code computes it (what the tables come from).
    #[cfg(test)]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "FilterbankPopulateState's float arithmetic, kept as is"
    )]
    fn computed() -> Self {
        let (low, high) = (125.0_f32, 7500.0_f32);
        let spectrum_size = FFT_SIZE / 2 + 1;
        let mel_low = freq_to_mel(low);
        let spacing = (freq_to_mel(high) - mel_low) / CHANNELS_PLUS_1 as f32;
        let centers: Vec<f32> = (0..CHANNELS_PLUS_1)
            .map(|i| mel_low + spacing * (i + 1) as f32)
            .collect();
        let hz_per_sbin = 0.5 * SAMPLE_RATE as f32 / (spectrum_size as f32 - 1.0);
        let start = (1.5 + low / hz_per_sbin) as usize;
        let mut fb = Self {
            start,
            end: 0,
            frequency_starts: [0; CHANNELS_PLUS_1],
            weight_starts: [0; CHANNELS_PLUS_1],
            widths: [0; CHANNELS_PLUS_1],
            weights: Vec::new(),
            unweights: Vec::new(),
            work: [0; CHANNELS_PLUS_1],
        };
        let (actual_starts, actual_widths) = fb.layout(&centers, hz_per_sbin);
        fb.fill_weights(&centers, mel_low, hz_per_sbin, &actual_starts, &actual_widths);
        fb
    }

    /// First pass: which FFT bins feed each channel, padded to blocks of 4.
    #[cfg(test)]
    #[allow(clippy::cast_precision_loss, reason = "C: int * float")]
    fn layout(&mut self, centers: &[f32], hz_per_sbin: f32) -> (Vec<usize>, Vec<usize>) {
        let mut actual_starts = vec![0; CHANNELS_PLUS_1];
        let mut actual_widths = vec![0; CHANNELS_PLUS_1];
        let mut chan_start = self.start;
        let mut weight_start = 0;
        let mut needs_zeros = false;
        for chan in 0..CHANNELS_PLUS_1 {
            let mut freq = chan_start;
            while freq_to_mel(freq as f32 * hz_per_sbin) <= centers[chan] {
                freq += 1;
            }
            let width = freq - chan_start;
            actual_starts[chan] = chan_start;
            actual_widths[chan] = width;
            if width == 0 {
                self.frequency_starts[chan] = 0;
                self.weight_starts[chan] = 0;
                self.widths[chan] = CHANNEL_BLOCK;
                if !needs_zeros {
                    needs_zeros = true;
                    for s in &mut self.weight_starts[..chan] {
                        *s += CHANNEL_BLOCK;
                    }
                    weight_start += CHANNEL_BLOCK;
                }
            } else {
                let aligned_start = (chan_start / INDEX_ALIGNMENT) * INDEX_ALIGNMENT;
                let aligned_width = chan_start - aligned_start + width;
                let padded = ((aligned_width - 1) / CHANNEL_BLOCK + 1) * CHANNEL_BLOCK;
                self.frequency_starts[chan] = aligned_start;
                self.weight_starts[chan] = weight_start;
                self.widths[chan] = padded;
                weight_start += padded;
            }
            chan_start = freq;
        }
        self.weights = vec![0; weight_start];
        self.unweights = vec![0; weight_start];
        (actual_starts, actual_widths)
    }

    /// Second pass: the triangular weights, quantized to 12 bits.
    #[cfg(test)]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "QuantizeFilterbankWeights' float to int16"
    )]
    fn fill_weights(
        &mut self,
        centers: &[f32],
        mel_low: f32,
        hz_per_sbin: f32,
        starts: &[usize],
        widths: &[usize],
    ) {
        let one = (1 << FILTERBANK_BITS) as f32;
        for chan in 0..CHANNELS_PLUS_1 {
            let mut frequency = starts[chan];
            let offset = frequency - self.frequency_starts[chan];
            let weight_start = self.weight_starts[chan];
            let denom = if chan == 0 { mel_low } else { centers[chan - 1] };
            for j in 0..widths[chan] {
                let weight =
                    (centers[chan] - freq_to_mel(frequency as f32 * hz_per_sbin)) / (centers[chan] - denom);
                let at = weight_start + offset + j;
                self.weights[at] = (weight * one + 0.5).floor() as i16;
                self.unweights[at] = ((1.0 - weight) * one + 0.5).floor() as i16;
                frequency += 1;
            }
            self.end = self.end.max(frequency);
        }
    }

    /// `FilterbankConvertFftComplexToEnergy`: |X|² per bin, as 32-bit values (the C code stores
    /// them as `int32_t` in the FFT's output buffer, which is twice the spectrum's size: padded
    /// channel blocks read past the spectrum, always with weight 0).
    #[allow(clippy::cast_possible_wrap, reason = "C: uint32 stored as int32")]
    fn energy(&self, spectrum: &[Cpx], energy: &mut [i32]) {
        for i in self.start..self.end {
            let (re, im) = (i32::from(spectrum[i].r), i32::from(spectrum[i].i));
            energy[i] = (re * re).cast_unsigned().wrapping_add((im * im).cast_unsigned()) as i32;
        }
    }

    /// `FilterbankAccumulateChannels`.
    #[allow(clippy::cast_sign_loss, reason = "C: int to uint64 conversions")]
    fn accumulate(&mut self, energy: &[i32]) {
        let mut weight_acc: u64 = 0;
        let mut unweight_acc: u64 = 0;
        for chan in 0..CHANNELS_PLUS_1 {
            let magnitudes = &energy[self.frequency_starts[chan]..];
            let ws = self.weight_starts[chan];
            let weights = self.weights[ws..].iter().zip(&self.unweights[ws..]);
            for (&m, (&w, &u)) in magnitudes.iter().zip(weights).take(self.widths[chan]) {
                let magnitude = i64::from(m) as u64;
                weight_acc = weight_acc.wrapping_add((i64::from(w) as u64).wrapping_mul(magnitude));
                unweight_acc = unweight_acc.wrapping_add((i64::from(u) as u64).wrapping_mul(magnitude));
            }
            self.work[chan] = weight_acc;
            weight_acc = unweight_acc;
            unweight_acc = 0;
        }
    }

    /// `FilterbankSqrt`: the channel magnitudes, scaled back down by the FFT's input shift.
    fn sqrt(&self, shift: u32) -> [u32; CHANNELS] {
        let mut out = [0; CHANNELS];
        for (o, &w) in out.iter_mut().zip(&self.work[1..]) {
            *o = sqrt64(w) >> shift;
        }
        out
    }
}

fn sqrt32(mut num: u32) -> u16 {
    if num == 0 {
        return 0;
    }
    let mut res: u32 = 0;
    let max_bit = (32 - msb32(num)) | 1;
    let mut bit: u32 = 1 << (31 - max_bit);
    let iterations = (31 - max_bit) / 2 + 1;
    for _ in 0..iterations {
        if num >= res + bit {
            num -= res + bit;
            res = (res >> 1) + bit;
        } else {
            res >>= 1;
        }
        bit >>= 2;
    }
    if num > res && res != 0xFFFF {
        res += 1;
    }
    #[allow(clippy::cast_possible_truncation, reason = "C returns uint16_t")]
    {
        res as u16
    }
}

#[allow(clippy::cast_possible_truncation, reason = "C: (uint32_t) results")]
fn sqrt64(mut num: u64) -> u32 {
    if num >> 32 == 0 {
        return u32::from(sqrt32(num as u32));
    }
    let mut res: u64 = 0;
    let max_bit = (64 - msb64(num)) | 1;
    let mut bit: u64 = 1 << (63 - max_bit);
    let iterations = (63 - max_bit) / 2 + 1;
    for _ in 0..iterations {
        if num >= res + bit {
            num -= res + bit;
            res = (res >> 1) + bit;
        } else {
            res >>= 1;
        }
        bit >>= 2;
    }
    if num > res && res != 0xFFFF_FFFF {
        res += 1;
    }
    res as u32
}

// ---------- Noise reduction ----------

#[derive(Debug, Clone)]
struct NoiseReduction {
    even_smoothing: u32,
    odd_smoothing: u32,
    min_signal_remaining: u32,
    estimate: [u32; CHANNELS],
}

impl NoiseReduction {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "C: float to uint32"
    )]
    fn new() -> Self {
        let q = |v: f32| (v * f32::from(1_u16 << NOISE_REDUCTION_BITS)) as u32;
        Self {
            even_smoothing: q(0.025),
            odd_smoothing: q(0.06),
            min_signal_remaining: q(0.05),
            estimate: [0; CHANNELS],
        }
    }

    #[allow(clippy::cast_possible_truncation, reason = "C: uint64 >> 14 into uint32")]
    fn apply(&mut self, signal: &mut [u32; CHANNELS]) {
        for (i, s) in signal.iter_mut().enumerate() {
            let smoothing = if i & 1 == 0 {
                self.even_smoothing
            } else {
                self.odd_smoothing
            };
            let one_minus = (1 << NOISE_REDUCTION_BITS) - smoothing;
            let scaled_up = *s << SMOOTHING_BITS;
            let mut estimate = ((u64::from(scaled_up) * u64::from(smoothing)
                + u64::from(self.estimate[i]) * u64::from(one_minus))
                >> NOISE_REDUCTION_BITS) as u32;
            self.estimate[i] = estimate;
            estimate = estimate.min(scaled_up);
            let floor =
                ((u64::from(*s) * u64::from(self.min_signal_remaining)) >> NOISE_REDUCTION_BITS) as u32;
            let subtracted = (scaled_up - estimate) >> SMOOTHING_BITS;
            *s = subtracted.max(floor);
        }
    }
}

// ---------- PCAN gain control ----------

const GAIN_LUT_SIZE: usize = 4 * WIDE_DYNAMIC_FUNCTION_BITS as usize - 3;

#[derive(Debug, Clone)]
struct Pcan {
    lut: [i16; GAIN_LUT_SIZE],
    snr_shift: u32,
}

impl Pcan {
    fn new() -> Self {
        let correction_bits = FFT_SIZE_MSB - 1 - (FILTERBANK_BITS / 2);
        let input_bits = SMOOTHING_BITS - correction_bits;
        let mut lut = [0_i16; GAIN_LUT_SIZE];
        lut[0] = pcan_gain(input_bits, 0);
        lut[1] = pcan_gain(input_bits, 1);
        for interval in 2..=WIDE_DYNAMIC_FUNCTION_BITS {
            let x0: u32 = 1 << (interval - 1);
            let x1 = x0 + (x0 >> 1);
            let x2 = if interval == WIDE_DYNAMIC_FUNCTION_BITS {
                x0 + (x0 - 1)
            } else {
                2 * x0
            };
            let y0 = pcan_gain(input_bits, x0);
            let diff1 = i32::from(pcan_gain(input_bits, x1)) - i32::from(y0);
            let diff2 = i32::from(pcan_gain(input_bits, x2)) - i32::from(y0);
            let a1 = 4 * diff1 - diff2;
            let a2 = diff2 - a1;
            let base = 4 * interval as usize - 6;
            #[allow(clippy::cast_possible_truncation, reason = "C: (int16_t) casts")]
            {
                lut[base] = y0;
                lut[base + 1] = a1 as i16;
                lut[base + 2] = a2 as i16;
            }
        }
        Self {
            lut,
            snr_shift: 21 - correction_bits - PCAN_SNR_BITS,
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "C conversions"
    )]
    fn apply(&self, signal: &mut [u32; CHANNELS], noise: &[u32; CHANNELS]) {
        for (s, &n) in signal.iter_mut().zip(noise) {
            let gain = i32::from(self.wide_dynamic(n)) as u32;
            let snr = ((u64::from(*s) * u64::from(gain)) >> self.snr_shift) as u32;
            *s = pcan_shrink(snr);
        }
    }

    /// `WideDynamicFunction`: the gain for a noise estimate, interpolated from the table.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "C conversions"
    )]
    fn wide_dynamic(&self, x: u32) -> i16 {
        if x <= 2 {
            return self.lut[x as usize];
        }
        let interval = msb32(x);
        let lut = &self.lut[4 * interval as usize - 6..];
        let frac = (if interval < 11 {
            x << (11 - interval)
        } else {
            x >> (interval - 11)
        } & 0x3FF) as i32;
        let mut result = (i32::from(lut[2]) * frac) >> 5;
        result = result.wrapping_add((i32::from(lut[1]).cast_unsigned() << 5) as i32);
        result = result.wrapping_mul(frac);
        result = result.wrapping_add(1 << 14) >> 15;
        result += i32::from(lut[0]);
        result as i16
    }
}

/// `PcanGainLookupFunction`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "the C code's float arithmetic"
)]
fn pcan_gain(input_bits: u32, x: u32) -> i16 {
    let x = x as f32 / (1_u32 << input_bits) as f32;
    let gain = (1_u32 << 21) as f32 * (x + 80.0).powf(-0.95);
    if gain > f32::from(i16::MAX) {
        return i16::MAX;
    }
    (gain + 0.5) as i16
}

fn pcan_shrink(x: u32) -> u32 {
    if x < (2 << PCAN_SNR_BITS) {
        x.wrapping_mul(x) >> (2 + 2 * PCAN_SNR_BITS - PCAN_OUTPUT_BITS)
    } else {
        (x >> (PCAN_SNR_BITS - PCAN_OUTPUT_BITS)).wrapping_sub(1 << PCAN_OUTPUT_BITS)
    }
}

// ---------- Log scale ----------

const LOG_SCALE_LOG2: u32 = 16;
const LOG_SEGMENTS_LOG2: u32 = 7;
const LOG_COEFF: u64 = 45426;

#[rustfmt::skip]
const LOG_LUT: [u16; 130] = [
    0, 224, 442, 654, 861, 1063, 1259, 1450, 1636, 1817, 1992, 2163, 2329, 2490, 2646, 2797, 2944,
    3087, 3224, 3358, 3487, 3611, 3732, 3848, 3960, 4068, 4172, 4272, 4368, 4460, 4549, 4633, 4714,
    4791, 4864, 4934, 5001, 5063, 5123, 5178, 5231, 5280, 5326, 5368, 5408, 5444, 5477, 5507, 5533,
    5557, 5578, 5595, 5610, 5622, 5631, 5637, 5640, 5641, 5638, 5633, 5626, 5615, 5602, 5586, 5568,
    5547, 5524, 5498, 5470, 5439, 5406, 5370, 5332, 5291, 5249, 5203, 5156, 5106, 5054, 5000, 4944,
    4885, 4825, 4762, 4697, 4630, 4561, 4490, 4416, 4341, 4264, 4184, 4103, 4020, 3935, 3848, 3759,
    3668, 3575, 3481, 3384, 3286, 3186, 3084, 2981, 2875, 2768, 2659, 2549, 2437, 2323, 2207, 2090,
    1971, 1851, 1729, 1605, 1480, 1353, 1224, 1094, 963, 830, 695, 559, 421, 282, 142, 0, 0,
];

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "C conversions"
)]
fn log2_fraction(x: u32, log2x: u32) -> u32 {
    let mut frac = (i64::from(x) - (1_i64 << log2x)) as i32;
    if log2x < LOG_SCALE_LOG2 {
        frac <<= LOG_SCALE_LOG2 - log2x;
    } else {
        frac >>= log2x - LOG_SCALE_LOG2;
    }
    let base_seg = (frac as u32) >> (LOG_SCALE_LOG2 - LOG_SEGMENTS_LOG2);
    let seg_unit = (1_u32 << LOG_SCALE_LOG2) >> LOG_SEGMENTS_LOG2;
    let c0 = i32::from(LOG_LUT[base_seg as usize]);
    let c1 = i32::from(LOG_LUT[base_seg as usize + 1]);
    let seg_base = (seg_unit * base_seg) as i32;
    let rel_pos = ((c1 - c0) * (frac - seg_base)) >> LOG_SCALE_LOG2;
    (frac + c0 + rel_pos) as u32
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::similar_names,
    reason = "C: uint64 >> 16 into uint32; log2/loge as in C"
)]
fn log(x: u32, scale_shift: u32) -> u32 {
    let integer = msb32(x) - 1;
    let fraction = log2_fraction(x, integer);
    let log2 = (integer << LOG_SCALE_LOG2).wrapping_add(fraction);
    let round: u32 = (1 << LOG_SCALE_LOG2) / 2;
    let loge = ((LOG_COEFF * u64::from(log2) + u64::from(round)) >> LOG_SCALE_LOG2) as u32;
    ((loge << scale_shift).wrapping_add(round)) >> LOG_SCALE_LOG2
}

/// `LogScaleApply`.
#[allow(clippy::cast_possible_truncation, reason = "clamped to u16")]
fn log_scale(signal: &[u32; CHANNELS], correction_bits: u32) -> Features {
    let mut out = [0; CHANNELS];
    for (o, &s) in out.iter_mut().zip(signal) {
        let value = s << correction_bits;
        let value = if value > 1 { log(value, LOG_SCALE_SHIFT) } else { 0 };
        *o = value.min(u32::from(u16::MAX)) as u16;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fft_has_four_radix_4_stages() {
        assert_eq!(factor(256), vec![(4, 64), (4, 16), (4, 4), (4, 1)]);
    }

    #[test]
    fn silence_gives_zero_features() {
        let mut f = Frontend::new();
        let silence = [0_i16; STEP];
        assert_eq!(f.process(&silence), (None, STEP));
        assert_eq!(f.process(&silence), (None, STEP));
        let (features, used) = f.process(&silence);
        assert_eq!(used, STEP);
        assert_eq!(features, Some([0; CHANNELS]));
    }

    /// The fixed filterbank tables are what the C code computes with glibc on `x86_64`.
    #[test]
    #[cfg(all(target_arch = "x86_64", target_env = "gnu"))]
    fn filterbank_tables_are_the_computed_ones() {
        let (fixed, computed) = (Filterbank::new(), Filterbank::computed());
        assert_eq!((fixed.start, fixed.end), (computed.start, computed.end));
        assert_eq!(fixed.frequency_starts, computed.frequency_starts);
        assert_eq!(fixed.weight_starts, computed.weight_starts);
        assert_eq!(fixed.widths, computed.widths);
        assert_eq!(fixed.weights, computed.weights);
        assert_eq!(fixed.unweights, computed.unweights);
    }

    #[test]
    fn integer_square_roots() {
        assert_eq!(sqrt32(0), 0);
        assert_eq!(sqrt32(16), 4);
        assert_eq!(sqrt32(17), 4);
        assert_eq!(sqrt32(20), 4);
        assert_eq!(sqrt32(21), 5, "rounds");
        assert_eq!(sqrt64(1 << 40), 1 << 20);
    }
}

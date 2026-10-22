//! Resampling CPU per quality (H15-80): converts synthetic 44.1 kHz stereo to 48 kHz in the
//! player's 10 ms blocks, into a sink that drops the frames, and reports the process CPU per
//! second of audio (`/proc/self/stat`, so it runs on the board without `perf`).
//!
//! ```sh
//! cargo zigbuild --release -p player --example resample_probe --target armv7-unknown-linux-musleabihf
//! ./resample_probe 60        # seconds of audio per quality
//! ```

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use player::dsp::{Controls, CrossfeedSettings, DspDevice, FILTERS, Filter, FilterKind, Settings};
use player::output::Device;
use player::output::resample::{Quality, ResampleDevice};

struct Null;

impl Device for Null {
    fn open(&mut self, _: u32, _: u16) -> io::Result<()> {
        Ok(())
    }
    fn write(&mut self, _: &[u8]) -> io::Result<()> {
        Ok(())
    }
    fn write_frames(&mut self, samples: &mut [f32]) -> io::Result<()> {
        std::hint::black_box(samples);
        Ok(())
    }
    fn delay_frames(&self) -> u32 {
        0
    }
    fn close(&mut self) {}
    fn is_open(&self) -> bool {
        true
    }
}

/// CPU time (user + system) of this process, from `/proc/self/stat` (10 ms ticks).
fn cpu_time() -> Duration {
    let stat = std::fs::read_to_string("/proc/self/stat").expect("/proc/self/stat");
    let rest = &stat[stat.rfind(')').expect("stat format") + 2..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    Duration::from_millis(ticks * 10)
}

#[allow(clippy::cast_precision_loss, reason = "H15-80: test signal and report")]
fn main() {
    let seconds: u32 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(30);
    // Two tones and noise-ish content, so the FFT paths do real work.
    let block: Vec<f32> = (0..441 * 100)
        .flat_map(|i| {
            let t = i as f32 / 44_100.0;
            let l = (t * 440.0 * std::f32::consts::TAU).sin() * 0.3
                + (t * 9_000.0 * std::f32::consts::TAU).sin() * 0.1;
            let r = (t * 660.0 * std::f32::consts::TAU).sin() * 0.3
                + (t * 13_000.0 * std::f32::consts::TAU).sin() * 0.1;
            [l, r]
        })
        .collect();
    // `chain`: the DSP alone at 44.1 kHz against resampling + DSP at 48 kHz, as in the player.
    if std::env::args().nth(2).as_deref() == Some("chain") {
        chain(seconds, &block);
        return;
    }
    for quality in [Quality::Fast, Quality::Balanced, Quality::High] {
        let mut dev = ResampleDevice::new(Null, Arc::new(|| Some(48_000)), quality);
        report(quality.as_str(), &mut dev, seconds, &block);
    }
}

/// Every filter active and crossfeed on, like the board's sound settings.
fn dsp_settings() -> Settings {
    let mut filters = [Filter::default(); FILTERS];
    for (i, f) in filters.iter_mut().enumerate() {
        #[allow(clippy::cast_precision_loss, reason = "H15-80: filter index")]
        let i = i as f64;
        *f = Filter {
            kind: FilterKind::Peak,
            frequency: 60.0 * 1.6f64.powf(i),
            q: 1.0,
            gain_db: 2.0,
        };
    }
    Settings {
        filters,
        crossfeed: Some(CrossfeedSettings::default()),
        preamp_db: -3.0,
        ..Settings::default()
    }
}

fn chain(seconds: u32, block: &[f32]) {
    let controls = Arc::new(Controls::new(dsp_settings()));
    let mut dsp = DspDevice::new(Null, Some(Arc::clone(&controls)));
    report("dsp@44.1", &mut dsp, seconds, block);
    for quality in [Quality::Fast, Quality::Balanced, Quality::High] {
        let mut dev = ResampleDevice::new(
            DspDevice::new(Null, Some(Arc::clone(&controls))),
            Arc::new(|| Some(48_000)),
            quality,
        );
        report(&format!("{}+dsp@48", quality.as_str()), &mut dev, seconds, block);
    }
}

#[allow(clippy::cast_precision_loss, reason = "H15-80: report")]
fn report(name: &str, dev: &mut dyn Device, seconds: u32, block: &[f32]) {
    dev.open(44_100, 2).unwrap();
    let chunks = seconds * 100;
    let (cpu0, wall0) = (cpu_time(), Instant::now());
    let mut buf = vec![0.0f32; 441 * 2];
    for c in 0..chunks as usize {
        let at = (c % 100) * 441 * 2;
        buf.copy_from_slice(&block[at..at + 441 * 2]);
        dev.write_frames(&mut buf).unwrap();
    }
    let cpu = cpu_time().saturating_sub(cpu0);
    let wall = wall0.elapsed();
    println!(
        "{name:16} {seconds} s audio: CPU {:.2} s = {:.2} % of one core in real time (wall {:.2} s), delay {} frames",
        cpu.as_secs_f64(),
        cpu.as_secs_f64() / f64::from(seconds) * 100.0,
        wall.as_secs_f64(),
        dev.delay_frames(),
    );
}

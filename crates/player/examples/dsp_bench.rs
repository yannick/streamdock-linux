//! H15-67: CPU-only DSP benchmark, no audio/PipeWire connection.
//! `dsp_bench 44_100 300` processes 300 seconds of stereo test audio as fast as possible.
//! Linux `/proc` CPU ticks are 100 Hz on the Luckfox (also used by `decode_bench`).
#[path = "dsp_bench/ablation.rs"]
mod ablation;
use player::dsp::{Compiled, CrossfeedSettings, Filter, FilterKind, Processor, Settings};
use std::{hint::black_box, time::Instant};

fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").expect("Linux /proc/self/stat");
    let fields: Vec<_> = stat[stat.rfind(')').unwrap() + 2..].split_whitespace().collect();
    let ticks = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    #[allow(
        clippy::cast_precision_loss,
        reason = "H15-67: CPU ticks converted for a benchmark report"
    )]
    {
        ticks as f64 / 100.0
    }
}
#[allow(clippy::approx_constant, reason = "H15-67: AutoEq measured Q=3.14, not pi")]
fn settings() -> Settings {
    let bands = [
        (120.0, 0.7, 3.0),
        (1000.0, 0.7, -2.0),
        (8000.0, 0.7, 2.0),
        (105.0, 0.7, -4.2),
        (143.0, 1.1, -5.2),
        (2289.0, 1.57, 6.1),
        (56.0, 1.19, 1.2),
        (5144.0, 6.0, -3.2),
        (407.0, 3.14, 1.7),
        (6715.0, 5.99, 3.0),
        (1007.0, 3.41, 1.0),
        (576.0, 3.55, -1.2),
        (10000.0, 0.7, -1.0),
    ];
    Settings {
        filters: std::array::from_fn(|i| Filter {
            kind: match i {
                0 | 3 => FilterKind::LowShelf,
                2 | 12 => FilterKind::HighShelf,
                _ => FilterKind::Peak,
            },
            frequency: bands[i].0,
            q: bands[i].1,
            gain_db: bands[i].2,
        }),
        crossfeed: Some(CrossfeedSettings::default()),
        ..Settings::default()
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if !(3..=5).contains(&args.len()) {
        return Err("usage: dsp_bench RATE SECONDS [full|biquads|crossfeed|limiter|checks] [flat]".into());
    }
    let rate: u32 = args[1].parse()?;
    let seconds: u32 = args[2].parse()?;
    if !(1..=3600).contains(&seconds) {
        return Err("seconds must be 1..3600".into());
    }
    let mode = args.get(3).map_or("full", String::as_str);
    let stage = match mode {
        "full" => None,
        "biquads" => Some(0),
        "crossfeed" => Some(1),
        "limiter" => Some(2),
        "checks" => Some(3),
        _ => return Err("unknown stage".into()),
    };
    let flat = args.get(4).is_some_and(|value| value == "flat");
    let mut settings = settings();
    if flat {
        for filter in &mut settings.filters {
            filter.gain_db = 0.0;
        }
    }
    let config = Compiled::new(rate, 1, settings)?;
    let mut ablation = stage
        .map(|stage| ablation::Ablation::new(rate, &settings, stage))
        .transpose()?;
    let mut dsp = Processor::new(2, &config)?;
    let frames = usize::try_from(rate / 100)?;
    let mut block = vec![0.0; frames * 2];
    let source: Vec<f32> = (0..rate)
        .flat_map(|i| {
            let t = f64::from(i) / f64::from(rate);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "H15-67: bounded test tones stored as f32"
            )]
            [
                ((std::f64::consts::TAU * 220.0 * t).sin() * 0.3) as f32,
                ((std::f64::consts::TAU * 330.0 * t).sin() * 0.3) as f32,
            ]
        })
        .collect();
    let start = Instant::now();
    let cpu_start = cpu_seconds();
    let mut at = 0;
    for _ in 0..seconds * 100 {
        for sample in &mut block {
            *sample = source[at];
            at = (at + 1) % source.len();
        }
        if let Some(ablation) = &mut ablation {
            ablation.process(&mut block);
        } else {
            dsp.process(&mut block)?;
        }
        black_box(&block);
    }
    let cpu = cpu_seconds() - cpu_start;
    let audio = f64::from(seconds) * 100.0 * f64::from(rate / 100) / f64::from(rate);
    println!(
        "rate={rate} mode={mode} flat={flat} filters=26 crossfeed=on peak_detector=sample audio={audio:.3}s cpu={cpu:.3}s cpu_percent={:.3} wall={:.3}s delay_frames={}",
        cpu / audio * 100.0,
        start.elapsed().as_secs_f64(),
        dsp.delay_frames()
    );
    Ok(())
}

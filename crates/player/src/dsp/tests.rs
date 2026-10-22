#![allow(
    clippy::cast_precision_loss,
    reason = "H15-67: bounded synthetic f32 test signals"
)]
use super::*;
use std::f32::consts::TAU;

#[test]
fn flat_processing_only_delays_without_changing_normal_samples() {
    let c = Compiled::new(44_100, 1, Settings::default()).unwrap();
    let mut p = Processor::new(2, &c).unwrap();
    let mut samples = vec![0.1, -0.2, 0.3, -0.1];
    samples.extend(vec![0.0; p.delay_frames() * 2]);
    let source = samples.clone();
    p.process(&mut samples).unwrap();
    assert!(samples[..p.delay_frames() * 2].iter().all(|v| *v == 0.0));
    assert_eq!(
        &samples[p.delay_frames() * 2..],
        &source[..samples.len() - p.delay_frames() * 2]
    );
}

#[test]
fn stereo_balance_and_preamp_use_the_same_channel_volumes() {
    let settings = Settings {
        balance: 0.5,
        preamp_db: 6.020_599_913,
        ..Settings::default()
    };
    let c = Compiled::new(48_000, 1, settings).unwrap();
    let mut p = Processor::new(2, &c).unwrap();
    let mut block = vec![0.1; (p.delay_frames() + 50) * 2];
    p.process(&mut block).unwrap();
    let last = &block[block.len() - 2..];
    assert!((last[0] - 0.1).abs() < 1e-6 && (last[1] - 0.2).abs() < 1e-6);
}

#[test]
fn mono_and_multichannel_tts_rates_remain_finite_and_bounded() {
    for rate in [16_000, 22_050, 24_000, 44_100, 48_000] {
        for channels in [1, 2, 6] {
            let mut settings = Settings {
                crossfeed: Some(CrossfeedSettings::default()),
                preamp_db: 20.0,
                ..Settings::default()
            };
            settings.filters[0] = Filter {
                kind: FilterKind::Peak,
                frequency: 1000.0,
                q: 1.0,
                gain_db: 24.0,
            };
            let c = Compiled::new(rate, 1, settings).unwrap();
            let mut p = Processor::new(channels, &c).unwrap();
            let mut input: Vec<f32> = (0..rate as usize * channels)
                .map(|i| (i as f32 * TAU * 0.1).sin())
                .collect();
            input[0] = f32::NAN;
            input[1] = f32::INFINITY;
            input[2] = f32::MAX;
            p.process(&mut input).unwrap();
            assert!(
                input
                    .iter()
                    .all(|v| v.is_finite() && v.abs() <= 10.0_f32.powf(-1.0 / 20.0) + 1e-6)
            );
        }
    }
}

#[test]
fn invalid_snapshot_or_incomplete_frame_does_not_mutate_current_processing() {
    let c = Compiled::new(44_100, 1, Settings::default()).unwrap();
    let mut p = Processor::new(2, &c).unwrap();
    assert!(
        p.update(&Compiled::new(48_000, 2, Settings::default()).unwrap())
            .is_err()
    );
    let mut incomplete = [0.1];
    assert!(p.process(&mut incomplete).is_err());
    assert_eq!(incomplete, [0.1]);
    assert_eq!(p.version(), 1);
    assert!(
        Compiled::new(
            44_100,
            2,
            Settings {
                balance: f32::NAN,
                ..Settings::default()
            }
        )
        .is_err()
    );
}

#[test]
fn block_boundary_updates_version_and_reset_clears_all_history() {
    let c = Compiled::new(44_100, 1, Settings::default()).unwrap();
    let mut p = Processor::new(2, &c).unwrap();
    p.process(&mut vec![0.4; 1000]).unwrap();
    let next = Compiled::new(
        44_100,
        2,
        Settings {
            balance: 1.0,
            ..Settings::default()
        },
    )
    .unwrap();
    p.update(&next).unwrap();
    assert_eq!(p.version(), 2);
    p.reset();
    let mut silence = vec![0.0; 1000];
    p.process(&mut silence).unwrap();
    assert!(silence.iter().all(|v| *v == 0.0));
    let mut block = vec![0.1; 1000];
    p.process(&mut block).unwrap();
    assert!(block.as_chunks::<2>().0.iter().all(|f| f[0] == 0.0));
}

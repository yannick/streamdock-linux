use super::{Controls, CrossfeedSettings, DspDevice, Settings};
use crate::output::Device;
use std::{
    io,
    sync::{Arc, Mutex},
};

type Log = (Vec<(u32, u16)>, Vec<f32>);

#[derive(Default, Clone)]
struct Recording(Arc<Mutex<Log>>);
impl Device for Recording {
    fn open(&mut self, rate: u32, channels: u16) -> io::Result<()> {
        self.0.lock().unwrap().0.push((rate, channels));
        Ok(())
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .1
            .extend(bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
        Ok(())
    }
    fn delay_frames(&self) -> u32 {
        7
    }
    fn close(&mut self) {}
    fn is_open(&self) -> bool {
        true
    }
}
#[test]
fn mono_upmix_applies_balance_and_drains_the_last_delayed_sample() {
    let recording = Recording::default();
    let controls = Arc::new(Controls::new(Settings {
        balance: 1.0,
        crossfeed: Some(CrossfeedSettings::default()),
        ..Default::default()
    }));
    let mut device = DspDevice::new(recording.clone(), Some(controls));
    device.open(16_000, 1).unwrap();
    device.write_frames(&mut [0.25]).unwrap();
    assert_eq!(device.delay_frames(), 87);
    device.close();
    let log = recording.0.lock().unwrap();
    assert_eq!(log.0, [(16_000, 2)]);
    assert_eq!(&log.1[log.1.len() - 2..], &[0.0, 0.25]);
}
#[test]
fn reset_discards_lookahead_and_new_settings_apply_between_blocks() {
    let recording = Recording::default();
    let controls = Arc::new(Controls::new(Settings::default()));
    let mut device = DspDevice::new(recording.clone(), Some(controls.clone()));
    device.open(24_000, 2).unwrap();
    device.write_frames(&mut [0.25, 0.25]).unwrap();
    device.reset();
    controls.publish(Settings {
        balance: -1.0,
        ..Default::default()
    });
    device.write_frames(&mut vec![0.125; 242]).unwrap();
    device.close();
    let log = recording.0.lock().unwrap();
    assert!(!log.1.contains(&0.25));
    assert_eq!(&log.1[log.1.len() - 2..], &[0.125, 0.0]);
}
#[test]
fn bypass_preserves_format_samples_and_delay() {
    let recording = Recording::default();
    let mut device = DspDevice::new(recording.clone(), None);
    device.open(22_050, 1).unwrap();
    device.write_frames(&mut [0.75, -0.5]).unwrap();
    device.close();
    assert_eq!(device.delay_frames(), 7);
    let log = recording.0.lock().unwrap();
    assert_eq!(log.0, [(22_050, 1)]);
    assert_eq!(log.1, [0.75, -0.5]);
}

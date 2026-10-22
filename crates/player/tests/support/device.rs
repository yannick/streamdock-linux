//! A paced output Device: no sound server, process or hardware access.

use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use player::output::Device;

#[derive(Debug, Clone, Default)]
pub struct FakeDevice(Arc<(Mutex<Log>, Condvar)>, Duration);

#[derive(Debug, Default)]
pub struct Log {
    pub formats: Vec<(u32, u16)>,
    pub samples: Vec<f32>,
    pub silence_writes: usize,
    pub silence_at: Vec<usize>,
    pub closed: bool,
    open: bool,
}

impl FakeDevice {
    pub fn paced(pace: Duration) -> Self {
        Self(Self::default().0, pace)
    }

    pub fn inspect<T>(&self, read: impl FnOnce(&Log) -> T) -> T {
        read(&self.0.0.lock().unwrap())
    }

    pub fn wait(&self, condition: impl Fn(&Log) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut log = self.0.0.lock().unwrap();
        while !condition(&log) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "fake device timed out: {log:?}");
            log = self.0.1.wait_timeout(log, left).unwrap().0;
        }
    }
}

impl Device for FakeDevice {
    fn open(&mut self, rate: u32, channels: u16) -> io::Result<()> {
        let mut log = self.0.0.lock().unwrap();
        log.formats.push((rate, channels));
        log.open = true;
        log.closed = false;
        self.0.1.notify_all();
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        // A real device blocks on back pressure. One ms keeps these tests fast without spinning.
        std::thread::sleep(self.1.max(Duration::from_millis(1)));
        let mut log = self.0.0.lock().unwrap();
        assert!(log.open);
        if bytes.iter().all(|b| *b == 0) {
            log.silence_writes += 1;
            let at = log.samples.len();
            log.silence_at.push(at);
        } else {
            log.samples
                .extend(bytes.as_chunks::<4>().0.iter().map(|s| f32::from_le_bytes(*s)));
        }
        self.0.1.notify_all();
        Ok(())
    }

    fn delay_frames(&self) -> u32 {
        0
    }
    fn close(&mut self) {
        let mut log = self.0.0.lock().unwrap();
        log.open = false;
        log.closed = true;
        self.0.1.notify_all();
    }
    fn is_open(&self) -> bool {
        self.0.0.lock().unwrap().open
    }
}

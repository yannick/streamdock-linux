//! Recording protocol stub. Events are explicit so tests can distinguish wire flow from decoding.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use slimproto::{Playback, PlaybackEvent, PlaybackStatus, Track};
use tokio::sync::{Notify, mpsc};

use super::lms::TIMEOUT;

#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Start(Track),
    Pause,
    Unpause,
    Stop,
    Flush,
    Skip(Duration),
    Volume(f32, f32, u32),
    Enable(bool),
}

#[derive(Default)]
struct State {
    calls: Vec<Call>,
    events: Vec<PlaybackEvent>,
    elapsed: Duration,
    clock: Option<Instant>,
    paused: bool,
    bodies: Vec<Vec<u8>>,
}

#[derive(Default)]
pub struct Recording {
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
}

impl Recording {
    pub fn emit(&self, event: PlaybackEvent) {
        let mut state = self.state.lock().unwrap();
        if event == PlaybackEvent::TrackStarted {
            state.elapsed = Duration::ZERO;
            state.clock = Some(Instant::now());
        }
        state.events.push(event);
    }

    pub fn calls(&self) -> Vec<Call> {
        self.state.lock().unwrap().calls.clone()
    }

    pub async fn body(&self, index: usize, len: usize) -> Vec<u8> {
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let changed = self.changed.notified();
                if let Some(body) = self.state.lock().unwrap().bodies.get(index)
                    && body.len() >= len
                {
                    return body.clone();
                }
                changed.await;
            }
        })
        .await
        .expect("stream body did not arrive")
    }
}

impl Playback for Recording {
    fn start(&self, track: Track, mut source: mpsc::Receiver<Bytes>) {
        let index = {
            let mut state = self.state.lock().unwrap();
            state.calls.push(Call::Start(track));
            state.bodies.push(Vec::new());
            state.bodies.len() - 1
        };
        let state = Arc::clone(&self.state);
        let changed = Arc::clone(&self.changed);
        tokio::spawn(async move {
            while let Some(chunk) = source.recv().await {
                state.lock().unwrap().bodies[index].extend_from_slice(&chunk);
                changed.notify_one();
            }
        });
    }

    fn pause(&self) {
        let mut state = self.state.lock().unwrap();
        state.calls.push(Call::Pause);
        if let Some(clock) = state.clock.take() {
            state.elapsed += clock.elapsed();
        }
        state.paused = true;
    }

    fn unpause(&self, _at: Option<Instant>) {
        let mut state = self.state.lock().unwrap();
        state.calls.push(Call::Unpause);
        state.clock = Some(Instant::now());
        state.paused = false;
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        state.calls.push(Call::Stop);
        state.clock = None;
        state.elapsed = Duration::ZERO;
        state.paused = false;
    }

    fn flush_next(&self) {
        self.state.lock().unwrap().calls.push(Call::Flush);
    }
    fn skip(&self, ahead: Duration) {
        self.state.lock().unwrap().calls.push(Call::Skip(ahead));
    }
    fn volume(&self, left: f32, right: f32, percent: u32) {
        self.state
            .lock()
            .unwrap()
            .calls
            .push(Call::Volume(left, right, percent));
    }
    fn enable(&self, on: bool) {
        self.state.lock().unwrap().calls.push(Call::Enable(on));
    }

    fn status(&self) -> PlaybackStatus {
        let mut state = self.state.lock().unwrap();
        PlaybackStatus {
            elapsed: state.elapsed + state.clock.map_or(Duration::ZERO, |clock| clock.elapsed()),
            output_fill: 100,
            output_size: 1000,
            paused: state.paused,
            events: std::mem::take(&mut state.events),
            ..Default::default()
        }
    }
}

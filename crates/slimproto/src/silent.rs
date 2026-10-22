//! A [`Playback`] that plays nothing: it drains each stream and reports the track as started,
//! decoded and done. For tests against a real server, and for `streamdockd` where no audio output
//! is available.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::mpsc;

use crate::{Playback, PlaybackEvent, PlaybackStatus, Track};

/// After a stream ends, wait this long for the next track before reporting the end of playback.
const DONE_AFTER: Duration = Duration::from_secs(2);

/// Plays nothing; see the module docs.
#[derive(Debug, Default)]
pub struct Silent {
    events: Arc<Mutex<Vec<PlaybackEvent>>>,
    started: Mutex<Option<Instant>>,
    /// Bumped by every start and stop, so a drained stream only reports the end of playback when
    /// nothing came after it.
    generation: Arc<AtomicU64>,
}

impl Silent {
    fn push(events: &Mutex<Vec<PlaybackEvent>>, event: PlaybackEvent) {
        events.lock().expect("silent playback lock poisoned").push(event);
    }
}

impl Playback for Silent {
    fn start(&self, track: Track, mut source: mpsc::Receiver<Bytes>) {
        tracing::debug!(?track, "silent playback: start");
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (events, current) = (Arc::clone(&self.events), Arc::clone(&self.generation));
        *self.started.lock().expect("silent playback lock poisoned") = Some(Instant::now());
        tokio::spawn(async move {
            let mut total = 0usize;
            while let Some(chunk) = source.recv().await {
                if total == 0 {
                    Self::push(&events, PlaybackEvent::TrackStarted);
                }
                total += chunk.len();
            }
            tracing::debug!(total, "silent playback: stream drained");
            Self::push(&events, PlaybackEvent::DecodeDone);
            tokio::time::sleep(DONE_AFTER).await;
            if current.load(Ordering::SeqCst) == generation {
                Self::push(&events, PlaybackEvent::PlaybackDone);
            }
        });
    }

    fn pause(&self) {}

    fn unpause(&self, _at: Option<Instant>) {}

    fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        *self.started.lock().expect("silent playback lock poisoned") = None;
    }

    fn flush_next(&self) {}

    fn skip(&self, _ahead: Duration) {}

    fn volume(&self, _left: f32, _right: f32, _percent: u32) {}

    fn enable(&self, _on: bool) {}

    fn status(&self) -> PlaybackStatus {
        let started = *self.started.lock().expect("silent playback lock poisoned");
        PlaybackStatus {
            elapsed: started.map(|t| t.elapsed()).unwrap_or_default(),
            output_size: 3_528_000,
            decode_size: 2 * 1024 * 1024,
            events: std::mem::take(&mut *self.events.lock().expect("silent playback lock poisoned")),
            ..PlaybackStatus::default()
        }
    }
}

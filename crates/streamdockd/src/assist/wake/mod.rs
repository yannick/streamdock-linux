//! The wake word (H15-68): with `wake_word` set in `[modules.homeassistant.assist]`, one
//! microphone stream (`pw-record`, or the `test_input` WAV) feeds the microWakeWord detector
//! (`crates/wakeword`). When it hears the word, the speech that follows goes to an assist request
//! (H15-43's pipeline), ended by voice activity detection (H15-69); detection pauses until the
//! answer has played. Turning the wake word off stops the listener and its `pw-record`.
//!
//! The listener reports to the UI through [`Hub::set_wake_state`] (level, probability, phase) and
//! to the activity feed (heard, outcome).

pub mod models;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures_util::future::BoxFuture;
use streamdock_proto::streamdock::v1 as pb;
use tokio::sync::mpsc;

use super::capture::Capture;
use super::{Assist, Update, vad};
use crate::hub::Hub;
use models::Models;

/// How often the UI gets the level and probability while listening.
const STATE_EVERY: Duration = Duration::from_millis(200);
/// Wait before trying again after the microphone or the model failed.
const RETRY: Duration = Duration::from_secs(10);
/// Activity feed source.
const SOURCE: &str = "voice";

/// What the listener runs with; a change restarts it.
#[derive(Debug, Clone, PartialEq)]
struct Key {
    config: super::Config,
    url: String,
    token_file: std::path::PathBuf,
}

impl Key {
    fn of(assist: &Assist) -> Self {
        Self {
            config: assist.config.clone(),
            url: assist.url.clone(),
            token_file: assist.token_file.clone(),
        }
    }
}

#[derive(Debug)]
struct Running {
    key: Key,
    task: tokio::task::AbortHandle,
}

/// The wake word service: one listener at most, (re)started by [`WakeWord::apply`].
#[derive(Debug)]
pub struct WakeWord {
    hub: Arc<Hub>,
    models: Models,
    running: Mutex<Option<Running>>,
}

impl WakeWord {
    #[must_use]
    pub fn new(hub: Arc<Hub>, models: Models) -> Arc<Self> {
        Arc::new(Self {
            hub,
            models,
            running: Mutex::new(None),
        })
    }

    #[must_use]
    pub fn models(&self) -> &Models {
        &self.models
    }

    /// Starts, restarts or stops the listener for these module tables (at start and on every
    /// reload); nothing happens when its settings are unchanged.
    pub fn apply(&self, modules: &BTreeMap<String, toml::Table>) {
        self.update(modules, false);
    }

    /// As [`Self::apply`], but restarts a running listener even if its settings are unchanged
    /// (its model file was replaced).
    pub fn restart(&self, modules: &BTreeMap<String, toml::Table>) {
        self.update(modules, true);
    }

    fn update(&self, modules: &BTreeMap<String, toml::Table>, force: bool) {
        let assist = match wanted(&self.hub, modules) {
            Ok(a) => a,
            Err(e) => {
                self.hub.report(
                    SOURCE,
                    pb::ActivityLevel::ACTIVITY_LEVEL_ERROR,
                    format!("wake word: {e:#}"),
                );
                None
            }
        };
        let key = assist.as_deref().map(Key::of);
        let mut running = self.running.lock().expect("wake word lock poisoned");
        if !force && running.as_ref().map(|r| &r.key) == key.as_ref() {
            return;
        }
        if let Some(old) = running.take() {
            old.task.abort();
        }
        match (assist, key) {
            (Some(assist), Some(key)) => {
                let task = tokio::spawn(listen(Arc::clone(&self.hub), self.models.clone(), assist));
                *running = Some(Running {
                    key,
                    task: task.abort_handle(),
                });
            }
            _ => {
                if self.hub.wake_state().is_some() {
                    self.hub.set_wake_state(off_state());
                }
            }
        }
    }
}

/// The assist service if the wake word is on: `[modules.homeassistant]` enabled and `wake_word` set.
fn wanted(hub: &Arc<Hub>, modules: &BTreeMap<String, toml::Table>) -> anyhow::Result<Option<Arc<Assist>>> {
    let ha_enabled = modules
        .get("homeassistant")
        .is_some_and(|t| t.get("enabled").and_then(toml::Value::as_bool) != Some(false));
    if !ha_enabled {
        return Ok(None);
    }
    let context = crate::modules::Context::new(Arc::clone(hub), modules);
    Ok(super::from_context(&context)?.filter(|a| a.config.wake_word.is_some()))
}

fn off_state() -> pb::WakeWordState {
    pb::WakeWordState {
        phase: "off".into(),
        ..Default::default()
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The listener: runs until aborted; after a failure it reports and tries again.
async fn listen(hub: Arc<Hub>, models: Models, assist: Arc<Assist>) {
    let model = assist.config.wake_word.clone().unwrap_or_default();
    let mut state = pb::WakeWordState {
        enabled: true,
        model: model.clone(),
        phase: "starting".into(),
        level_db: -100.0,
        ..Default::default()
    };
    let mut reported = String::new();
    loop {
        hub.set_wake_state(state.clone());
        match listen_once(&hub, &models, &assist, &mut state).await {
            Ok(()) => {
                // The test input played to its end: keep the state, wait for the next change.
                state.phase = "test input played".into();
                hub.set_wake_state(state.clone());
                std::future::pending::<()>().await;
            }
            Err(e) => {
                let problem = format!("{e:#}");
                tracing::warn!(error = %problem, "wake word listener stopped");
                if problem != reported {
                    hub.report(
                        SOURCE,
                        pb::ActivityLevel::ACTIVITY_LEVEL_ERROR,
                        format!("wake word: {problem}"),
                    );
                    reported.clone_from(&problem);
                }
                state.problem = problem;
                state.phase = "off".into();
                hub.set_wake_state(state.clone());
                tokio::time::sleep(RETRY).await;
                state.phase = "starting".into();
            }
        }
    }
}

/// One microphone session: detects until the source ends (`Ok`) or fails.
async fn listen_once(
    hub: &Arc<Hub>,
    models: &Models,
    assist: &Arc<Assist>,
    state: &mut pb::WakeWordState,
) -> anyhow::Result<()> {
    let (info, bytes) = models.load(&state.model)?;
    let threshold = assist.config.wake_threshold.unwrap_or(info.cutoff);
    let mut detector = wakeword::Detector::new(wakeword::Model::new(&bytes)?, threshold, info.window)?;
    let mut mic = assist.capture()?;
    state.wake_word.clone_from(&info.wake_word);
    state.threshold = threshold;
    state.phase = "listening".into();
    state.problem.clear();
    hub.set_wake_state(state.clone());
    tracing::info!(model = %info.id, threshold, "wake word listening");
    let mut meter = Meter::default();
    loop {
        let Some(block) = mic.next().await? else {
            return Ok(());
        };
        let samples: Vec<i16> = block
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        meter.add(&samples);
        let mut heard = false;
        for score in detector.push(&samples)? {
            state.probability = score.probability;
            state.mean = score.mean;
            heard |= score.detected;
        }
        if let Some(level) = meter.due() {
            state.level_db = level;
            hub.set_wake_state(state.clone());
        }
        if heard {
            heard_once(hub, assist, &mut mic, state).await;
            detector.reset();
        }
    }
}

/// The word was heard: runs the request on the speech that follows, then reports how it ended.
async fn heard_once(
    hub: &Arc<Hub>,
    assist: &Arc<Assist>,
    mic: &mut Box<dyn Capture>,
    state: &mut pb::WakeWordState,
) {
    state.heard_count += 1;
    state.heard_at_unix_ms = unix_ms();
    state.phase = "request".into();
    hub.set_wake_state(state.clone());
    hub.report(
        SOURCE,
        pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
        format!("\"{}\" heard", state.wake_word),
    );
    let started = Instant::now();
    let (level, text) = request(assist, mic).await;
    tracing::info!(result = %text, seconds = started.elapsed().as_secs_f32(), "wake word request ended");
    hub.report(SOURCE, level, text.clone());
    state.last_result = text;
    state.phase = "listening".into();
    state.probability = 0.0;
    state.mean = 0.0;
    hub.set_wake_state(state.clone());
}

/// Streams the microphone into an assist request (ended by VAD) and keeps draining it until the
/// request is over, answer included. Returns the activity level and text of the outcome.
async fn request(assist: &Arc<Assist>, mic: &mut Box<dyn Capture>) -> (pb::ActivityLevel, String) {
    use pb::ActivityLevel::{ACTIVITY_LEVEL_ERROR, ACTIVITY_LEVEL_INFO, ACTIVITY_LEVEL_OK};
    let (tx, rx) = mpsc::channel(256);
    let mut forward = Some(tx);
    let mut request = assist.request_with(vad::Vad::wrap(Box::new(Forwarded { rx }), assist.config.vad));
    let mut mic_open = true;
    loop {
        tokio::select! {
            update = request.updates.recv() => match update {
                Some(Update::Done(o)) if o.heard.is_empty() => return (ACTIVITY_LEVEL_INFO, "nothing understood".into()),
                Some(Update::Done(o)) => return (ACTIVITY_LEVEL_OK, format!("\"{}\" → {}", o.heard, o.answer)),
                Some(Update::Failed(e)) => return (ACTIVITY_LEVEL_ERROR, format!("failed: {e}")),
                Some(Update::Cancelled(why)) => return (ACTIVITY_LEVEL_INFO, why),
                Some(_) => {}
                None => return (ACTIVITY_LEVEL_ERROR, "the request ended without a result".into()),
            },
            block = mic.next(), if mic_open => match block {
                Ok(Some(b)) => {
                    // The request's capture ends at the end of speech; then just drain the mic.
                    if forward.as_ref().is_some_and(|tx| tx.try_send(b).is_err()) {
                        forward = None;
                    }
                }
                Ok(None) => {
                    mic_open = false;
                    forward = None;
                }
                Err(e) => {
                    request.cancel();
                    return (ACTIVITY_LEVEL_ERROR, format!("failed: microphone: {e}"));
                }
            },
        }
    }
}

/// The microphones PipeWire offers (`Audio/Source` nodes from `pw-dump`): node name and
/// description. Empty when PipeWire can't be asked.
pub async fn microphones(hub: &Hub) -> Vec<(String, String)> {
    let run = tokio::process::Command::new("pw-dump")
        .envs(crate::player::pipewire_env(hub))
        .kill_on_drop(true)
        .output();
    let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(5), run).await else {
        return Vec::new();
    };
    sources(&String::from_utf8_lossy(&out.stdout))
}

fn sources(dump: &str) -> Vec<(String, String)> {
    let Ok(serde_json::Value::Array(objects)) = serde_json::from_str::<serde_json::Value>(dump) else {
        return Vec::new();
    };
    objects
        .iter()
        .filter(|o| o["type"] == "PipeWire:Interface:Node")
        .map(|o| &o["info"]["props"])
        .filter(|p| p["media.class"] == "Audio/Source")
        .filter_map(|p| {
            let name = p["node.name"].as_str()?.to_owned();
            let description = p["node.description"].as_str().unwrap_or(&name).to_owned();
            Some((name, description))
        })
        .collect()
}

/// The request's side of the microphone: the blocks the listener forwards.
struct Forwarded {
    rx: mpsc::Receiver<Bytes>,
}

impl Capture for Forwarded {
    fn next(&mut self) -> BoxFuture<'_, std::io::Result<Option<Bytes>>> {
        Box::pin(async move { Ok(self.rx.recv().await) })
    }

    fn ends_on_release(&self) -> bool {
        false
    }
}

/// The microphone level (RMS in dBFS) over [`STATE_EVERY`].
#[derive(Debug)]
struct Meter {
    sum: f64,
    count: usize,
    since: Instant,
}

impl Default for Meter {
    fn default() -> Self {
        Self {
            sum: 0.0,
            count: 0,
            since: Instant::now(),
        }
    }
}

impl Meter {
    fn add(&mut self, samples: &[i16]) {
        self.sum += samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>();
        self.count += samples.len();
    }

    /// The level when the period is over (and starts the next one).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "a dB value"
    )]
    fn due(&mut self) -> Option<f32> {
        if self.since.elapsed() < STATE_EVERY || self.count == 0 {
            return None;
        }
        let mean = self.sum / self.count as f64 / (32768.0 * 32768.0);
        let db = if mean > 0.0 {
            (10.0 * mean.log10()).max(-100.0)
        } else {
            -100.0
        };
        *self = Self::default();
        Some(db as f32)
    }
}

#[cfg(test)]
mod tests;

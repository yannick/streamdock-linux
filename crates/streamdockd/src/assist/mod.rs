//! Push-to-talk (H15-43): while a key with `{ type = "assist" }` is held, the request is recorded
//! and streamed to Home Assistant's assist pipeline (speech to text, intent, text to speech, over
//! the WebSocket API); its spoken answer plays through the same PipeWire path as the built-in
//! player (`pw-cat` into the DSP node).
//!
//! ```toml
//! [modules.homeassistant.assist]
//! pipeline = "01h…"            # optional: assist pipeline id (default: Home Assistant's preferred)
//! microphone = "alsa_input.…"  # optional: PipeWire source (default: PipeWire's default source)
//! test_input = "/var/lib/streamdockd/assist-test.wav"  # optional: a 16-bit WAV instead of the mic
//! speak = true                 # play the answer (default true)
//! silence_ms = 700             # tap mode (H15-69): silence after speech that ends the request
//! vad_threshold = 0.5          # speech score 0-1 (lower = more sensitive)
//! no_speech_ms = 5000          # tap mode: no speech this long = nothing heard (cancelled)
//! max_seconds = 15             # longest recording, in every mode
//! wake_word = "okay_nabu"      # H15-68: listen for a wake word (built-in or uploaded model id)
//! wake_threshold = 0.85        # its detection threshold 0.5-0.99 (default: the model's)
//! ```
//!
//! Keys: `{ type = "assist" }` listens while held (`mode = "hold"`); `mode = "tap"` starts on a tap
//! and ends by itself at the end of speech ([`vad`]); a second tap ends it early, a tap while
//! Home Assistant answers cancels the request.

pub mod capture;
pub mod pipeline;
pub mod vad;
pub mod wake;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use slimproto::{Format, Playback as _, PlaybackEvent, Track};
use tokio::sync::{mpsc, oneshot, watch};

use crate::hub::Hub;
use capture::{Capture, PwRecord, WavFile};
pub use pipeline::{Outcome, Progress};

/// Longest answer we play.
const MAX_ANSWER: Duration = Duration::from_secs(120);

/// `[modules.homeassistant.assist]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub pipeline: Option<String>,
    pub microphone: Option<String>,
    pub test_input: Option<PathBuf>,
    pub speak: bool,
    /// End-of-speech detection for the tap mode.
    pub vad: vad::Settings,
    /// Longest recording.
    pub max_listen: Duration,
    /// The wake word's model id (H15-68); `None`: no wake word.
    pub wake_word: Option<String>,
    /// Its threshold; `None`: the model's own cutoff.
    pub wake_threshold: Option<f32>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pipeline: None,
            microphone: None,
            test_input: None,
            speak: true,
            vad: vad::Settings::default(),
            max_listen: pipeline::MAX_LISTEN,
            wake_word: None,
            wake_threshold: None,
        }
    }
}

/// How an assist key records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// While the key is held (H15-43).
    #[default]
    Hold,
    /// From a tap to the end of speech (H15-69).
    Tap,
}

/// Reads an integer setting within `range`.
fn number(
    table: &toml::Table,
    key: &str,
    range: std::ops::RangeInclusive<i64>,
) -> anyhow::Result<Option<u64>> {
    let at = "[modules.homeassistant.assist]";
    table
        .get(key)
        .map(|v| {
            v.as_integer()
                .filter(|n| range.contains(n))
                .and_then(|n| u64::try_from(n).ok())
                .with_context(|| format!("{at} {key} must be {}-{}", range.start(), range.end()))
        })
        .transpose()
}

impl Config {
    pub const KEYS: [&str; 10] = [
        "pipeline",
        "microphone",
        "test_input",
        "speak",
        "silence_ms",
        "vad_threshold",
        "no_speech_ms",
        "max_seconds",
        "wake_word",
        "wake_threshold",
    ];

    /// Reads `[modules.homeassistant.assist]` (missing: the defaults).
    pub fn from_table(table: Option<&toml::Table>) -> anyhow::Result<Self> {
        let at = "[modules.homeassistant.assist]";
        let mut config = Self::default();
        let Some(table) = table else { return Ok(config) };
        let text = |key: &str| -> anyhow::Result<Option<String>> {
            table
                .get(key)
                .map(|v| {
                    v.as_str()
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_owned)
                        .with_context(|| format!("{at} {key} must be a non-empty string"))
                })
                .transpose()
        };
        for key in table.keys() {
            anyhow::ensure!(
                Self::KEYS.contains(&key.as_str()),
                "{at} unknown key {key:?} (known: {})",
                Self::KEYS.join(", ")
            );
        }
        config.pipeline = text("pipeline")?;
        config.microphone = text("microphone")?;
        config.test_input = text("test_input")?.map(PathBuf::from);
        if let Some(v) = table.get("speak") {
            config.speak = v
                .as_bool()
                .with_context(|| format!("{at} speak must be true or false"))?;
        }
        if let Some(ms) = number(table, "silence_ms", 200..=5000)? {
            config.vad.silence = Duration::from_millis(ms);
        }
        if let Some(ms) = number(table, "no_speech_ms", 1000..=30_000)? {
            config.vad.no_speech = Duration::from_millis(ms);
        }
        if let Some(s) = number(table, "max_seconds", 2..=60)? {
            config.max_listen = Duration::from_secs(s);
        }
        if let Some(v) = table.get("vad_threshold") {
            #[allow(clippy::cast_possible_truncation, reason = "H15-69: a 0-1 threshold")]
            let t = v
                .as_float()
                .filter(|t| (0.05..=0.95).contains(t))
                .with_context(|| format!("{at} vad_threshold must be 0.05-0.95"))? as f32;
            config.vad.threshold = t;
        }
        config.wake_word = text("wake_word")?;
        if let Some(v) = table.get("wake_threshold") {
            #[allow(clippy::cast_possible_truncation, reason = "H15-68: a 0-1 threshold")]
            let t = v
                .as_float()
                .filter(|t| (0.5..=0.99).contains(t))
                .with_context(|| format!("{at} wake_threshold must be 0.5-0.99"))? as f32;
            config.wake_threshold = Some(t);
        }
        Ok(config)
    }
}

/// What a request reports to the key that started it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Progress(Progress),
    /// The answer is playing.
    Speaking,
    Done(Outcome),
    Failed(String),
    /// Cancelled by a tap, or nobody spoke (tap mode): nothing was sent.
    Cancelled(String),
}

/// The assist service of one keys configuration.
#[derive(Debug)]
pub struct Assist {
    pub config: Config,
    /// Home Assistant's base URL and token file (`[modules.homeassistant]`).
    pub url: String,
    pub token_file: PathBuf,
    pub hub: Arc<Hub>,
    pub http: reqwest::Client,
}

/// A running request. [`Request::stop`] ends the recording now (key released, second tap),
/// [`Request::cancel`] drops it (and stops the answer). Updates arrive on `updates`; the last one
/// is [`Update::Done`], [`Update::Failed`] or [`Update::Cancelled`].
#[derive(Debug)]
pub struct Request {
    stop: Option<oneshot::Sender<()>>,
    cancel: watch::Sender<bool>,
    pub updates: mpsc::UnboundedReceiver<Update>,
}

impl Request {
    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }

    pub fn cancel(&self) {
        let _ = self.cancel.send(true);
    }

    /// Splits off the updates (to forward them elsewhere), keeping the controls.
    pub fn take_updates(&mut self) -> mpsc::UnboundedReceiver<Update> {
        std::mem::replace(&mut self.updates, mpsc::unbounded_channel().1)
    }
}

/// The request failed because it was cancelled.
#[derive(Debug, thiserror::Error)]
#[error("cancelled")]
struct Cancelled;

/// Waits until `cancel` is set (or its sender is gone).
async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    let _ = cancel.wait_for(|c| *c).await;
}

/// The assist service, when `[modules.homeassistant]` is configured (H15-43, H15-69): shared by
/// the assist keys and the wake word.
pub fn from_context(context: &crate::modules::Context) -> anyhow::Result<Option<Arc<Assist>>> {
    let Some(table) = context.tables.get("homeassistant") else {
        return Ok(None);
    };
    let ha = crate::modules::homeassistant::Config::from_table(table)?;
    let assist = table
        .get("assist")
        .map(|v| {
            v.as_table()
                .context("[modules.homeassistant] assist must be a table")
        })
        .transpose()?;
    Ok(Some(Arc::new(Assist {
        config: Config::from_table(assist)?,
        url: ha.url,
        token_file: ha.token_file,
        hub: Arc::clone(&context.hub),
        http: reqwest::Client::builder().build()?,
    })))
}

impl Assist {
    /// Starts a request from an assist key: its microphone (or test input); in tap mode it ends
    /// by itself at the end of speech.
    pub fn request(self: &Arc<Self>, mode: Mode) -> Request {
        match self.capture() {
            Ok(capture) => {
                let capture = match mode {
                    Mode::Hold => capture,
                    Mode::Tap => vad::Vad::wrap(capture, self.config.vad),
                };
                self.request_with(capture)
            }
            Err(e) => {
                let (tx, updates) = mpsc::unbounded_channel();
                let _ = tx.send(Update::Failed(format!("{e:#}")));
                Request {
                    stop: None,
                    cancel: watch::channel(false).0,
                    updates,
                }
            }
        }
    }

    /// Starts a request on any capture (the wake word's, H15-68). A capture that ends by itself
    /// ([`Capture::ends_on_release`] false, or wrapped in [`vad::Vad`]) needs no [`Request::stop`].
    pub fn request_with(self: &Arc<Self>, capture: Box<dyn Capture>) -> Request {
        let (stop_tx, stop) = oneshot::channel();
        let (cancel_tx, cancel) = watch::channel(false);
        let (tx, updates) = mpsc::unbounded_channel();
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let update = match this.run(capture, stop, cancel, &tx).await {
                Ok(outcome) => Update::Done(outcome),
                Err(e) if e.is::<Cancelled>() => Update::Cancelled("cancelled".into()),
                Err(e)
                    if e.chain()
                        .any(|c| c.downcast_ref::<std::io::Error>().is_some_and(is_nothing_heard)) =>
                {
                    Update::Cancelled("nothing heard".into())
                }
                Err(e) => Update::Failed(format!("{e:#}")),
            };
            let _ = tx.send(update);
        });
        Request {
            stop: Some(stop_tx),
            cancel: cancel_tx,
            updates,
        }
    }

    /// The configured microphone, or the test input.
    pub(crate) fn capture(&self) -> anyhow::Result<Box<dyn Capture>> {
        if let Some(path) = &self.config.test_input {
            let file = WavFile::open(path).with_context(|| format!("test input {}", path.display()))?;
            return Ok(Box::new(file));
        }
        let env = crate::player::pipewire_env(&self.hub);
        let mic = PwRecord::start(&env, self.config.microphone.as_deref())
            .context("cannot start pw-record (no microphone?)")?;
        Ok(Box::new(mic))
    }

    async fn run(
        &self,
        capture: Box<dyn Capture>,
        stop: oneshot::Receiver<()>,
        mut cancel: watch::Receiver<bool>,
        tx: &mpsc::UnboundedSender<Update>,
    ) -> anyhow::Result<Outcome> {
        let token = std::fs::read_to_string(&self.token_file).with_context(|| {
            format!(
                "cannot read the Home Assistant token {}",
                self.token_file.display()
            )
        })?;
        let target = pipeline::Target {
            url: self.url.clone(),
            token: token.trim().to_owned(),
            pipeline: self.config.pipeline.clone(),
            max_listen: self.config.max_listen,
        };
        let (ptx, mut prx) = mpsc::unbounded_channel();
        let forward = {
            let tx = tx.clone();
            tokio::spawn(async move {
                while let Some(p) = prx.recv().await {
                    let _ = tx.send(Update::Progress(p));
                }
            })
        };
        // Dropping the run closes the WebSocket: Home Assistant abandons it.
        let outcome = tokio::select! {
            outcome = pipeline::run(&target, capture, stop, ptx) => outcome,
            () = cancelled(&mut cancel) => Err(Cancelled.into()),
        };
        let _ = forward.await;
        let outcome = outcome?;
        if self.config.speak
            && let Some(url) = &outcome.tts_url
        {
            let _ = tx.send(Update::Speaking);
            self.speak(url, &mut cancel).await.context("playing the answer")?;
        }
        Ok(outcome)
    }

    /// Fetches the answer's audio and plays it into the DSP node, until it has played out.
    async fn speak(&self, url: &str, cancel: &mut watch::Receiver<bool>) -> anyhow::Result<()> {
        let res = self
            .http
            .get(url)
            .timeout(Duration::from_secs(30))
            .send()
            .await?
            .error_for_status()?;
        let mime = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = res.bytes().await?;
        let format = audio_format(&mime, url)?;
        let device = crate::player::pw_cat(&self.hub, &crate::config::OutputChoice::Dsp);
        let on_volume: player::VolumeHandler = Box::new(|_, _, _| {});
        let player = player::Player::new(device, Duration::from_secs(2), on_volume)?;
        let (stx, srx) = mpsc::channel(1);
        stx.try_send(body).expect("fresh channel");
        drop(stx);
        player.start(
            Track {
                format,
                replay_gain: 1.0,
                autostart: true,
                // The whole answer is in memory: a short threshold avoids starting on an empty buffer.
                output_threshold: Duration::from_millis(300),
                transition: slimproto::Transition::default(),
            },
            srx,
        );
        let started = tokio::time::Instant::now();
        let result = tokio::select! {
            result = wait_played(&player) => result,
            () = cancelled(cancel) => {
                // Silence now: the output's buffer is dropped, not played out.
                player.stop();
                Err(Cancelled.into())
            }
        };
        tracing::info!(
            ok = result.is_ok(),
            seconds = started.elapsed().as_secs_f32(),
            "assist: answer played"
        );
        // Joins the player's threads: off the async runtime.
        tokio::task::spawn_blocking(move || drop(player)).await?;
        result
    }
}

fn is_nothing_heard(e: &std::io::Error) -> bool {
    e.get_ref()
        .is_some_and(<dyn std::error::Error + Send + Sync>::is::<vad::NothingHeard>)
}

/// Polls the player until the answer played out.
async fn wait_played(player: &player::Player) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + MAX_ANSWER;
    loop {
        for event in player.status().events {
            match event {
                PlaybackEvent::PlaybackDone => return Ok(()),
                PlaybackEvent::DecodeError(e) => anyhow::bail!("cannot decode the answer: {e}"),
                _ => {}
            }
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "the answer played too long"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The decoder format for Home Assistant's TTS audio (MP3 by default, WAV or FLAC when set up).
fn audio_format(mime: &str, url: &str) -> anyhow::Result<Format> {
    let ext = url.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    let from_header = Format::Pcm {
        sample_rate: None,
        sample_size: None,
        channels: None,
        big_endian: None,
    };
    Ok(match (mime, ext.as_str()) {
        ("audio/mpeg" | "audio/mp3", _) | (_, "mp3") => Format::Mp3,
        ("audio/flac" | "audio/x-flac", _) | (_, "flac") => Format::Flac,
        ("audio/wav" | "audio/x-wav" | "audio/wave", _) | (_, "wav") => from_header,
        _ => anyhow::bail!("unsupported answer audio {mime:?} ({url})"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config() {
        let defaults = Config::from_table(None).unwrap();
        assert_eq!(defaults, Config::default());
        assert!(defaults.speak);
        assert_eq!(defaults.vad.silence, Duration::from_millis(700));
        let t: toml::Table =
            toml::from_str("silence_ms = 900\nvad_threshold = 0.3\nno_speech_ms = 4000\nmax_seconds = 10")
                .unwrap();
        let c = Config::from_table(Some(&t)).unwrap();
        assert_eq!(c.vad.silence, Duration::from_millis(900));
        assert!((c.vad.threshold - 0.3).abs() < 1e-6);
        assert_eq!(c.vad.no_speech, Duration::from_secs(4));
        assert_eq!(c.max_listen, Duration::from_secs(10));
        for bad in [
            "silence_ms = 10",
            "vad_threshold = 2.0",
            "max_seconds = 0",
            "no_speech_ms = \"x\"",
        ] {
            let t: toml::Table = toml::from_str(bad).unwrap();
            assert!(Config::from_table(Some(&t)).is_err(), "{bad}");
        }
        let t: toml::Table = toml::from_str(
            r#"pipeline = "p"
test_input = "/x.wav"
speak = false"#,
        )
        .unwrap();
        let c = Config::from_table(Some(&t)).unwrap();
        assert_eq!(c.pipeline.as_deref(), Some("p"));
        assert_eq!(c.test_input, Some(PathBuf::from("/x.wav")));
        assert!(!c.speak);
        let bad: toml::Table = toml::from_str("mic = \"x\"").unwrap();
        assert!(Config::from_table(Some(&bad)).is_err());
    }

    #[test]
    fn formats() {
        assert_eq!(audio_format("audio/mpeg", "/a").unwrap(), Format::Mp3);
        assert_eq!(audio_format("", "/api/tts_proxy/x.mp3").unwrap(), Format::Mp3);
        assert_eq!(audio_format("", "/x.flac").unwrap(), Format::Flac);
        assert!(matches!(
            audio_format("audio/wav", "/x").unwrap(),
            Format::Pcm { .. }
        ));
        assert!(audio_format("text/html", "/x").is_err());
    }
}

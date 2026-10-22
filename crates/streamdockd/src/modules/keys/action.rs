//! Actions a configured key or knob runs (H15-23), parsed from the config and executed against
//! the outside world: HTTP, commands, the mode switch, dock brightness, Home Assistant, Lyrion.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context as _, bail};
use serde_json::Value as Json;
use tokio::sync::Mutex;

use crate::hub::{Command, Hub};
use crate::modes::ModeSwitch;
use crate::modules::lyrion::client::Client as LyrionClient;

/// Default timeout for `http` and `command` actions.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Longest `delay` step in a multi action.
const MAX_DELAY: Duration = Duration::from_secs(60);

/// What a key or knob does.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Http {
        method: reqwest::Method,
        url: String,
        body: Option<String>,
        headers: Vec<(String, String)>,
        timeout: Duration,
    },
    Command {
        argv: Vec<String>,
        timeout: Duration,
    },
    Mode(String),
    /// Switch to a profile by name (H15-25).
    Profile(String),
    BrightnessSet(u8),
    /// Percent per knob detent (or per press).
    BrightnessStep(i32),
    /// This machine's audio output (H15-50): volume percent per knob detent (or per press).
    AudioStep(i32),
    /// Mute or unmute this machine's audio output.
    AudioMute,
    HomeAssistant {
        domain: String,
        service: String,
        data: Json,
    },
    /// An LMS command on `player` (default: `[modules.lyrion] player`).
    Lyrion {
        command: Vec<String>,
        player: Option<String>,
    },
    Multi(Vec<Step>),
    /// Alternates: `on` when off, `off` when on.
    Toggle {
        on: Box<Action>,
        off: Box<Action>,
        on_label: Option<String>,
        off_label: Option<String>,
        /// Key images per side (H15-28), overriding the key's `image`.
        on_image: Option<String>,
        off_image: Option<String>,
        /// A Home Assistant entity whose state ("on") the key follows: `state = "light.x"`, or
        /// the one entity both sides call.
        state: Option<String>,
    },
    /// Push-to-talk (H15-43): held = listening (`mode = "hold"`), or from a tap to the end of
    /// speech (`mode = "tap"`, H15-69); then Home Assistant's assist pipeline answers. Only
    /// directly on a key; uses `[modules.homeassistant]` and its `assist` table.
    Assist(crate::assist::Mode),
}

/// One step of a multi action.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Run(Action),
    Delay(Duration),
}

/// What the actions may use. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Env {
    pub hub: Arc<Hub>,
    pub modes: Arc<OnceLock<ModeSwitch>>,
    pub http: reqwest::Client,
    pub allow_commands: bool,
    pub homeassistant: Option<HomeAssistantTarget>,
    pub lyrion: Option<LyrionTarget>,
    /// Push-to-talk, when a key has an `assist` action.
    pub assist: Option<Arc<crate::assist::Assist>>,
}

#[derive(Debug, Clone)]
pub struct HomeAssistantTarget {
    pub url: String,
    pub token_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct LyrionTarget {
    pub client: LyrionClient,
    /// The default player as configured: name or MAC.
    pub player: String,
    /// Resolved MACs by configured player name, once looked up.
    pub player_ids: Arc<Mutex<std::collections::HashMap<String, String>>>,
}

/// An action table being parsed: its `type` and settings, for error messages.
struct Fields<'a> {
    kind: &'a str,
    table: &'a toml::Table,
}

impl Fields<'_> {
    fn text(&self, key: &str) -> anyhow::Result<Option<String>> {
        self.table
            .get(key)
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .with_context(|| format!("{}: {key} must be a string", self.kind))
            })
            .transpose()
    }

    /// A required, non-empty string.
    fn need(&self, key: &str) -> anyhow::Result<String> {
        self.text(key)?
            .filter(|v| !v.trim().is_empty())
            .with_context(|| format!("{}: {key} is required", self.kind))
    }

    fn allowed(&self, keys: &[&str]) -> anyhow::Result<()> {
        for key in self.table.keys() {
            if key != "type" && !keys.contains(&key.as_str()) {
                bail!(
                    "{}: unknown setting {key:?} (known: {})",
                    self.kind,
                    keys.join(", ")
                );
            }
        }
        Ok(())
    }

    fn timeout(&self) -> anyhow::Result<Duration> {
        match self.table.get("timeout_ms") {
            None => Ok(TIMEOUT),
            Some(v) => v
                .as_integer()
                .and_then(|ms| u64::try_from(ms).ok())
                .filter(|ms| (1..=300_000).contains(ms))
                .map(Duration::from_millis)
                .with_context(|| format!("{}: timeout_ms must be 1-300000", self.kind)),
        }
    }

    fn strings(&self, key: &str) -> anyhow::Result<Vec<String>> {
        let wrong = || format!("{}: {key} must be a list of strings", self.kind);
        let list: Vec<String> = self
            .table
            .get(key)
            .and_then(toml::Value::as_array)
            .with_context(wrong)?
            .iter()
            .map(|v| v.as_str().map(str::to_owned).with_context(wrong))
            .collect::<anyhow::Result<_>>()?;
        anyhow::ensure!(!list.is_empty(), "{}: {key} must not be empty", self.kind);
        Ok(list)
    }
}

impl Action {
    /// Parses an action table, e.g. `{ type = "http", url = "…" }`.
    pub fn parse(value: &toml::Value, allow_commands: bool) -> anyhow::Result<Self> {
        let table = value
            .as_table()
            .context("an action must be a table with a `type`")?;
        let kind = table.get("type").and_then(toml::Value::as_str).context(
            "an action needs `type` (http, command, mode, brightness, homeassistant, lyrion, multi, toggle, assist)",
        )?;
        let f = Fields { kind, table };
        match kind {
            "http" => Self::parse_http(&f),
            "command" => {
                f.allowed(&["argv", "timeout_ms"])?;
                anyhow::ensure!(
                    allow_commands,
                    "command actions are disabled; set allow_commands = true in [modules.keys] to allow them"
                );
                Ok(Self::Command {
                    argv: f.strings("argv")?,
                    timeout: f.timeout()?,
                })
            }
            "mode" => {
                f.allowed(&["id"])?;
                Ok(Self::Mode(f.need("id")?))
            }
            "profile" => {
                f.allowed(&["name"])?;
                Ok(Self::Profile(f.need("name")?))
            }
            "brightness" => Self::parse_brightness(&f),
            "audio" => Self::parse_audio(&f),
            "homeassistant" => Self::parse_homeassistant(&f),
            "lyrion" => {
                f.allowed(&["command", "player"])?;
                let player = match table.get("player") {
                    None => None,
                    Some(v) => Some(
                        v.as_str()
                            .filter(|p| !p.trim().is_empty())
                            .context("lyrion: player must be a player name or MAC")?
                            .to_owned(),
                    ),
                };
                Ok(Self::Lyrion {
                    command: f.strings("command")?,
                    player,
                })
            }
            "multi" => {
                f.allowed(&["steps"])?;
                let steps = table
                    .get("steps")
                    .and_then(toml::Value::as_array)
                    .context("multi: steps must be a list")?;
                anyhow::ensure!(!steps.is_empty(), "multi: steps must not be empty");
                Ok(Self::Multi(
                    steps
                        .iter()
                        .enumerate()
                        .map(|(i, step)| {
                            Step::parse(step, allow_commands)
                                .with_context(|| format!("multi: step {}", i + 1))
                        })
                        .collect::<anyhow::Result<_>>()?,
                ))
            }
            "toggle" => Self::parse_toggle(&f, allow_commands),
            "assist" => {
                f.allowed(&["mode"])?;
                let mode = match f.text("mode")?.as_deref() {
                    None | Some("hold") => crate::assist::Mode::Hold,
                    Some("tap") => crate::assist::Mode::Tap,
                    Some(other) => bail!("assist: mode must be \"hold\" or \"tap\", not {other:?}"),
                };
                Ok(Self::Assist(mode))
            }
            other => bail!(
                "unknown action type {other:?} (known: http, command, mode, profile, brightness, homeassistant, lyrion, multi, toggle, assist)"
            ),
        }
    }

    fn parse_http(f: &Fields) -> anyhow::Result<Self> {
        f.allowed(&["method", "url", "body", "headers", "timeout_ms"])?;
        let url = f.need("url")?;
        anyhow::ensure!(
            url.starts_with("http://") || url.starts_with("https://"),
            "http: url must start with http:// or https://"
        );
        let method = f
            .text("method")?
            .unwrap_or_else(|| "POST".to_owned())
            .to_uppercase();
        let method = reqwest::Method::from_bytes(method.as_bytes()).context("http: invalid method")?;
        let headers = match f.table.get("headers") {
            None => Vec::new(),
            Some(v) => v
                .as_table()
                .context("http: headers must be a table")?
                .iter()
                .map(|(k, v)| {
                    let v = v.as_str().context("http: header values must be strings")?;
                    Ok((k.clone(), v.to_owned()))
                })
                .collect::<anyhow::Result<_>>()?,
        };
        Ok(Self::Http {
            method,
            url,
            body: f.text("body")?,
            headers,
            timeout: f.timeout()?,
        })
    }

    fn parse_brightness(f: &Fields) -> anyhow::Result<Self> {
        f.allowed(&["set", "step"])?;
        match (f.table.get("set"), f.table.get("step")) {
            (Some(set), None) => Ok(Self::BrightnessSet(
                set.as_integer()
                    .and_then(|v| u8::try_from(v).ok())
                    .filter(|v| *v <= 100)
                    .context("brightness: set must be 0-100")?,
            )),
            (None, Some(step)) => Ok(Self::BrightnessStep(
                step.as_integer()
                    .and_then(|v| i32::try_from(v).ok())
                    .filter(|v| (-100..=100).contains(v) && *v != 0)
                    .context("brightness: step must be -100..100, not 0")?,
            )),
            _ => bail!("brightness: give either set or step"),
        }
    }

    fn parse_audio(f: &Fields) -> anyhow::Result<Self> {
        f.allowed(&["step", "mute"])?;
        match (f.table.get("step"), f.table.get("mute")) {
            (Some(step), None) => Ok(Self::AudioStep(
                step.as_integer()
                    .and_then(|v| i32::try_from(v).ok())
                    .filter(|v| (-100..=100).contains(v) && *v != 0)
                    .context("audio: step must be -100..100, not 0")?,
            )),
            (None, Some(mute)) => {
                anyhow::ensure!(mute.as_str() == Some("toggle"), "audio: mute must be \"toggle\"");
                Ok(Self::AudioMute)
            }
            _ => bail!("audio: give either step (volume per detent) or mute = \"toggle\""),
        }
    }

    fn parse_homeassistant(f: &Fields) -> anyhow::Result<Self> {
        f.allowed(&["service", "data"])?;
        let service = f.need("service")?;
        let (domain, service) = service
            .split_once('.')
            .filter(|(d, s)| !d.is_empty() && !s.is_empty())
            .context("homeassistant: service must look like \"light.toggle\"")?;
        let data = match f.table.get("data") {
            None => Json::Object(serde_json::Map::new()),
            Some(v) => serde_json::to_value(v).context("homeassistant: invalid data")?,
        };
        // A placeholder like "light." would be accepted by HA and do nothing.
        let entities = match data.get("entity_id") {
            None => Vec::new(),
            Some(Json::String(e)) => vec![e.as_str()],
            Some(Json::Array(list)) => list.iter().filter_map(Json::as_str).collect(),
            Some(_) => bail!("homeassistant: entity_id must be a string or a list of strings"),
        };
        for entity in entities {
            anyhow::ensure!(
                entity
                    .split_once('.')
                    .is_some_and(|(d, o)| !d.is_empty() && !o.is_empty()),
                "homeassistant: entity_id {entity:?} must look like \"light.desk\""
            );
        }
        Ok(Self::HomeAssistant {
            domain: domain.to_owned(),
            service: service.to_owned(),
            data,
        })
    }

    fn parse_toggle(f: &Fields, allow_commands: bool) -> anyhow::Result<Self> {
        f.allowed(&[
            "on",
            "off",
            "on_label",
            "off_label",
            "on_image",
            "off_image",
            "state",
        ])?;
        let side = |key: &str| -> anyhow::Result<Box<Self>> {
            let v = f
                .table
                .get(key)
                .with_context(|| format!("toggle: {key} is required"))?;
            let action = Self::parse(v, allow_commands).with_context(|| format!("toggle: {key}"))?;
            anyhow::ensure!(
                !matches!(action, Self::Toggle { .. }),
                "toggle: {key} can't be another toggle"
            );
            anyhow::ensure!(
                !matches!(action, Self::Assist(_)),
                "toggle: assist works only directly on a key"
            );
            Ok(Box::new(action))
        };
        let (on, off) = (side("on")?, side("off")?);
        let state = match f.text("state")? {
            Some(entity) => {
                anyhow::ensure!(
                    entity
                        .split_once('.')
                        .is_some_and(|(d, o)| !d.is_empty() && !o.is_empty()),
                    "toggle: state must be a Home Assistant entity id like \"light.x\""
                );
                Some(entity)
            }
            None => match (on.entity(), off.entity()) {
                (Some(a), Some(b)) if a == b => Some(a.to_owned()),
                _ => None,
            },
        };
        Ok(Self::Toggle {
            on,
            off,
            on_label: f.text("on_label")?,
            off_label: f.text("off_label")?,
            on_image: image_name(f, "on_image")?,
            off_image: image_name(f, "off_image")?,
            state,
        })
    }

    /// The single `entity_id` a Home Assistant call targets.
    fn entity(&self) -> Option<&str> {
        match self {
            Self::HomeAssistant { data, .. } => data.get("entity_id").and_then(Json::as_str),
            _ => None,
        }
    }

    /// The Home Assistant entity a toggle follows.
    pub fn followed_entity(&self) -> Option<&str> {
        match self {
            Self::Toggle { state, .. } => state.as_deref(),
            _ => None,
        }
    }

    /// A short description for the activity feed, e.g. `POST https://x/hook` or `light.toggle`.
    pub fn describe(&self, detents: i32) -> String {
        match self {
            Self::Http { method, url, .. } => format!("{method} {}", subst(url, detents)),
            Self::Command { argv, .. } => format!("run {}", argv[0]),
            Self::Mode(id) => format!("mode {id}"),
            Self::Profile(name) => format!("profile {name}"),
            Self::BrightnessSet(percent) => format!("brightness {percent} %"),
            Self::BrightnessStep(step) => format!("brightness {:+} %", step.saturating_mul(detents)),
            Self::AudioStep(step) => format!("volume {:+} %", step.saturating_mul(detents)),
            Self::AudioMute => "mute".to_owned(),
            Self::HomeAssistant {
                domain,
                service,
                data,
            } => match data.get("entity_id").and_then(Json::as_str) {
                Some(entity) => format!("{domain}.{service} {entity}"),
                None => format!("{domain}.{service}"),
            },
            Self::Lyrion { command, player } => {
                let command: Vec<String> = command.iter().map(|c| subst(c, detents)).collect();
                match player {
                    Some(p) => format!("lyrion {} on {p}", command.join(" ")),
                    None => format!("lyrion {}", command.join(" ")),
                }
            }
            Self::Multi(steps) => format!("{} steps", steps.len()),
            Self::Toggle { .. } => "toggle".to_owned(),
            Self::Assist(crate::assist::Mode::Hold) => "assist".to_owned(),
            Self::Assist(crate::assist::Mode::Tap) => "assist (tap)".to_owned(),
        }
    }

    /// Whether this action (or a step of it) needs `[modules.homeassistant]` / `[modules.lyrion]`.
    pub fn needs(&self) -> (bool, bool) {
        match self {
            Self::HomeAssistant { .. } | Self::Assist(_) => (true, false),
            Self::Lyrion { .. } => (false, true),
            Self::Multi(steps) => steps.iter().fold((false, false), |(h, l), s| match s {
                Step::Run(a) => {
                    let (h2, l2) = a.needs();
                    (h || h2, l || l2)
                }
                Step::Delay(_) => (h, l),
            }),
            Self::Toggle { on, off, state, .. } => {
                let (a, b) = (on.needs(), off.needs());
                (a.0 || b.0 || state.is_some(), a.1 || b.1)
            }
            _ => (false, false),
        }
    }

    /// Runs the action. `detents` scales brightness steps (1 for a press); `{delta}` in strings is
    /// replaced by the signed detents, so a knob can drive e.g. `["mixer", "volume", "{delta}"]`.
    pub async fn run(&self, env: &Env, detents: i32) -> anyhow::Result<()> {
        match self {
            Self::Http {
                method,
                url,
                body,
                headers,
                timeout,
            } => {
                let mut req = env
                    .http
                    .request(method.clone(), subst(url, detents))
                    .timeout(*timeout);
                for (k, v) in headers {
                    req = req.header(k, subst(v, detents));
                }
                if let Some(body) = body {
                    req = req.body(subst(body, detents));
                }
                let status = req.send().await?.status();
                anyhow::ensure!(status.is_success(), "HTTP {status}");
                Ok(())
            }
            Self::Command { argv, timeout } => {
                anyhow::ensure!(env.allow_commands, "command actions are disabled");
                let argv: Vec<String> = argv.iter().map(|a| subst(a, detents)).collect();
                run_command(&argv, *timeout).await
            }
            Self::Mode(id) => {
                let modes = env.modes.get().context("the mode controller isn't running")?;
                modes.set(id).await.map_err(anyhow::Error::from)
            }
            Self::Profile(name) => {
                let modes = env.modes.get().context("the mode controller isn't running")?;
                modes.set_profile(name).await.map_err(anyhow::Error::from)
            }
            Self::BrightnessSet(percent) => set_brightness(env, |_| *percent).await,
            Self::BrightnessStep(step) => {
                let by = step.saturating_mul(detents);
                set_brightness(env, |current| {
                    u8::try_from((i32::from(current) + by).clamp(0, 100)).unwrap_or(100)
                })
                .await
            }
            Self::AudioStep(step) => {
                let audio = env
                    .hub
                    .audio()
                    .context("this streamdockd doesn't manage the audio")?;
                audio.step_volume(step.saturating_mul(detents)).await.map(drop)
            }
            Self::AudioMute => {
                let audio = env
                    .hub
                    .audio()
                    .context("this streamdockd doesn't manage the audio")?;
                audio.toggle_mute().await.map(drop)
            }
            Self::HomeAssistant {
                domain,
                service,
                data,
            } => call_service(env, domain, service, data).await,
            Self::Lyrion { command, player } => {
                let lms = env
                    .lyrion
                    .as_ref()
                    .context("[modules.lyrion] is not configured")?;
                let player = lyrion_player(lms, player.as_deref().unwrap_or(&lms.player)).await?;
                let command: Vec<Json> = command.iter().map(|c| Json::String(subst(c, detents))).collect();
                lms.client.request(&player, Json::Array(command)).await.map(drop)
            }
            Self::Multi(steps) => {
                for step in steps {
                    match step {
                        Step::Delay(d) => tokio::time::sleep(*d).await,
                        Step::Run(action) => Box::pin(action.run(env, detents)).await?,
                    }
                }
                Ok(())
            }
            Self::Toggle { .. } => bail!("a toggle runs one of its sides; the key keeps its state"),
            Self::Assist(_) => bail!("assist works only directly on a key"),
        }
    }
}

/// Runs `argv` without a shell; its stderr goes to the daemon's log.
async fn run_command(argv: &[String], timeout: Duration) -> anyhow::Result<()> {
    let mut child = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("cannot start {}", argv[0]))?;
    let Ok(status) = tokio::time::timeout(timeout, child.wait()).await else {
        let _ = child.kill().await;
        bail!("{} timed out after {timeout:?}", argv[0]);
    };
    let status = status?;
    anyhow::ensure!(status.success(), "{} exited with {status}", argv[0]);
    Ok(())
}

/// Whether a Home Assistant entity is "on" (`GET /api/states/<entity>`).
pub async fn homeassistant_state(env: &Env, entity: &str) -> anyhow::Result<bool> {
    let ha = env
        .homeassistant
        .as_ref()
        .context("[modules.homeassistant] is not configured")?;
    let token = std::fs::read_to_string(&ha.token_file)
        .with_context(|| format!("cannot read the Home Assistant token {}", ha.token_file.display()))?;
    let res = env
        .http
        .get(format!("{}/api/states/{entity}", ha.url))
        .bearer_auth(token.trim())
        .timeout(TIMEOUT)
        .send()
        .await?;
    let status = res.status();
    anyhow::ensure!(
        status.is_success(),
        "Home Assistant answered HTTP {status} for {entity}"
    );
    let body: Json = res.json().await?;
    Ok(body["state"] == "on")
}

/// `POST /api/services/<domain>/<service>` with the token of `[modules.homeassistant]`.
async fn call_service(env: &Env, domain: &str, service: &str, data: &Json) -> anyhow::Result<()> {
    let ha = env
        .homeassistant
        .as_ref()
        .context("[modules.homeassistant] is not configured")?;
    let token = std::fs::read_to_string(&ha.token_file)
        .with_context(|| format!("cannot read the Home Assistant token {}", ha.token_file.display()))?;
    let status = env
        .http
        .post(format!("{}/api/services/{domain}/{service}", ha.url))
        .bearer_auth(token.trim())
        .json(data)
        .timeout(TIMEOUT)
        .send()
        .await?
        .status();
    anyhow::ensure!(status.is_success(), "Home Assistant answered HTTP {status}");
    Ok(())
}

impl Step {
    fn parse(value: &toml::Value, allow_commands: bool) -> anyhow::Result<Self> {
        let is_delay = value
            .as_table()
            .and_then(|t| t.get("type"))
            .and_then(toml::Value::as_str)
            == Some("delay");
        if !is_delay {
            let action = Action::parse(value, allow_commands)?;
            anyhow::ensure!(
                !matches!(action, Action::Assist(_)),
                "assist works only directly on a key, not in a multi"
            );
            return Ok(Self::Run(action));
        }
        let table = value.as_table().expect("checked above");
        for key in table.keys() {
            anyhow::ensure!(
                key == "type" || key == "ms",
                "delay: unknown setting {key:?} (known: ms)"
            );
        }
        let ms = table
            .get("ms")
            .and_then(toml::Value::as_integer)
            .and_then(|ms| u64::try_from(ms).ok())
            .context("delay: ms is required")?;
        let delay = Duration::from_millis(ms);
        anyhow::ensure!(delay <= MAX_DELAY, "delay: at most {} ms", MAX_DELAY.as_millis());
        Ok(Self::Delay(delay))
    }
}

/// An optional key image name (H15-28).
fn image_name(f: &Fields, key: &str) -> anyhow::Result<Option<String>> {
    let name = f.text(key)?;
    if let Some(name) = &name {
        anyhow::ensure!(
            crate::assets::valid_name(name),
            "{}: {key} {name:?} is not an image name (lowercase letters, digits, - and _)",
            f.kind
        );
    }
    Ok(name)
}

/// Replaces `{delta}` with the signed detent count.
fn subst(text: &str, detents: i32) -> String {
    if text.contains("{delta}") {
        text.replace("{delta}", &format!("{detents:+}"))
    } else {
        text.to_owned()
    }
}

/// Sets the brightness of every attached dock to `f(current)`.
async fn set_brightness(env: &Env, f: impl Fn(u8) -> u8) -> anyhow::Result<()> {
    let devices = env.hub.devices();
    anyhow::ensure!(!devices.is_empty(), "no dock is attached");
    for device in devices {
        let current = u8::try_from(device.brightness).unwrap_or(100);
        env.hub
            .command(&device.id, Command::SetBrightness(f(current)))
            .await?;
    }
    Ok(())
}

/// The MAC of `player` (a name or MAC), looked up once per name.
async fn lyrion_player(lms: &LyrionTarget, player: &str) -> anyhow::Result<String> {
    let mut cached = lms.player_ids.lock().await;
    if let Some(id) = cached.get(player) {
        return Ok(id.clone());
    }
    let players = lms
        .client
        .request("", serde_json::json!(["players", "0", "100"]))
        .await?;
    let id = crate::modules::lyrion::find_player(&players, player)
        .with_context(|| format!("Lyrion player {player:?} not found"))?;
    cached.insert(player.to_owned(), id.clone());
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> anyhow::Result<Action> {
        let v: toml::Table = toml::from_str(&format!("a = {src}")).unwrap();
        Action::parse(&v["a"], false)
    }

    #[test]
    fn assist_only_directly_on_a_key() {
        assert_eq!(
            parse(r#"{ type = "assist" }"#).unwrap(),
            Action::Assist(crate::assist::Mode::Hold)
        );
        assert_eq!(
            parse(r#"{ type = "assist", mode = "tap" }"#).unwrap(),
            Action::Assist(crate::assist::Mode::Tap)
        );
        assert!(parse(r#"{ type = "assist", mode = "push" }"#).is_err());
        assert!(parse(r#"{ type = "assist", x = 1 }"#).is_err());
        assert!(parse(r#"{ type = "multi", steps = [{ type = "assist" }] }"#).is_err());
        assert!(
            parse(r#"{ type = "toggle", on = { type = "assist" }, off = { type = "mode", id = "x" } }"#)
                .is_err()
        );
    }

    #[test]
    fn parses_every_action_type() {
        assert!(
            matches!(parse(r#"{ type = "http", url = "https://x/hook" }"#).unwrap(), Action::Http { method, .. } if method == reqwest::Method::POST)
        );
        assert_eq!(
            parse(r#"{ type = "mode", id = "lyrion" }"#).unwrap(),
            Action::Mode("lyrion".into())
        );
        assert_eq!(
            parse(r#"{ type = "profile", name = "Evening/Music" }"#).unwrap(),
            Action::Profile("Evening/Music".into())
        );
        assert_eq!(
            parse(r#"{ type = "brightness", set = 40 }"#).unwrap(),
            Action::BrightnessSet(40)
        );
        assert_eq!(
            parse(r#"{ type = "brightness", step = -5 }"#).unwrap(),
            Action::BrightnessStep(-5)
        );
        assert!(matches!(
            parse(r#"{ type = "homeassistant", service = "light.toggle", data = { entity_id = "light.a" } }"#).unwrap(),
            Action::HomeAssistant { domain, service, data } if domain == "light" && service == "toggle" && data["entity_id"] == "light.a"
        ));
        assert_eq!(
            parse(r#"{ type = "lyrion", command = ["pause"] }"#).unwrap(),
            Action::Lyrion {
                command: vec!["pause".into()],
                player: None
            }
        );
        assert_eq!(
            parse(r#"{ type = "lyrion", command = ["pause"], player = "Kitchen Radio" }"#).unwrap(),
            Action::Lyrion {
                command: vec!["pause".into()],
                player: Some("Kitchen Radio".into())
            }
        );
        let multi = parse(r#"{ type = "multi", steps = [{ type = "brightness", set = 100 }, { type = "delay", ms = 300 }] }"#).unwrap();
        assert_eq!(
            multi,
            Action::Multi(vec![
                Step::Run(Action::BrightnessSet(100)),
                Step::Delay(Duration::from_millis(300))
            ])
        );
        let toggle = parse(r#"{ type = "toggle", on = { type = "brightness", set = 100 }, off = { type = "brightness", set = 10 }, on_label = "Bright" }"#).unwrap();
        assert!(matches!(toggle, Action::Toggle { on_label: Some(l), off_label: None, .. } if l == "Bright"));
    }

    #[test]
    fn toggles_follow_one_home_assistant_entity() {
        let ha = |service: &str, entity: &str| {
            format!(
                r#"{{ type = "homeassistant", service = "{service}", data = {{ entity_id = "{entity}" }} }}"#
            )
        };
        let same = parse(&format!(
            r#"{{ type = "toggle", on = {}, off = {} }}"#,
            ha("light.turn_on", "light.a"),
            ha("light.turn_off", "light.a")
        ))
        .unwrap();
        assert_eq!(same.followed_entity(), Some("light.a"));
        let different = parse(&format!(
            r#"{{ type = "toggle", on = {}, off = {} }}"#,
            ha("light.turn_on", "light.a"),
            ha("light.turn_off", "light.b")
        ))
        .unwrap();
        assert_eq!(different.followed_entity(), None);
        let explicit = parse(
            r#"{ type = "toggle", state = "switch.fan", on = { type = "mode", id = "a" }, off = { type = "mode", id = "b" } }"#,
        )
        .unwrap();
        assert_eq!(explicit.followed_entity(), Some("switch.fan"));
        assert_eq!(
            explicit.needs(),
            (true, false),
            "an explicit state needs Home Assistant"
        );
        assert!(parse(r#"{ type = "toggle", state = "fan", on = { type = "mode", id = "a" }, off = { type = "mode", id = "b" } }"#).is_err());
    }

    #[test]
    fn rejects_bad_actions() {
        for (src, why) in [
            (r#"{ type = "nope" }"#, "unknown action type"),
            (r#"{ type = "mode", id = "" }"#, "id is required"),
            (
                r#"{ type = "homeassistant", service = "light.toggle", data = { entity_id = "light." } }"#,
                "must look like \"light.desk\"",
            ),
            (
                r#"{ type = "homeassistant", service = "light.toggle", data = { entity_id = ["light.a", "b"] } }"#,
                "\"b\"",
            ),
            (r#"{ type = "profile", name = " " }"#, "name is required"),
            (r#"{ type = "http", url = "ftp://x" }"#, "http://"),
            (
                r#"{ type = "http", url = "https://x", colour = 1 }"#,
                "unknown setting",
            ),
            (r#"{ type = "command", argv = ["true"] }"#, "allow_commands"),
            (r#"{ type = "brightness", set = 101 }"#, "0-100"),
            (r#"{ type = "brightness" }"#, "either set or step"),
            (
                r#"{ type = "homeassistant", service = "toggle" }"#,
                "light.toggle",
            ),
            (r#"{ type = "multi", steps = [] }"#, "must not be empty"),
            (
                r#"{ type = "multi", steps = [{ type = "delay", ms = 999999 }] }"#,
                "step 1",
            ),
            (
                r#"{ type = "toggle", on = { type = "mode", id = "a" } }"#,
                "off is required",
            ),
        ] {
            let err = format!("{:#}", parse(src).unwrap_err());
            assert!(err.contains(why), "{src}: {err}");
        }
    }

    #[test]
    fn commands_need_the_opt_in() {
        let v: toml::Table = toml::from_str(r#"a = { type = "command", argv = ["true"] }"#).unwrap();
        assert!(Action::parse(&v["a"], true).is_ok());
    }

    #[test]
    fn delta_substitution() {
        assert_eq!(subst("{delta}", 3), "+3");
        assert_eq!(subst("vol {delta}", -2), "vol -2");
        assert_eq!(subst("plain", 5), "plain");
    }

    #[test]
    fn needs_reports_integrations_in_nested_actions() {
        let multi = parse(r#"{ type = "multi", steps = [{ type = "lyrion", command = ["pause"] }, { type = "delay", ms = 1 }] }"#).unwrap();
        assert_eq!(multi.needs(), (false, true));
        let toggle = parse(r#"{ type = "toggle", on = { type = "homeassistant", service = "light.turn_on" }, off = { type = "mode", id = "x" } }"#).unwrap();
        assert_eq!(toggle.needs(), (true, false));
    }
}

//! This machine's audio (H15-50), for the settings page and the dock: PipeWire outputs (sinks),
//! their volume and mute, which one plays, and a tone control (bass, mid, treble, balance). Only
//! with `[audio] manage = true` (the Luckfox).
//!
//! PipeWire runs as another user (`sound` on the Luckfox), so the daemon talks to that user's
//! PipeWire socket with its command-line tools: `pw-dump` reads the graph (and `pw-dump -m`
//! follows changes), `wpctl` sets volume, mute and the default output, native snapshots set the tone. The
//! daemon needs that user's group and the runtime directory open for it (`packaging/luckfox/`).
//!
//! Sound processing runs in the native player and Assist output device. Audio publishes
//! immutable controls on settings/volume changes; each stream compiles them at its own rate.
//! Other PipeWire clients bypass these controls.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde_json::Value;
use streamdock_proto::streamdock::v1 as pb;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Notify;

use crate::dsp::Dsp;
use crate::hub::Hub;
use crate::state::StateFile;

/// Highest volume the page offers, in percent (PipeWire allows more; 100 = unity).
pub const MAX_VOLUME: u32 = 150;
/// How long a PipeWire tool may take.
const TOOL_TIMEOUT: Duration = Duration::from_secs(5);
/// Changes arriving within this time are read once.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the Bluetooth bitrate is measured while a Bluetooth output plays.
const BITRATE_EVERY: Duration = Duration::from_secs(3);
/// Wait before following PipeWire again after `pw-dump -m` ended.
const RETRY: Duration = Duration::from_secs(5);

/// What `pw-dump` says about the outputs.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Graph {
    /// Every output except the tone filter, ordered by kind and name.
    pub outputs: Vec<Output>,
    /// Ids of every node, device and metadata object, to tell which removals matter.
    pub known: BTreeSet<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub kind: &'static str,
    pub selected: bool,
    pub volume_percent: u32,
    pub muted: bool,
    pub codec: String,
    pub running: bool,
    /// Sample rate PipeWire runs it at, e.g. 48000; 0 if unknown.
    pub sample_rate: u32,
}

/// Reads `pw-dump` output.
pub fn parse(dump: &str) -> anyhow::Result<Graph> {
    let objects: Vec<Value> = serde_json::from_str(dump).context("pw-dump printed no JSON")?;
    let default = default_sink(&objects);
    let mut graph = Graph::default();
    for o in &objects {
        let Some(id) = o["id"].as_u64().and_then(|id| u32::try_from(id).ok()) else {
            continue;
        };
        if relevant_type(o) {
            graph.known.insert(id);
        }
        if o["type"] != "PipeWire:Interface:Node" {
            continue;
        }
        let props = &o["info"]["props"];
        let name = props["node.name"].as_str().unwrap_or_default();
        // Hide legacy stages during the one-time packaging migration too.
        if props["media.class"] == "Audio/Sink" && !name.starts_with("streamdock_eq") {
            graph.outputs.push(output(id, name, o, default.as_deref()));
        }
    }
    graph
        .outputs
        .sort_by(|a, b| (rank(a.kind), &a.description).cmp(&(rank(b.kind), &b.description)));
    Ok(graph)
}

fn output(id: u32, name: &str, node: &Value, default: Option<&str>) -> Output {
    let props = &node["info"]["props"];
    let text = |key: &str| props[key].as_str().unwrap_or_default().to_owned();
    // The last Props entry carries the volume (wpctl reads the same).
    let volume = node["info"]["params"]["Props"]
        .as_array()
        .and_then(|list| list.iter().rev().find(|p| p.get("channelVolumes").is_some()));
    let linear = volume
        .and_then(|p| p["channelVolumes"].as_array())
        .map_or(1.0, |v| v.iter().filter_map(Value::as_f64).fold(0.0, f64::max));
    Output {
        id,
        name: name.to_owned(),
        description: [text("node.description"), text("node.nick"), name.to_owned()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or_default(),
        kind: kind(name, props),
        selected: default == Some(name),
        volume_percent: percent(linear),
        muted: volume.and_then(|p| p["mute"].as_bool()).unwrap_or(false),
        codec: text("api.bluez5.codec"),
        running: node["info"]["state"] == "running",
        sample_rate: node["info"]["params"]["Format"]
            .as_array()
            .and_then(|f| f.iter().find_map(|f| f["rate"].as_u64()))
            .and_then(|r| u32::try_from(r).ok())
            .unwrap_or(0),
    }
}

/// The default sink's node name, from the `default` metadata.
fn default_sink(objects: &[Value]) -> Option<String> {
    let entries =
        objects.iter().find(|o| {
            o["type"] == "PipeWire:Interface:Metadata" && o["props"]["metadata.name"] == "default"
        })?["metadata"]
            .as_array()?;
    let value = &entries.iter().find(|e| e["key"] == "default.audio.sink")?["value"];
    // Usually a JSON object; some versions store it as a JSON string.
    match value {
        Value::String(s) => serde_json::from_str::<Value>(s).ok()?["name"]
            .as_str()
            .map(str::to_owned),
        v => v["name"].as_str().map(str::to_owned),
    }
}

fn kind(name: &str, props: &Value) -> &'static str {
    if name.starts_with("bluez_") {
        "bluetooth"
    } else if props["device.bus"] == "usb" || name.contains("usb") {
        "usb"
    } else if name.starts_with("alsa_") {
        "builtin"
    } else {
        "other"
    }
}

fn rank(kind: &str) -> u8 {
    match kind {
        "bluetooth" => 0,
        "usb" => 1,
        "builtin" => 2,
        _ => 3,
    }
}

/// PipeWire's linear volume as wpctl's percent (cubic, 100 = unity), 0..=[`MAX_VOLUME`].
fn percent(linear: f64) -> u32 {
    let p = (linear.max(0.0).cbrt() * 100.0)
        .round()
        .clamp(0.0, f64::from(MAX_VOLUME));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=150 (H15-50)"
    )]
    let p = p as u32;
    p
}

/// The audio manager.
#[derive(Debug)]
pub struct Audio {
    /// The PipeWire user's runtime directory (holds the `pipewire-0` socket).
    runtime_dir: PathBuf,
    state: Arc<StateFile>,
    /// The graph as last read, with our own changes applied ahead of PipeWire: changes take their
    /// node ids from here, so a slider step costs one `wpctl`, not a `pw-dump` as well.
    graph: Mutex<Option<Graph>>,
    /// Changes not yet sent to PipeWire, per output; the latest wins (a dragged slider sends many).
    pending: Mutex<Pending>,
    /// Wakes [`Audio::apply_changes`].
    wake: Notify,
    /// The Bluetooth controller's bitrate, measured while a Bluetooth output plays.
    bitrate: Mutex<Bitrate>,
    controls: Arc<player::dsp::Controls>,
    /// Serializes control-plane gathering/publication, never read by the audio thread.
    publish: Mutex<()>,
}

#[derive(Debug, Default)]
struct Pending {
    outputs: BTreeMap<String, Change>,
}

#[derive(Debug, Default, Clone, Copy)]
struct Change {
    volume: Option<u32>,
    mute: Option<bool>,
}

#[derive(Debug, Default)]
struct Bitrate {
    /// When and at which byte count the controller was last read.
    last: Option<(Instant, u64)>,
    kbps: u32,
}

impl Audio {
    /// For PipeWire running as `user` (its runtime directory `/run/user/<uid>`).
    pub fn new(user: &str, state: Arc<StateFile>) -> anyhow::Result<Self> {
        let passwd = std::fs::read_to_string("/etc/passwd").context("cannot read /etc/passwd")?;
        let uid = uid_of(&passwd, user).with_context(|| format!("[audio] user {user:?} doesn't exist"))?;
        let controls = Arc::new(player::dsp::Controls::new(state.dsp().native_settings(100)));
        Ok(Self {
            runtime_dir: PathBuf::from(format!("/run/user/{uid}")),
            state,
            graph: Mutex::default(),
            pending: Mutex::default(),
            wake: Notify::new(),
            bitrate: Mutex::default(),
            controls,
            publish: Mutex::new(()),
        })
    }

    /// Runs a PipeWire tool against the PipeWire user's daemon and returns its output.
    async fn tool(&self, program: &str, args: &[&str]) -> anyhow::Result<String> {
        let run = tokio::process::Command::new(program)
            .args(args)
            .env("PIPEWIRE_RUNTIME_DIR", &self.runtime_dir)
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(TOOL_TIMEOUT, run)
            .await
            .with_context(|| format!("{program} took longer than {TOOL_TIMEOUT:?}"))?
            .with_context(|| format!("cannot run {program}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            bail!("{program} {} failed: {}", args.join(" "), err.trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Reads the graph from PipeWire and keeps it.
    async fn read_graph(&self) -> anyhow::Result<Graph> {
        let graph = parse(&self.tool("pw-dump", &[]).await?)?;
        *self.graph.lock().expect("audio lock poisoned") = Some(graph.clone());
        self.publish_dsp();
        Ok(graph)
    }

    /// The kept graph, read once if there is none yet.
    async fn cached(&self) -> anyhow::Result<Graph> {
        let kept = self.graph.lock().expect("audio lock poisoned").clone();
        match kept {
            Some(graph) => Ok(graph),
            None => self.read_graph().await,
        }
    }

    /// Shared immutable controls for native player/Assist devices.
    #[must_use]
    pub fn dsp_controls(&self) -> Arc<player::dsp::Controls> {
        self.controls.clone()
    }

    fn publish_dsp(&self) {
        let _publish = self.publish.lock().expect("audio publication lock poisoned");
        let volume = self
            .graph
            .lock()
            .expect("audio lock poisoned")
            .as_ref()
            .map_or(100, selected_volume);
        self.controls.publish(self.state.dsp().native_settings(volume));
    }

    /// The PipeWire node id of the output named `name` as last seen (the built-in player's fixed
    /// output, H15-61).
    #[must_use]
    pub fn output_node_id(&self, name: &str) -> Option<u32> {
        let graph = self.graph.lock().expect("audio lock poisoned");
        graph
            .as_ref()?
            .outputs
            .iter()
            .find(|o| o.name == name)
            .map(|o| o.id)
    }

    /// The sample rate of the output named `name` as last seen; 0 if unknown (H15-80).
    #[must_use]
    pub fn output_sample_rate(&self, name: &str) -> u32 {
        let graph = self.graph.lock().expect("audio lock poisoned");
        graph
            .as_ref()
            .and_then(|g| g.outputs.iter().find(|o| o.name == name))
            .map_or(0, |o| o.sample_rate)
    }

    /// The selected output's sample rate as last seen (what PipeWire runs it at); 0 if unknown.
    #[must_use]
    pub fn selected_sample_rate(&self) -> u32 {
        let graph = self.graph.lock().expect("audio lock poisoned");
        graph
            .as_ref()
            .and_then(|g| g.outputs.iter().find(|o| o.selected))
            .map_or(0, |o| o.sample_rate)
    }

    /// The `PIPEWIRE_RUNTIME_DIR` of the audio user, for PipeWire tools started elsewhere.
    #[must_use]
    pub fn runtime_dir(&self) -> &std::path::Path {
        &self.runtime_dir
    }

    /// The outputs and the tone, read from PipeWire; a problem is reported in the state.
    pub async fn state(&self) -> pb::AudioState {
        match self.read_graph().await {
            Ok(graph) => self.message(&graph),
            Err(e) => self.problem(&e),
        }
    }

    /// The outputs and the tone as kept (no PipeWire call), including changes still being applied.
    pub async fn current(&self) -> pb::AudioState {
        match self.cached().await {
            Ok(graph) => self.message(&graph),
            Err(e) => self.problem(&e),
        }
    }

    fn problem(&self, e: &anyhow::Error) -> pb::AudioState {
        pb::AudioState {
            tone: self.tone_message(false, 100).into(),
            problem: format!("{e:#}"),
            ..Default::default()
        }
    }

    fn message(&self, graph: &Graph) -> pb::AudioState {
        let kbps = self.bitrate.lock().expect("audio lock poisoned").kbps;
        pb::AudioState {
            outputs: graph
                .outputs
                .iter()
                .map(|o| pb::AudioOutput {
                    name: o.name.clone(),
                    description: o.description.clone(),
                    kind: o.kind.to_owned(),
                    selected: o.selected,
                    volume_percent: o.volume_percent,
                    muted: o.muted,
                    codec: o.codec.clone(),
                    running: o.running,
                    sample_rate: o.sample_rate,
                    bitrate_kbps: if o.kind == "bluetooth" && o.running {
                        kbps
                    } else {
                        0
                    },
                    ..Default::default()
                })
                .collect(),
            tone: self.tone_message(true, selected_volume(graph)).into(),
            presets: crate::dsp::PRESETS
                .iter()
                .map(|p| pb::AudioPreset {
                    id: p.id.to_owned(),
                    label: p.label.to_owned(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn tone_message(&self, available: bool, volume: u32) -> pb::AudioTone {
        let d = self.state.dsp();
        let (loudness_bass_db, loudness_treble_db) = d.loudness_db(volume);
        pb::AudioTone {
            bass_db: d.bass_db,
            mid_db: d.mid_db,
            treble_db: d.treble_db,
            balance: d.balance,
            available,
            warmth: d.warmth,
            energy: d.energy,
            loudness: d.loudness,
            correction: d.correction.clone(),
            bands: d
                .bands
                .iter()
                .map(|b| pb::AudioBand {
                    freq_hz: b.freq_hz,
                    gain_db: b.gain_db,
                    q: b.q,
                    ..Default::default()
                })
                .collect(),
            preamp_db: d.preamp_db,
            crossfeed: d.crossfeed,
            crossfeed_db: d.crossfeed_db,
            limit_db: d.limit_db,
            loudness_bass_db,
            loudness_treble_db,
            ..Default::default()
        }
    }

    /// Makes `name` the output everything plays on, chosen by the user: it goes first in the output
    /// priority, restored after audio restarts and reconnects (H15-79, `output_memory.rs`).
    pub async fn select(&self, name: &str) -> anyhow::Result<()> {
        let label = self.switch(name).await?;
        self.state
            .prefer_output(crate::output_memory::Chosen::of(name, &label));
        Ok(())
    }

    /// Makes `name` the output everything plays on, without changing the priority (the priority
    /// logic itself switching).
    pub async fn switch_to(&self, name: &str) -> anyhow::Result<()> {
        self.switch(name).await.map(drop)
    }

    /// `wpctl set-default`; returns the output's description.
    async fn switch(&self, name: &str) -> anyhow::Result<String> {
        let graph = self.cached().await?;
        let output = find(&graph, |o| o.name == name).with_context(|| format!("no audio output {name:?}"))?;
        self.tool("wpctl", &["set-default", &output.id.to_string()])
            .await?;
        Ok(output.description.clone())
    }

    /// Sets the volume (0..=[`MAX_VOLUME`] percent) and mute of output `name`. Returns at once; the
    /// change reaches PipeWire shortly after (only the latest of quick changes).
    pub async fn set_volume(&self, name: &str, percent: u32, muted: bool) -> anyhow::Result<()> {
        self.cached().await?;
        self.change(
            |o| o.name == name,
            |o| (Some(percent.min(MAX_VOLUME)), (o.muted != muted).then_some(muted)),
        )
        .with_context(|| format!("no audio output {name:?}"))?;
        Ok(())
    }

    /// Changes the volume of the selected output by `delta` percent (a knob detent); returns the new
    /// volume. Quick turns add up, since each step starts from the latest value.
    pub async fn step_volume(&self, delta: i32) -> anyhow::Result<u32> {
        self.cached().await?;
        let output = self
            .change(
                |o| o.selected,
                |o| {
                    let volume =
                        (i64::from(o.volume_percent) + i64::from(delta)).clamp(0, i64::from(MAX_VOLUME));
                    (u32::try_from(volume).ok(), None)
                },
            )
            .context("no audio output is selected")?;
        Ok(output.volume_percent)
    }

    /// Sets output `name`'s volume (`None`: the selected output's) from the built-in player
    /// (Lyrion's volume, H15-55), from the cached graph without waiting. `false` if there is no
    /// such output or nothing was read yet.
    pub fn set_player_volume(&self, name: Option<&str>, percent: u32) -> bool {
        let percent = percent.min(MAX_VOLUME);
        self.change(
            |o| name.map_or(o.selected, |n| o.name == n),
            |o| ((o.volume_percent != percent).then_some(percent), None),
        )
        .is_some()
    }

    /// Mutes or unmutes the selected output; returns whether it is muted now.
    pub async fn toggle_mute(&self) -> anyhow::Result<bool> {
        self.cached().await?;
        let output = self
            .change(|o| o.selected, |o| (None, Some(!o.muted)))
            .context("no audio output is selected")?;
        Ok(output.muted)
    }

    /// Applies `edit` (new volume, new mute) to the first kept output matching `which`, queues it
    /// for PipeWire and returns the output as changed; `None` if no output matches.
    fn change(
        &self,
        which: impl Fn(&Output) -> bool,
        edit: impl FnOnce(&Output) -> (Option<u32>, Option<bool>),
    ) -> Option<Output> {
        let output = {
            let mut graph = self.graph.lock().expect("audio lock poisoned");
            let output = graph.as_mut()?.outputs.iter_mut().find(|o| which(o))?;
            let (volume, mute) = edit(output);
            let mut pending = self.pending.lock().expect("audio lock poisoned");
            let change = pending.outputs.entry(output.name.clone()).or_default();
            if let Some(v) = volume {
                output.volume_percent = v;
                change.volume = Some(v);
            }
            if let Some(m) = mute {
                output.muted = m;
                change.mute = Some(m);
            }
            output.clone()
        };
        self.publish_dsp();
        self.wake.notify_one();
        Some(output)
    }

    /// Stores sound settings and publishes them directly to the native DSP.
    pub fn set_dsp(&self, dsp: Dsp) {
        self.state.set_dsp(dsp.clamped());
        self.publish_dsp();
    }

    /// Sends queued changes to PipeWire, forever; only the latest change per output counts.
    pub async fn apply_changes(self: Arc<Self>) {
        loop {
            self.wake.notified().await;
            loop {
                let Pending { outputs } =
                    std::mem::take(&mut *self.pending.lock().expect("audio lock poisoned"));
                if outputs.is_empty() {
                    break;
                }
                if let Err(e) = self.send(outputs).await {
                    tracing::warn!(error = %format!("{e:#}"), "cannot change the audio");
                }
            }
        }
    }

    async fn send(&self, outputs: BTreeMap<String, Change>) -> anyhow::Result<()> {
        let graph = self.cached().await?;
        for (name, change) in outputs {
            let Some(output) = find(&graph, |o| o.name == name) else {
                continue;
            };
            let id = output.id.to_string();
            if let Some(volume) = change.volume {
                let value = format!("{:.2}", f64::from(volume) / 100.0);
                self.tool("wpctl", &["set-volume", &id, &value]).await?;
            }
            if let Some(muted) = change.mute {
                self.tool("wpctl", &["set-mute", &id, if muted { "1" } else { "0" }])
                    .await?;
            }
        }
        Ok(())
    }

    /// Follows PipeWire forever: publishes the state on every change, measures the Bluetooth
    /// bitrate while it plays. Graph reads also publish loudness after external volume changes.
    pub async fn watch(self: Arc<Self>, hub: Arc<Hub>) {
        loop {
            if let Err(e) = self.follow(&hub).await {
                tracing::warn!(error = %format!("{e:#}"), "cannot follow PipeWire; retrying");
            }
            hub.set_audio(self.state().await);
            tokio::time::sleep(RETRY).await;
        }
    }

    /// Runs `pw-dump -m` until it ends, refreshing after each burst of changes.
    async fn follow(&self, hub: &Hub) -> anyhow::Result<()> {
        let mut child = tokio::process::Command::new("pw-dump")
            .arg("-m")
            .env("PIPEWIRE_RUNTIME_DIR", &self.runtime_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("cannot run pw-dump -m")?;
        let mut lines = BufReader::new(child.stdout.take().context("pw-dump has no stdout")?).lines();
        let mut chunks = Chunks::default();
        let mut tick = tokio::time::interval(BITRATE_EVERY);
        // No catch-up ticks after a slow refresh: two reads close together overstate the rate.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        self.refresh(hub).await;
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let line = line?.context("pw-dump -m ended")?;
                    let Some(update) = chunks.push(&line) else { continue };
                    if !self.matters(&update) {
                        continue;
                    }
                    debounce(&mut lines, &mut chunks).await?;
                    self.refresh(hub).await;
                }
                _ = tick.tick() => self.measure_bitrate(hub).await,
            }
        }
    }

    /// Whether a `pw-dump -m` update touches what we show: nodes, devices or metadata, or the
    /// removal of one we know. Not clients: every tool we run is one, and reacting to those would
    /// read the graph again and again (each read is another client).
    fn matters(&self, update: &str) -> bool {
        let Ok(objects) = serde_json::from_str::<Vec<Value>>(update) else {
            return true;
        };
        let graph = self.graph.lock().expect("audio lock poisoned");
        objects.iter().any(|o| {
            relevant_type(o)
                || (o["info"].is_null()
                    && o["id"]
                        .as_u64()
                        .and_then(|id| u32::try_from(id).ok())
                        .is_some_and(|id| graph.as_ref().is_none_or(|g| g.known.contains(&id))))
        })
    }

    async fn refresh(&self, hub: &Hub) {
        let graph = match self.read_graph().await {
            Ok(graph) => graph,
            Err(e) => {
                hub.set_audio(self.problem(&e));
                return;
            }
        };
        hub.set_audio(self.message(&graph));
    }

    /// While a Bluetooth output plays: the controller's transmitted bytes per second since the
    /// last tick (the codec's real bitrate, e.g. LDAC's adaptive 330-990 kbit/s).
    async fn measure_bitrate(&self, hub: &Hub) {
        let playing = self
            .graph
            .lock()
            .expect("audio lock poisoned")
            .as_ref()
            .is_some_and(|g| g.outputs.iter().any(|o| o.kind == "bluetooth" && o.running));
        let bytes = if playing {
            self.tool("hciconfig", &["hci0"])
                .await
                .ok()
                .and_then(|t| tx_bytes(&t))
        } else {
            None
        };
        {
            let mut rate = self.bitrate.lock().expect("audio lock poisoned");
            let now = Instant::now();
            match (bytes, rate.last) {
                // A short window overstates bursts: keep the last value and measure on.
                (Some(_), Some((at, _))) if now - at < BITRATE_EVERY / 2 => return,
                (Some(b), Some((at, before))) if b >= before => rate.kbps = kbps(b - before, now - at),
                _ => rate.kbps = 0,
            }
            rate.last = bytes.map(|b| (now, b));
        }
        if playing {
            hub.set_audio(self.current().await);
        }
    }
}

/// Takes the lines that arrive within [`DEBOUNCE`], so a burst of changes is read once.
async fn debounce(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    chunks: &mut Chunks,
) -> anyhow::Result<()> {
    let quiet = tokio::time::sleep(DEBOUNCE);
    tokio::pin!(quiet);
    loop {
        tokio::select! {
            () = &mut quiet => return Ok(()),
            line = lines.next_line() => {
                // Keep the framing; the refresh after the burst reads everything anyway.
                chunks.push(&line?.context("pw-dump -m ended")?);
            }
        }
    }
}

/// Splits `pw-dump -m` output into its updates: each is a JSON array from a line `[` to a line `]`.
#[derive(Debug, Default)]
struct Chunks {
    text: String,
}

impl Chunks {
    /// Adds a line; returns an update once its closing `]` arrives.
    fn push(&mut self, line: &str) -> Option<String> {
        if line == "[" {
            self.text.clear();
        }
        self.text.push_str(line);
        self.text.push('\n');
        (line == "]").then(|| std::mem::take(&mut self.text))
    }
}

/// Objects whose changes show in the audio state.
fn relevant_type(o: &Value) -> bool {
    matches!(
        o["type"].as_str(),
        Some("PipeWire:Interface:Node" | "PipeWire:Interface:Device" | "PipeWire:Interface:Metadata")
    )
}

/// The selected output's volume (100 when none is selected).
fn selected_volume(graph: &Graph) -> u32 {
    find(graph, |o| o.selected).map_or(100, |o| o.volume_percent)
}

fn find(graph: &Graph, which: impl Fn(&Output) -> bool) -> Option<&Output> {
    graph.outputs.iter().find(|o| which(o))
}

/// "TX bytes:120181975" in `hciconfig` output.
fn tx_bytes(text: &str) -> Option<u64> {
    let rest = &text[text.find("TX bytes:")? + "TX bytes:".len()..];
    rest.split_whitespace().next()?.parse().ok()
}

/// Bytes over a duration as kbit/s.
fn kbps(bytes: u64, over: Duration) -> u32 {
    let ms = u64::try_from(over.as_millis()).unwrap_or(u64::MAX).max(1);
    u32::try_from(bytes.saturating_mul(8) / ms).unwrap_or(u32::MAX)
}

/// The uid of `user` in `/etc/passwd` text.
fn uid_of(passwd: &str, user: &str) -> Option<u32> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next()? == user).then_some(())?;
        fields.nth(1)?.parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed `pw-dump` of the Luckfox with a Bluetooth headset, the built-in codec and the filter.
    const DUMP: &str = r#"[
      { "id": 0, "type": "PipeWire:Interface:Core", "info": {} },
      { "id": 30, "type": "PipeWire:Interface:Metadata", "props": { "metadata.name": "default" },
        "metadata": [
          { "subject": 0, "key": "default.audio.sink", "type": "Spa:String:JSON",
            "value": { "name": "bluez_output.02_00_00_00_00_B4.a2dp-sink" } } ] },
      { "id": 40, "type": "PipeWire:Interface:Node", "info": { "state": "suspended",
          "props": { "node.name": "alsa_output.platform-acodec-sound.stereo-fallback",
                     "node.description": "Built-in Audio Stereo", "media.class": "Audio/Sink" },
          "params": { "Props": [ { "volume": 1.0, "mute": false, "channelVolumes": [ 0.405, 0.405 ] } ] } } },
      { "id": 52, "type": "PipeWire:Interface:Node", "info": { "state": "running",
          "props": { "node.name": "bluez_output.02_00_00_00_00_B4.a2dp-sink", "node.description": "WH-1000XM4",
                     "media.class": "Audio/Sink", "api.bluez5.codec": "ldac" },
          "params": { "Props": [ { "mute": true, "channelVolumes": [ 0.389, 0.389 ] } ] } } },
      { "id": 58, "type": "PipeWire:Interface:Node", "info": { "state": "running",
          "props": { "node.name": "streamdock_eq", "node.description": "Streamdock EQ", "media.class": "Audio/Sink" } } },
      { "id": 59, "type": "PipeWire:Interface:Node", "info": { "state": "running",
          "props": { "node.name": "streamdock_eq.output", "media.class": "Stream/Output/Audio" } } },
      { "id": 60, "type": "PipeWire:Interface:Node", "info": { "state": "running",
          "props": { "node.name": "streamdock_eq.2", "node.description": "Streamdock DSP (2/5)", "media.class": "Audio/Sink" } } },
      { "id": 33, "type": "PipeWire:Interface:Node", "info": { "state": "running",
          "props": { "node.name": "PipeWire ALSA [squeezelite]", "media.class": "Stream/Output/Audio" } } }
    ]"#;

    #[test]
    fn reads_outputs_default_volume_and_the_filter() {
        let g = parse(DUMP).unwrap();
        let names: Vec<_> = g.outputs.iter().map(|o| o.description.as_str()).collect();
        assert_eq!(
            names,
            ["WH-1000XM4", "Built-in Audio Stereo"],
            "bluetooth first, no filter, no streams"
        );
        let bt = &g.outputs[0];
        assert!(bt.selected && bt.muted && bt.running);
        assert_eq!(
            (bt.kind, bt.codec.as_str(), bt.volume_percent),
            ("bluetooth", "ldac", 73)
        );
        let builtin = &g.outputs[1];
        assert!(!builtin.selected && !builtin.muted && !builtin.running);
        assert_eq!((builtin.kind, builtin.volume_percent), ("builtin", 74));
    }

    #[test]
    fn a_default_stored_as_a_json_string_is_read_too() {
        let dump = DUMP.replace(
            r#""value": { "name": "bluez_output.02_00_00_00_00_B4.a2dp-sink" }"#,
            r#""value": "{\"name\":\"alsa_output.platform-acodec-sound.stereo-fallback\"}""#,
        );
        let g = parse(&dump).unwrap();
        assert!(g.outputs[1].selected && !g.outputs[0].selected);
    }

    #[test]
    fn reads_the_sample_rate_and_the_controller_bytes() {
        let dump = DUMP.replace(
            r#""params": { "Props": [ { "mute": true"#,
            r#""params": { "Format": [ { "format": "F32LE", "rate": 48000, "channels": 2 } ], "Props": [ { "mute": true"#,
        );
        let g = parse(&dump).unwrap();
        assert_eq!((g.outputs[0].sample_rate, g.outputs[1].sample_rate), (48000, 0));
        let hci = "hci0:	Type: Primary  Bus: UART
	RX bytes:899307 acl:1180 sco:0 events:107117 errors:0
	TX bytes:120181975 acl:177649 sco:0 commands:494 errors:0
";
        assert_eq!(tx_bytes(hci), Some(120_181_975));
        assert_eq!(kbps(375_000, Duration::from_secs(3)), 1000);
    }

    #[test]
    fn splits_monitor_updates_and_ignores_clients() {
        let mut chunks = Chunks::default();
        let mut updates = Vec::new();
        for line in "[\n  { \"id\": 70, \"type\": \"PipeWire:Interface:Client\" }\n]\n[\n  { \"id\": 52, \"info\": null }\n]".lines() {
            updates.extend(chunks.push(line));
        }
        assert_eq!(updates.len(), 2);
        let objects: Vec<Value> = serde_json::from_str(&updates[0]).unwrap();
        assert!(!relevant_type(&objects[0]), "a client");
        let g = parse(DUMP).unwrap();
        assert!(g.known.contains(&52) && g.known.contains(&30) && !g.known.contains(&0));
    }

    #[test]
    fn finds_the_uid() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\nsound:x:1001:1001::/home/sound:/usr/sbin/nologin\n";
        assert_eq!(uid_of(passwd, "sound"), Some(1001));
        assert_eq!(uid_of(passwd, "nobody"), None);
    }
}

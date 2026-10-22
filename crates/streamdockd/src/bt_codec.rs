//! The Bluetooth audio codec per device (H15-78): which A2DP codecs PipeWire offers for a
//! connected headset, which one is active and, for LDAC, its quality; choosing one and
//! remembering the choice per device address (`state.rs`), reapplied whenever it connects.
//!
//! PipeWire 0.3.48 offers one device profile per codec (`a2dp-sink-ldac`, `a2dp-sink-sbc_xq`,
//! `a2dp-sink-sbc`, …; the automatic `a2dp-sink` picks the best). Choosing a codec sets the
//! bluez device's `Profile` (`save: true`, so WirePlumber also remembers it); PipeWire then
//! reconfigures the stream, a short gap in the audio.
//!
//! LDAC's quality (auto/hq/sq/mq) is the `quality` property of the Bluetooth sink node. The
//! node's adapter forwards `Props` to its audio converter too, whose resampler also has a
//! `quality`. So once the encoder reads back the new quality, a second write restores the
//! resampler's through its own `params` key (read back, retried). One combined write doesn't
//! work: the plain `quality` wins over `params` (H15-78, on the board).
//!
//! The link's sample rate: the audio graph runs at 48 kHz. When we configure the link, PipeWire
//! prefers 48 kHz (`bluez5.default.rate`), but a headset that connects by itself configures it,
//! and the WH-1000XM4 picks 96 kHz, which the sink then resamples to. Unless the device keeps its
//! own rate (`state.rs`), such a link is renegotiated once per connect: another codec and back,
//! so PipeWire configures it (choosing the same codec again changes nothing in 0.3.48).
//!
//! Every switch recreates the sink node: the new one gets the old one's volume and mute. A
//! headset that reconnects at volume 0 soon after such a switch (the WH-1000XM4 did, reporting
//! its own volume as 0) gets the volume from before the switch back.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use serde_json::Value;

use crate::hub::{Hub, HubEvent};
use crate::state::StateFile;

/// How long the tools may take.
const TOOL_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a profile switch may take before the new node shows up.
const SWITCH_TIMEOUT: Duration = Duration::from_secs(8);
/// The audio graph's rate, which the link should run at.
pub const GRAPH_RATE: u32 = 48_000;
/// At most one automatic renegotiation per device in this time: a headset that insists on its
/// own rate must not be bounced in a loop.
const RENEGOTIATE_EVERY: Duration = Duration::from_secs(60);
/// How long after a switch a reconnect at volume 0 counts as caused by it.
const RECONNECT_AFTER_SWITCH: Duration = Duration::from_secs(120);
/// The pause between the two switches of a renegotiation, so the headset settles in between.
const RENEGOTIATE_PAUSE: Duration = Duration::from_secs(1);

/// LDAC's quality modes: `quality` values of PipeWire's LDAC encoder.
pub const LDAC_QUALITIES: [(&str, i64); 4] = [("auto", -1), ("hq", 0), ("sq", 1), ("mq", 2)];

/// A codec the device offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Codec {
    /// PipeWire's codec id, e.g. `ldac`, `sbc_xq`.
    pub id: String,
    /// e.g. "LDAC", "SBC-XQ".
    pub label: String,
    profile_index: i64,
}

/// A sink node's volume: its channel volumes (linear, as PipeWire stores them) and mute.
#[derive(Debug, Clone, PartialEq)]
pub struct Volume {
    channels: Vec<f64>,
    mute: bool,
}

impl Volume {
    fn silent(&self) -> bool {
        self.channels.iter().all(|v| *v <= 0.0)
    }

    /// Equal within what a readback rounds to.
    fn same(&self, other: &Self) -> bool {
        self.mute == other.mute
            && self.channels.len() == other.channels.len()
            && self
                .channels
                .iter()
                .zip(&other.channels)
                .all(|(a, b)| (a - b).abs() < 1e-4)
    }

    /// The `Props` that set it: `{ mute: false, channelVolumes: [ 0.06, 0.06 ] }`.
    fn props(&self) -> String {
        let channels: Vec<String> = self.channels.iter().map(f64::to_string).collect();
        format!(
            "{{ mute: {}, channelVolumes: [ {} ] }}",
            self.mute,
            channels.join(", ")
        )
    }
}

/// What PipeWire says about one Bluetooth device's audio.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCodecs {
    device_id: i64,
    pub codecs: Vec<Codec>,
    /// The codec in use (`None` while no A2DP profile is active).
    pub active: Option<String>,
    /// LDAC's quality ("auto", "hq", …) while LDAC is active.
    pub ldac_quality: Option<String>,
    /// The link's sample rate in Hz (the sink node's format).
    pub link_rate: Option<u32>,
    volume: Option<Volume>,
    node_id: Option<i64>,
    /// The node's `object.serial`: unlike its id, never reused for the next node.
    node_serial: Option<i64>,
    resample_quality: Option<i64>,
    profile_index: Option<i64>,
}

/// A remembered choice.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Preference {
    pub codec: String,
    /// For LDAC; `None` = auto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ldac_quality: Option<String>,
}

fn int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| {
        v.as_f64().map(|f| {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "PipeWire ids and qualities are small"
            )]
            let i = f as i64;
            i
        })
    })
}

/// The address as PipeWire spells it in node names: `02_00_00_00_00_B4`.
fn same_address(a: &str, b: &str) -> bool {
    a.replace('_', ":").eq_ignore_ascii_case(&b.replace('_', ":"))
}

/// Reads `address`'s codecs from a `pw-dump`; `None` when PipeWire has no such device.
#[must_use]
pub fn parse(dump: &str, address: &str) -> Option<DeviceCodecs> {
    let objects: Vec<Value> = serde_json::from_str(dump).ok()?;
    let device = objects.iter().find(|o| {
        let p = &o["info"]["props"];
        o["type"] == "PipeWire:Interface:Device"
            && p["device.api"] == "bluez5"
            && p["api.bluez5.address"]
                .as_str()
                .is_some_and(|a| same_address(a, address))
    })?;
    let device_id = int(&device["id"])?;
    let params = &device["info"]["params"];
    let codecs = params["EnumProfile"]
        .as_array()
        .map(|list| list.iter().filter_map(codec_of).collect())
        .unwrap_or_default();
    let profile = params["Profile"][0]["name"].as_str().unwrap_or_default();
    let profile_index = int(&params["Profile"][0]["index"]);
    let node = objects.iter().find(|o| {
        let p = &o["info"]["props"];
        o["type"] == "PipeWire:Interface:Node"
            && p["media.class"] == "Audio/Sink"
            && int(&p["device.id"]) == Some(device_id)
    });
    let node_codec = node.and_then(|n| n["info"]["props"]["api.bluez5.codec"].as_str());
    let active = match profile.strip_prefix("a2dp-sink-") {
        Some(codec) => Some(codec.to_owned()),
        None if profile == "a2dp-sink" => node_codec.map(str::to_owned),
        None => None,
    };
    let (codec_quality, resample_quality) = node.map(node_qualities).unwrap_or_default();
    let ldac_quality = (active.as_deref() == Some("ldac"))
        .then(|| codec_quality.and_then(quality_name))
        .flatten()
        .map(str::to_owned);
    Some(DeviceCodecs {
        device_id,
        codecs,
        active,
        ldac_quality,
        link_rate: node.and_then(node_rate),
        volume: node.and_then(node_volume),
        node_id: node.and_then(|n| int(&n["id"])),
        node_serial: node.and_then(|n| int(&n["info"]["props"]["object.serial"])),
        resample_quality,
        profile_index,
    })
}

/// The sink node's volume, from its `Props` object with `channelVolumes`.
fn node_volume(node: &Value) -> Option<Volume> {
    let props = node["info"]["params"]["Props"].as_array()?;
    let o = props.iter().find(|o| o.get("channelVolumes").is_some())?;
    let channels = o["channelVolumes"]
        .as_array()?
        .iter()
        .map(Value::as_f64)
        .collect::<Option<Vec<f64>>>()?;
    Some(Volume {
        channels,
        mute: o["mute"].as_bool().unwrap_or(false),
    })
}

/// The sink node's rate: its one `EnumFormat`, a plain number or a choice with a default.
fn node_rate(node: &Value) -> Option<u32> {
    let rate = &node["info"]["params"]["EnumFormat"][0]["rate"];
    let rate = int(rate).or_else(|| int(&rate["default"]))?;
    u32::try_from(rate).ok().filter(|r| *r > 0)
}

/// An `a2dp-sink-<codec>` profile as a codec ("High Fidelity Playback (A2DP Sink, codec LDAC)").
fn codec_of(profile: &Value) -> Option<Codec> {
    let id = profile["name"].as_str()?.strip_prefix("a2dp-sink-")?.to_owned();
    if profile["available"] == "no" {
        return None;
    }
    let label = profile["description"]
        .as_str()
        .and_then(|d| d.split("codec ").nth(1))
        .map_or_else(|| id.to_uppercase(), |l| l.trim_end_matches(')').to_owned());
    Some(Codec {
        id,
        label,
        profile_index: int(&profile["index"])?,
    })
}

/// The node's `Props`: the Bluetooth sink's object holds only `quality` (the codec's); the
/// converter's holds `rate` and the resampler's `quality`.
fn node_qualities(node: &Value) -> (Option<i64>, Option<i64>) {
    let Some(props) = node["info"]["params"]["Props"].as_array() else {
        return (None, None);
    };
    let only_quality = |o: &&Value| {
        o.as_object()
            .is_some_and(|m| m.len() == 1 && m.contains_key("quality"))
    };
    let codec = props.iter().find(only_quality).and_then(|o| int(&o["quality"]));
    let resample = props
        .iter()
        .find(|o| o.get("rate").is_some())
        .and_then(|o| int(&o["quality"]));
    (codec, resample)
}

fn quality_name(q: i64) -> Option<&'static str> {
    LDAC_QUALITIES.iter().find(|(_, v)| *v == q).map(|(n, _)| *n)
}

fn quality_value(name: &str) -> Option<i64> {
    LDAC_QUALITIES.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

/// The service: reads and sets codecs, and reapplies the remembered ones when a headset connects.
#[derive(Debug)]
pub struct BtCodecs {
    hub: Arc<Hub>,
    state: Arc<StateFile>,
    /// One change at a time: a profile switch recreates the sink node, which looks like a
    /// connect to the watcher; its reapply waits here and then finds nothing to do.
    busy: tokio::sync::Mutex<()>,
    /// When each device's link was last renegotiated automatically.
    renegotiated: Mutex<HashMap<String, Instant>>,
    /// Each device's last switch by us and its volume before it.
    switched: Mutex<HashMap<String, (Instant, Volume)>>,
}

impl BtCodecs {
    #[must_use]
    pub fn new(hub: Arc<Hub>, state: Arc<StateFile>) -> Arc<Self> {
        Arc::new(Self {
            hub,
            state,
            busy: tokio::sync::Mutex::new(()),
            renegotiated: Mutex::new(HashMap::new()),
            switched: Mutex::new(HashMap::new()),
        })
    }

    /// The device's codecs right now; `None` when PipeWire doesn't know it (not connected).
    pub async fn get(&self, address: &str) -> anyhow::Result<Option<DeviceCodecs>> {
        Ok(parse(&self.tool("pw-dump", &[]).await?, address))
    }

    /// The remembered choice for `address`.
    #[must_use]
    pub fn preference(&self, address: &str) -> Option<Preference> {
        self.state.bt_codec(address)
    }

    /// Chooses `codec` (and LDAC's quality) for `address`, applies it if connected and remembers it.
    pub async fn set(&self, address: &str, preference: Preference) -> anyhow::Result<()> {
        if let Some(q) = &preference.ldac_quality
            && quality_value(q).is_none()
        {
            bail!("unknown LDAC quality {q:?} (auto, hq, sq or mq)");
        }
        let _busy = self.busy.lock().await;
        if let Some(now) = self.get(address).await? {
            self.apply(address, &now, &preference).await?;
        }
        self.state.set_bt_codec(address, Some(preference));
        Ok(())
    }

    /// Back to PipeWire's automatic choice: forgets the preference and selects `a2dp-sink`.
    pub async fn reset(&self, address: &str) -> anyhow::Result<()> {
        let _busy = self.busy.lock().await;
        self.state.set_bt_codec(address, None);
        if let Some(now) = self.get(address).await? {
            let dump = self.tool("pw-dump", &[]).await?;
            if let Some(index) = auto_profile_index(&dump, now.device_id)
                && now.profile_index != Some(index)
            {
                self.switch(address, &now, index, None).await?;
            }
        }
        Ok(())
    }

    /// Whether `address` keeps the link rate its headset chooses (otherwise 48 kHz).
    #[must_use]
    pub fn any_rate(&self, address: &str) -> bool {
        self.state.bt_any_rate(address)
    }

    /// Keeps `address`'s link at 48 kHz (`any` false) or at the headset's rate, remembered; a
    /// connected link at another rate is renegotiated now.
    pub async fn set_link_rate(&self, address: &str, any: bool) -> anyhow::Result<()> {
        let _busy = self.busy.lock().await;
        self.state.set_bt_any_rate(address, any);
        if any {
            return Ok(());
        }
        let Some(now) = self.get(address).await? else {
            return Ok(());
        };
        if wrong_rate(&now) {
            let now = self.renegotiate(address, &now).await?;
            self.keep_choice(address, &now).await?;
        }
        Ok(())
    }

    /// Switches to another codec and back, so that PipeWire configures the link at 48 kHz.
    async fn renegotiate(&self, address: &str, now: &DeviceCodecs) -> anyhow::Result<DeviceCodecs> {
        let active = now.active.clone().context("no A2DP codec active")?;
        let back = now.profile_index.context("no active profile")?;
        let other = now
            .codecs
            .iter()
            .filter(|c| c.id != active)
            .min_by_key(|c| c.id != "sbc")
            .context("the device offers no other codec to renegotiate the rate with")?;
        let was = now.link_rate.unwrap_or_default();
        let between = self
            .switch(address, now, other.profile_index, Some(&other.id))
            .await?;
        tokio::time::sleep(RENEGOTIATE_PAUSE).await;
        let after = self.switch(address, &between, back, Some(&active)).await?;
        let rate = after.link_rate.unwrap_or_default();
        tracing::info!(address, codec = %active, was, rate, "Bluetooth link renegotiated");
        if rate != GRAPH_RATE {
            bail!("the headset stays at {rate} Hz with {active}");
        }
        Ok(after)
    }

    /// Restores the remembered choice after a renegotiation (LDAC's quality is per node).
    async fn keep_choice(&self, address: &str, now: &DeviceCodecs) -> anyhow::Result<()> {
        match self.preference(address) {
            Some(want) if !runs(now, &want) => self.apply(address, now, &want).await,
            _ => Ok(()),
        }
    }

    /// Whether an automatic renegotiation of `address` is allowed now (and notes it).
    fn may_renegotiate(&self, address: &str) -> bool {
        let mut last = self.renegotiated.lock().expect("renegotiation lock poisoned");
        let now = Instant::now();
        if last
            .get(address)
            .is_some_and(|t| now.duration_since(*t) < RENEGOTIATE_EVERY)
        {
            return false;
        }
        last.insert(address.to_owned(), now);
        true
    }

    async fn apply(&self, address: &str, now: &DeviceCodecs, want: &Preference) -> anyhow::Result<()> {
        let codec = now
            .codecs
            .iter()
            .find(|c| c.id == want.codec)
            .with_context(|| format!("the device doesn't offer {}", want.codec))?;
        let mut now = now.clone();
        if now.active.as_deref() != Some(codec.id.as_str()) {
            now = self
                .switch(address, &now, codec.profile_index, Some(&codec.id))
                .await?;
        }
        if codec.id == "ldac" {
            let quality = want.ldac_quality.as_deref().unwrap_or("auto");
            if now.ldac_quality.as_deref() != Some(quality) {
                self.set_ldac_quality(&now, quality).await?;
            }
        }
        tracing::info!(address, codec = %codec.id, quality = ?want.ldac_quality, "Bluetooth codec set");
        Ok(())
    }

    /// Switches `now`'s device to profile `index`, waits until a new node runs `codec` (any
    /// codec for `None`: the automatic profile) and gives that node `now`'s volume.
    async fn switch(
        &self,
        address: &str,
        now: &DeviceCodecs,
        index: i64,
        codec: Option<&str>,
    ) -> anyhow::Result<DeviceCodecs> {
        if let Some(volume) = &now.volume {
            self.switched
                .lock()
                .expect("switch lock poisoned")
                .insert(address.to_owned(), (Instant::now(), volume.clone()));
        }
        self.set_profile(now.device_id, index).await?;
        let mut after = self.wait_for_switch(address, codec, now.node_serial).await?;
        if let Some(volume) = &now.volume {
            self.keep_volume(&mut after, volume).await?;
        }
        Ok(after)
    }

    /// Sets `volume` on `now`'s node unless it has it already.
    async fn keep_volume(&self, now: &mut DeviceCodecs, volume: &Volume) -> anyhow::Result<()> {
        let Some(props) = volume_change(now.volume.as_ref(), volume) else {
            return Ok(());
        };
        let node = now.node_id.context("no Bluetooth sink node")?.to_string();
        self.tool("pw-cli", &["set-param", &node, "Props", &props])
            .await?;
        tracing::info!(node, was = ?now.volume, volume = ?volume, "Bluetooth volume carried over");
        now.volume = Some(volume.clone());
        Ok(())
    }

    /// The volume from before our last switch of `address`, if that was recent.
    fn volume_before_switch(&self, address: &str) -> Option<Volume> {
        let switched = self.switched.lock().expect("switch lock poisoned");
        let (at, volume) = switched.get(address)?;
        (at.elapsed() < RECONNECT_AFTER_SWITCH).then(|| volume.clone())
    }

    async fn set_profile(&self, device: i64, index: i64) -> anyhow::Result<()> {
        let param = format!("{{ index: {index}, save: true }}");
        self.tool("pw-cli", &["set-param", &device.to_string(), "Profile", &param])
            .await
            .map(drop)
    }

    /// Polls until the device runs `codec` (or any, for `None`) on a node other than the one
    /// with serial `old`.
    async fn wait_for_switch(
        &self,
        address: &str,
        codec: Option<&str>,
        old: Option<i64>,
    ) -> anyhow::Result<DeviceCodecs> {
        let deadline = tokio::time::Instant::now() + SWITCH_TIMEOUT;
        loop {
            if let Some(now) = self.get(address).await?
                && now.active.is_some()
                && codec.is_none_or(|c| now.active.as_deref() == Some(c))
                && now.node_id.is_some()
                && (old.is_none() || now.node_serial != old)
            {
                return Ok(now);
            }
            if tokio::time::Instant::now() > deadline {
                bail!(
                    "the headset didn't switch to {} (it may not accept it now)",
                    codec.unwrap_or("the automatic codec")
                );
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// Sets LDAC's quality on the sink node, restores the resampler's quality, checks both.
    async fn set_ldac_quality(&self, now: &DeviceCodecs, quality: &str) -> anyhow::Result<()> {
        let node = Node {
            codecs: self,
            id: now.node_id.context("no Bluetooth sink node")?,
        };
        let value = quality_value(quality).context("unknown LDAC quality")?;
        let restored = set_quality(&node, value, now.resample_quality, POLL)
            .await
            .with_context(|| format!("LDAC quality {quality}"))?;
        if !restored {
            tracing::warn!(expected = ?now.resample_quality, "the resampler's quality changed with the LDAC quality");
            self.hub.report(
                crate::hub::DAEMON,
                streamdock_proto::streamdock::v1::ActivityLevel::ACTIVITY_LEVEL_ERROR,
                "Bluetooth: the resampler's quality couldn't be restored after the LDAC quality".to_owned(),
            );
        }
        Ok(())
    }

    /// Runs a PipeWire tool against the PipeWire user's daemon.
    async fn tool(&self, program: &str, args: &[&str]) -> anyhow::Result<String> {
        let run = tokio::process::Command::new(program)
            .args(args)
            .envs(crate::player::pipewire_env(&self.hub))
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(TOOL_TIMEOUT, run)
            .await
            .with_context(|| format!("{program} took longer than {TOOL_TIMEOUT:?}"))?
            .with_context(|| format!("cannot run {program}"))?;
        if !out.status.success() {
            bail!(
                "{program} {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Reapplies remembered codecs whenever a Bluetooth output appears (a headset connected).
    pub async fn watch(self: Arc<Self>) {
        let mut events = self.hub.subscribe().events;
        let mut connected: BTreeSet<String> = BTreeSet::new();
        loop {
            match events.recv().await {
                Ok(HubEvent::Audio(state)) => connected = self.on_audio(&state, &connected),
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    }

    /// The Bluetooth outputs now; newly connected ones get their remembered codec.
    fn on_audio(
        self: &Arc<Self>,
        state: &streamdock_proto::streamdock::v1::AudioState,
        before: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        let now: BTreeSet<String> = state
            .outputs
            .iter()
            .filter_map(|o| bluetooth_address(&o.name))
            .collect();
        for address in now.difference(before) {
            let this = Arc::clone(self);
            let address = address.clone();
            tokio::spawn(async move { this.reapply(&address).await });
        }
        now
    }

    /// Brings the link to 48 kHz and applies the remembered choice if the device doesn't run
    /// them (after a connect, or after a node was recreated by a switch, which then finds
    /// nothing to do).
    async fn reapply(&self, address: &str) {
        // Give WirePlumber a moment to finish setting the device up.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _busy = self.busy.lock().await;
        if let Err(e) = self.settle(address).await {
            tracing::warn!(address, error = %format!("{e:#}"), "reapplying the Bluetooth codec failed");
            self.hub.report(
                crate::hub::DAEMON,
                streamdock_proto::streamdock::v1::ActivityLevel::ACTIVITY_LEVEL_ERROR,
                format!("Bluetooth {address}: {e:#}"),
            );
        }
    }

    /// Under the lock, so a manual choice made meanwhile is the one kept.
    async fn settle(&self, address: &str) -> anyhow::Result<()> {
        let Some(mut now) = self.get(address).await? else {
            return Ok(());
        };
        self.after_reconnect(address, &mut now).await?;
        if wrong_rate(&now) && !self.any_rate(address) && self.may_renegotiate(address) {
            now = self
                .renegotiate(address, &now)
                .await
                .context("link rate not renegotiated")?;
        }
        self.keep_choice(address, &now).await.context("codec not applied")
    }
}

impl BtCodecs {
    /// A headset back at volume 0 soon after our switch gets the volume from before it.
    async fn after_reconnect(&self, address: &str, now: &mut DeviceCodecs) -> anyhow::Result<()> {
        let Some(before) = self.volume_before_switch(address) else {
            return Ok(());
        };
        if !restores(now.volume.as_ref(), &before) {
            return Ok(());
        }
        self.keep_volume(now, &before)
            .await
            .context("volume not restored")?;
        self.hub.report(
            crate::hub::DAEMON,
            streamdock_proto::streamdock::v1::ActivityLevel::ACTIVITY_LEVEL_INFO,
            format!("Bluetooth {address}: reconnected at volume 0 after a codec switch, volume restored"),
        );
        Ok(())
    }
}

/// How often LDAC's quality is read back while it settles.
const POLL: Duration = Duration::from_millis(200);

/// A sink node's `Props` as setting LDAC's quality needs them.
trait NodeProps {
    async fn write(&self, props: &str) -> anyhow::Result<()>;
    /// LDAC's quality and the resampler's, as the node reads back.
    async fn read(&self) -> anyhow::Result<(Option<i64>, Option<i64>)>;
}

/// A real node, through pw-cli and pw-dump.
struct Node<'a> {
    codecs: &'a BtCodecs,
    id: i64,
}

impl NodeProps for Node<'_> {
    async fn write(&self, props: &str) -> anyhow::Result<()> {
        let id = self.id.to_string();
        self.codecs
            .tool("pw-cli", &["set-param", &id, "Props", props])
            .await
            .map(drop)
    }

    async fn read(&self) -> anyhow::Result<(Option<i64>, Option<i64>)> {
        let dump = self.codecs.tool("pw-dump", &[]).await?;
        let objects: Vec<Value> = serde_json::from_str(&dump).unwrap_or_default();
        Ok(objects
            .iter()
            .find(|o| int(&o["id"]) == Some(self.id))
            .map(node_qualities)
            .unwrap_or_default())
    }
}

/// Sets LDAC's `quality`, waits until the encoder reads it back, then restores the resampler's
/// quality `resample` with a separate `params` write (3 tries). Whether the resampler is back.
async fn set_quality(
    node: &impl NodeProps,
    quality: i64,
    resample: Option<i64>,
    poll: Duration,
) -> anyhow::Result<bool> {
    node.write(&format!("{{ quality: {quality} }}")).await?;
    let mut read = node.read().await?;
    for _ in 0..10 {
        if read.0 == Some(quality) {
            break;
        }
        tokio::time::sleep(poll).await;
        read = node.read().await?;
    }
    if read.0 != Some(quality) {
        bail!("PipeWire didn't take it");
    }
    let Some(resample) = resample else {
        return Ok(true);
    };
    for _ in 0..3 {
        if read.1 == Some(resample) {
            return Ok(true);
        }
        node.write(&format!("{{ params: [ \"resample.quality\", {resample} ] }}"))
            .await?;
        for _ in 0..5 {
            tokio::time::sleep(poll).await;
            read = node.read().await?;
            if read.1 == Some(resample) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// The `Props` that give a node at `now` the volume `want`; `None` when it has it already.
fn volume_change(now: Option<&Volume>, want: &Volume) -> Option<String> {
    (!now.is_some_and(|v| v.same(want))).then(|| want.props())
}

/// Whether a node at `now` gets `before` back: only a silent node, and only to a non-silent volume.
fn restores(now: Option<&Volume>, before: &Volume) -> bool {
    now.is_some_and(Volume::silent) && !before.silent()
}

/// An A2DP link at another rate than the graph's.
fn wrong_rate(now: &DeviceCodecs) -> bool {
    now.active.is_some() && now.link_rate.is_some_and(|r| r != GRAPH_RATE)
}

/// Whether the device already runs `want` (codec, and LDAC's quality for LDAC).
fn runs(now: &DeviceCodecs, want: &Preference) -> bool {
    now.active.as_deref() == Some(want.codec.as_str())
        && (want.codec != "ldac"
            || now.ldac_quality.as_deref() == Some(want.ldac_quality.as_deref().unwrap_or("auto")))
}

/// `02:00:00:00:00:B4` from `bluez_output.02_00_00_00_00_B4.a2dp-sink`.
fn bluetooth_address(node_name: &str) -> Option<String> {
    let rest = node_name.strip_prefix("bluez_output.")?;
    let addr = rest.split('.').next()?;
    (addr.len() == 17).then(|| addr.replace('_', ":"))
}

/// The index of the automatic `a2dp-sink` profile of `device`.
fn auto_profile_index(dump: &str, device: i64) -> Option<i64> {
    let objects: Vec<Value> = serde_json::from_str(dump).ok()?;
    let d = objects.iter().find(|o| int(&o["id"]) == Some(device))?;
    d["info"]["params"]["EnumProfile"]
        .as_array()?
        .iter()
        .find(|p| p["name"] == "a2dp-sink")
        .and_then(|p| int(&p["index"]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The WH-1000XM4 on the Luckfox (PipeWire 0.3.48), trimmed from `pw-dump`.
    const BOARD: &str = include_str!("bt_codec_fixture.json");

    #[test]
    fn reads_the_offered_codecs_and_the_active_one() {
        let d = parse(BOARD, "02:00:00:00:00:b4").unwrap();
        let ids: Vec<&str> = d.codecs.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["sbc", "sbc_xq", "ldac"]);
        let labels: Vec<&str> = d.codecs.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["SBC", "SBC-XQ", "LDAC"]);
        assert_eq!(d.active.as_deref(), Some("ldac"));
        assert_eq!(d.ldac_quality.as_deref(), Some("auto"));
        assert_eq!(
            (d.device_id, d.node_id, d.resample_quality),
            (115, Some(106), Some(4))
        );
        assert_eq!(d.codecs[2].profile_index, 10);
        assert_eq!(auto_profile_index(BOARD, 115), Some(2));
        assert!(parse(BOARD, "11:22:33:44:55:66").is_none());
        assert!(parse("garbage", "02:00:00:00:00:B4").is_none());
    }

    #[test]
    fn addresses_from_node_names() {
        assert_eq!(
            bluetooth_address("bluez_output.02_00_00_00_00_B4.a2dp-sink").as_deref(),
            Some("02:00:00:00:00:B4")
        );
        assert_eq!(bluetooth_address("alsa_output.platform-acodec"), None);
        assert!(same_address("02_00_00_00_00_B4", "02:00:00:00:00:b4"));
    }

    #[test]
    fn a_device_already_running_the_choice_needs_nothing() {
        let d = parse(BOARD, "02:00:00:00:00:B4").unwrap();
        let want = |codec: &str, q: Option<&str>| Preference {
            codec: codec.into(),
            ldac_quality: q.map(str::to_owned),
        };
        assert!(runs(&d, &want("ldac", None)), "LDAC auto runs");
        assert!(runs(&d, &want("ldac", Some("auto"))));
        assert!(!runs(&d, &want("ldac", Some("hq"))));
        assert!(!runs(&d, &want("sbc_xq", None)));
    }

    #[test]
    fn a_link_the_headset_set_to_96_khz_is_renegotiated() {
        // After the board's reboot the XM4 connected by itself at 96 kHz (H15-78).
        let mut d = parse(BOARD, "02:00:00:00:00:B4").unwrap();
        assert_eq!((d.link_rate, d.profile_index), (Some(96_000), Some(10)));
        assert!(wrong_rate(&d));
        d.link_rate = Some(GRAPH_RATE);
        assert!(!wrong_rate(&d));
        d.link_rate = Some(44_100);
        d.active = None;
        assert!(!wrong_rate(&d), "only A2DP links (a headset profile runs 16 kHz)");
    }

    #[test]
    fn rates_as_plain_numbers_or_choices() {
        let node =
            |rate: Value| serde_json::json!({ "info": { "params": { "EnumFormat": [{ "rate": rate }] } } });
        assert_eq!(node_rate(&node(serde_json::json!(48000))), Some(48_000));
        let choice = serde_json::json!({ "default": 44100, "min": 1, "max": 96000 });
        assert_eq!(node_rate(&node(choice)), Some(44_100));
        assert_eq!(node_rate(&serde_json::json!({})), None);
    }

    /// The board's dump after a switch: a new node (other serial) at `channels`, unmuted.
    fn switched(channels: f64) -> String {
        let mut objects: Vec<Value> = serde_json::from_str(BOARD).unwrap();
        for o in &mut objects {
            if o["id"] == 106 {
                o["info"]["props"]["object.serial"] = 200.into();
                o["info"]["params"]["Props"][0]["channelVolumes"] = serde_json::json!([channels, channels]);
                o["info"]["params"]["Props"][0]["mute"] = false.into();
            }
        }
        serde_json::to_string(&objects).unwrap()
    }

    #[test]
    fn reads_the_volume_and_the_node_serial() {
        let d = parse(BOARD, "02:00:00:00:00:B4").unwrap();
        let v = d.volume.unwrap();
        assert_eq!(
            (v.channels.as_slice(), v.mute),
            ([0.287_496, 0.287_496].as_slice(), true)
        );
        assert_eq!(d.node_serial, Some(164));
        assert_eq!(
            v.props(),
            "{ mute: true, channelVolumes: [ 0.287496, 0.287496 ] }"
        );
    }

    #[test]
    fn a_switch_carries_the_volume_to_the_new_node() {
        let mut before = parse(BOARD, "02:00:00:00:00:B4").unwrap();
        before.volume.as_mut().unwrap().mute = false;
        let old = before.volume.clone().unwrap();
        // H15-78: the new node came up at 0 %.
        let after = parse(&switched(0.0), "02:00:00:00:00:B4").unwrap();
        assert_ne!(after.node_serial, before.node_serial, "a new node");
        assert_eq!(
            volume_change(after.volume.as_ref(), &old).as_deref(),
            Some("{ mute: false, channelVolumes: [ 0.287496, 0.287496 ] }")
        );
        // WirePlumber restored it already: nothing to write.
        let restored = parse(&switched(0.287_496), "02:00:00:00:00:B4").unwrap();
        assert_eq!(volume_change(restored.volume.as_ref(), &old), None);
    }

    #[test]
    fn a_reconnect_at_zero_after_a_switch_gets_the_volume_back() {
        let at = |c: f64| Volume {
            channels: vec![c, c],
            mute: false,
        };
        assert!(restores(Some(&at(0.0)), &at(0.06)));
        assert!(
            !restores(Some(&at(0.02)), &at(0.06)),
            "a volume the user set stays"
        );
        assert!(!restores(Some(&at(0.0)), &at(0.0)));
        assert!(!restores(None, &at(0.06)));
    }

    /// A node that applies writes only after `delay` reads, like PipeWire's asynchronous
    /// delivery; `quality` also sets the resampler's (the converter shares the key), `params`
    /// only the resampler's. `lose` drops that many `params` writes.
    struct Delayed {
        delay: usize,
        lose: std::cell::Cell<usize>,
        state: std::cell::Cell<(Option<i64>, Option<i64>)>,
        pending: std::cell::RefCell<Vec<(usize, String)>>,
        reads: std::cell::Cell<usize>,
        writes: std::cell::RefCell<Vec<String>>,
    }

    impl Delayed {
        fn new(delay: usize, lose: usize) -> Self {
            Self {
                delay,
                lose: lose.into(),
                state: (Some(-1), Some(4)).into(),
                pending: Vec::new().into(),
                reads: 0.into(),
                writes: Vec::new().into(),
            }
        }

        fn apply(&self, props: &str) {
            let (codec, resample) = self.state.get();
            let number = |p: &str| {
                p.trim_matches(|c: char| !c.is_ascii_digit() && c != '-')
                    .parse()
                    .ok()
            };
            if let Some(rest) = props.strip_prefix("{ quality: ") {
                let q = number(rest);
                self.state.set((q, q));
            } else if let Some(rest) = props.split(',').nth(1) {
                self.state.set((codec, number(rest).or(resample)));
            }
        }
    }

    impl NodeProps for Delayed {
        async fn write(&self, props: &str) -> anyhow::Result<()> {
            tokio::task::yield_now().await;
            self.writes.borrow_mut().push(props.to_owned());
            if props.contains("params") && self.lose.get() > 0 {
                self.lose.set(self.lose.get() - 1);
                return Ok(());
            }
            let at = self.reads.get() + self.delay;
            self.pending.borrow_mut().push((at, props.to_owned()));
            Ok(())
        }

        async fn read(&self) -> anyhow::Result<(Option<i64>, Option<i64>)> {
            tokio::task::yield_now().await;
            self.reads.set(self.reads.get() + 1);
            let due: Vec<String> = {
                let mut pending = self.pending.borrow_mut();
                let (due, later) = pending.drain(..).partition(|(at, _)| *at <= self.reads.get());
                *pending = later;
                due.into_iter().map(|(_, p)| p).collect()
            };
            for p in due {
                self.apply(&p);
            }
            Ok(self.state.get())
        }
    }

    #[tokio::test]
    async fn the_resampler_is_restored_only_after_the_encoder_took_the_quality() {
        // H15-78: the quality arrives late; a restore sent before it would be overwritten.
        let node = Delayed::new(3, 0);
        assert!(set_quality(&node, 1, Some(4), Duration::ZERO).await.unwrap());
        assert_eq!(node.state.get(), (Some(1), Some(4)));
        assert_eq!(
            *node.writes.borrow(),
            ["{ quality: 1 }", "{ params: [ \"resample.quality\", 4 ] }"]
        );
    }

    #[tokio::test]
    async fn a_lost_restore_is_retried_and_a_lasting_one_reported() {
        let node = Delayed::new(1, 2);
        assert!(set_quality(&node, 2, Some(4), Duration::ZERO).await.unwrap());
        assert_eq!(node.writes.borrow().len(), 4, "the quality and three restores");
        let node = Delayed::new(1, 3);
        assert!(!set_quality(&node, 2, Some(4), Duration::ZERO).await.unwrap());
        assert_eq!(node.state.get(), (Some(2), Some(2)));
    }

    #[tokio::test]
    async fn a_quality_the_encoder_never_takes_is_an_error() {
        let node = Delayed::new(100, 0);
        assert!(set_quality(&node, 0, Some(4), Duration::ZERO).await.is_err());
        let node = Delayed::new(0, 0);
        assert!(set_quality(&node, 0, None, Duration::ZERO).await.unwrap());
    }

    #[test]
    fn ldac_quality_names() {
        assert_eq!(quality_value("hq"), Some(0));
        assert_eq!(quality_name(-1), Some("auto"));
        assert_eq!(quality_value("best"), None);
    }
}

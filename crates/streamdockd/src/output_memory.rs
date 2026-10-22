//! The output priority (H15-79): the outputs the user chose, in order. The highest one that is
//! there plays; a lower one only while the higher ones are absent, and the higher one takes over
//! again as soon as it appears (after an audio restart, a reboot or a reconnect). Choosing an
//! output on the settings page puts it first.
//!
//! While the first output is a Bluetooth headset that is absent, the daemon connects it: soon
//! after it went away (an audio restart drops every Bluetooth link, and headsets rarely reconnect
//! by themselves), then rarely and never while another Bluetooth output plays (paging the radio
//! can make that stream stutter). Optionally (H15-79, off by default), other
//! Bluetooth audio devices are disconnected while a higher one plays.

use std::sync::Arc;
use std::time::{Duration, Instant};

use streamdock_proto::streamdock::v1 as pb;

use crate::hub::{Hub, HubEvent};
use crate::state::StateFile;

/// How often the schedule is checked without audio events.
const TICK: Duration = Duration::from_secs(3);
/// Connection attempts after the first output went absent, then every [`LATER`].
const SOON: [Duration; 4] = [
    Duration::from_secs(3),
    Duration::from_secs(15),
    Duration::from_secs(45),
    Duration::from_secs(120),
];
const LATER: Duration = Duration::from_secs(600);
/// At most this many outputs in the list.
pub const MAX_PRIORITIES: usize = 12;

/// An output in the priority list: PipeWire's node name, the Bluetooth address for a Bluetooth
/// one (any of its profiles counts), and its description to show while it is absent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Chosen {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluetooth: Option<String>,
    #[serde(default)]
    pub label: String,
}

impl Chosen {
    /// Output `name` (its address taken from a `bluez_output.<addr>.<profile>` name).
    #[must_use]
    pub fn of(name: &str, label: &str) -> Self {
        Self {
            name: name.to_owned(),
            bluetooth: bluetooth_address(name),
            label: label.to_owned(),
        }
    }

    /// Whether `output` is this one: the same Bluetooth device (any profile), or the same name.
    #[must_use]
    pub fn is(&self, output: &pb::AudioOutput) -> bool {
        match &self.bluetooth {
            Some(address) => bluetooth_address(&output.name).as_deref() == Some(address.as_str()),
            None => output.name == self.name,
        }
    }

    /// The same output as `other` (same device or same name).
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        match (&self.bluetooth, &other.bluetooth) {
            (Some(a), Some(b)) => a == b,
            _ => self.name == other.name,
        }
    }
}

/// The list and the behaviour, as kept in `state.rs`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Priority {
    #[serde(default)]
    pub outputs: Vec<Chosen>,
    /// Disconnect other Bluetooth audio devices while a higher one plays.
    #[serde(default)]
    pub disconnect_others: bool,
}

impl Priority {
    /// Puts `chosen` first (the user just chose it).
    pub fn prefer(&mut self, chosen: Chosen) {
        self.outputs.retain(|o| !o.same(&chosen));
        self.outputs.insert(0, chosen);
        self.outputs.truncate(MAX_PRIORITIES);
    }
}

/// `02:00:00:00:00:B4` from `bluez_output.02_00_00_00_00_B4.a2dp-sink`.
#[must_use]
pub fn bluetooth_address(node_name: &str) -> Option<String> {
    let addr = node_name.strip_prefix("bluez_output.")?.split('.').next()?;
    (addr.len() == 17).then(|| addr.replace('_', ":").to_ascii_uppercase())
}

/// What to do now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    /// Make this output the default.
    Select(String),
    /// Try to connect this Bluetooth device.
    Connect(String),
    /// Disconnect this Bluetooth device (it isn't the one playing; `disconnect_others`).
    Disconnect(String),
}

/// Where the reconnect schedule stands while the first output is absent.
#[derive(Debug, Clone, Copy, Default)]
struct Absence {
    since: Option<Instant>,
    attempts: usize,
    last: Option<Instant>,
}

/// What to do for `outputs` now; updates `absence`.
fn decide(
    priority: &Priority,
    outputs: &[pb::AudioOutput],
    absence: &mut Absence,
    now: Instant,
) -> Vec<Action> {
    let mut actions = Vec::new();
    let best = priority
        .outputs
        .iter()
        .enumerate()
        .find_map(|(rank, chosen)| outputs.iter().find(|o| chosen.is(o)).map(|o| (rank, o)));
    if let Some((_, output)) = best
        && !output.selected
    {
        actions.push(Action::Select(output.name.clone()));
    }
    if let Some(action) = reconnect(priority, outputs, best.map(|(rank, _)| rank), absence, now) {
        actions.push(action);
    }
    if priority.disconnect_others
        && let Some((_, playing)) = best
    {
        let playing = bluetooth_address(&playing.name);
        let others = outputs
            .iter()
            .filter_map(|o| bluetooth_address(&o.name))
            .filter(|a| Some(a) != playing.as_ref());
        for address in others {
            let action = Action::Disconnect(address);
            if !actions.contains(&action) {
                actions.push(action);
            }
        }
    }
    actions
}

/// A connection attempt for the first output, when it is an absent Bluetooth one and one is due.
fn reconnect(
    priority: &Priority,
    outputs: &[pb::AudioOutput],
    best_rank: Option<usize>,
    absence: &mut Absence,
    now: Instant,
) -> Option<Action> {
    let first = priority.outputs.first()?;
    if best_rank == Some(0) {
        *absence = Absence::default();
        return None;
    }
    let address = first.bluetooth.as_ref()?;
    let since = *absence.since.get_or_insert(now);
    let due = if let Some(after) = SOON.get(absence.attempts) {
        now >= since + *after
    } else {
        let other_playing = outputs.iter().any(|o| o.kind == "bluetooth" && o.running);
        !other_playing && absence.last.is_none_or(|last| now >= last + LATER)
    };
    if !due {
        return None;
    }
    absence.attempts += 1;
    absence.last = Some(now);
    Some(Action::Connect(address.clone()))
}

/// The service.
#[derive(Debug)]
pub struct OutputMemory {
    hub: Arc<Hub>,
    state: Arc<StateFile>,
}

impl OutputMemory {
    #[must_use]
    pub fn new(hub: Arc<Hub>, state: Arc<StateFile>) -> Arc<Self> {
        Arc::new(Self { hub, state })
    }

    /// The priority as the settings page shows it: each output marked present or not.
    #[must_use]
    pub fn get(&self) -> pb::OutputPriority {
        let priority = self.state.output_priority();
        let outputs = self.hub.audio_state().map(|a| a.outputs).unwrap_or_default();
        pb::OutputPriority {
            outputs: priority
                .outputs
                .iter()
                .map(|c| pb::PriorityOutput {
                    name: c.name.clone(),
                    bluetooth: c.bluetooth.clone().unwrap_or_default(),
                    label: c.label.clone(),
                    present: outputs.iter().any(|o| c.is(o)),
                    ..Default::default()
                })
                .collect(),
            disconnect_others: priority.disconnect_others,
            ..Default::default()
        }
    }

    /// Replaces the priority (the settings page); applied at the next audio event or tick.
    pub fn set(&self, priority: &pb::OutputPriority) -> anyhow::Result<()> {
        anyhow::ensure!(
            priority.outputs.len() <= MAX_PRIORITIES,
            "at most {MAX_PRIORITIES} outputs in the priority"
        );
        let mut outputs: Vec<Chosen> = Vec::new();
        for o in &priority.outputs {
            anyhow::ensure!(!o.name.trim().is_empty(), "an output without a name");
            let chosen = Chosen::of(o.name.trim(), &o.label);
            if !outputs.iter().any(|c| c.same(&chosen)) {
                outputs.push(chosen);
            }
        }
        self.state.set_output_priority(Priority {
            outputs,
            disconnect_others: priority.disconnect_others,
        });
        Ok(())
    }

    /// Follows the audio state and keeps the highest present output playing.
    pub async fn watch(self: Arc<Self>) {
        let mut events = self.hub.subscribe().events;
        let mut tick = tokio::time::interval(TICK);
        let mut outputs: Vec<pb::AudioOutput> = Vec::new();
        let mut absence = Absence::default();
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Ok(HubEvent::Audio(state)) => outputs = state.outputs,
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                _ = tick.tick() => {}
            }
            // Before the first audio state, or when PipeWire is away, there is nothing to judge.
            if outputs.is_empty() {
                continue;
            }
            let priority = self.state.output_priority();
            for action in decide(&priority, &outputs, &mut absence, Instant::now()) {
                self.act(action).await;
            }
        }
    }

    async fn act(&self, action: Action) {
        match action {
            Action::Select(name) => {
                let Some(audio) = self.hub.audio() else { return };
                match audio.switch_to(&name).await {
                    Ok(()) => self.report(
                        pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
                        format!("output switched to {name} (output priority)"),
                    ),
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "switching to the preferred output failed");
                    }
                }
            }
            Action::Connect(address) => self.connect(&address).await,
            Action::Disconnect(address) => self.disconnect(&address).await,
        }
    }

    /// Connects a paired, disconnected headset (never pairs a new one).
    async fn connect(&self, address: &str) {
        let Some(network) = self.hub.network() else { return };
        let Ok(bluetooth) = network.bluetooth().await else {
            return;
        };
        let Ok(devices) = bluetooth.devices().await else {
            return;
        };
        let Some(device) = devices.iter().find(|d| d.address.eq_ignore_ascii_case(address)) else {
            return;
        };
        if !device.paired || device.connected {
            return;
        }
        tracing::info!(address, "connecting the first output");
        match bluetooth.connect(address).await {
            Ok(()) => self.report(
                pb::ActivityLevel::ACTIVITY_LEVEL_OK,
                format!("reconnected {} (first in the output priority)", device.name),
            ),
            Err(e) => {
                tracing::info!(address, error = %format!("{e:#}"), "the first output isn't reachable");
            }
        }
    }

    async fn disconnect(&self, address: &str) {
        let Some(network) = self.hub.network() else { return };
        let Ok(bluetooth) = network.bluetooth().await else {
            return;
        };
        match bluetooth.disconnect(address).await {
            Ok(()) => self.report(
                pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
                format!("disconnected {address}: a higher output in the priority plays"),
            ),
            Err(e) => {
                tracing::warn!(address, error = %format!("{e:#}"), "disconnecting a lower output failed");
            }
        }
    }

    fn report(&self, level: pb::ActivityLevel, text: String) {
        self.hub.report(crate::hub::DAEMON, level, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XM4: &str = "bluez_output.02_00_00_00_00_B4.a2dp-sink";
    const ESSENCE: &str = "bluez_output.00_11_22_33_44_55.a2dp-sink";
    const ANALOG: &str = "alsa_output.platform-acodec-sound.stereo-fallback";

    fn output(name: &str, selected: bool, running: bool) -> pb::AudioOutput {
        pb::AudioOutput {
            name: name.into(),
            kind: if name.starts_with("bluez") {
                "bluetooth"
            } else {
                "analog"
            }
            .into(),
            selected,
            running,
            ..Default::default()
        }
    }

    fn priority(names: &[&str]) -> Priority {
        Priority {
            outputs: names.iter().map(|n| Chosen::of(n, n)).collect(),
            disconnect_others: false,
        }
    }

    #[test]
    fn the_highest_present_output_plays() {
        let p = priority(&[XM4, ANALOG]);
        let mut a = Absence::default();
        let now = Instant::now();
        // The XM4 is back while the Essence (not in the list) plays: switch.
        let outputs = [
            output(ESSENCE, true, true),
            output(XM4, false, false),
            output(ANALOG, false, false),
        ];
        assert_eq!(decide(&p, &outputs, &mut a, now), [Action::Select(XM4.into())]);
        // The XM4 is absent: the analog output stands in (and the XM4 gets a reconnect schedule).
        let without = [output(ESSENCE, true, true), output(ANALOG, false, false)];
        assert_eq!(decide(&p, &without, &mut a, now), [Action::Select(ANALOG.into())]);
        // Another profile of the same headset counts as it.
        let hfp = [output(
            "bluez_output.02_00_00_00_00_B4.headset-head-unit",
            true,
            false,
        )];
        assert_eq!(decide(&p, &hfp, &mut a, now), []);
    }

    #[test]
    fn an_absent_first_headset_is_reconnected_soon_then_rarely_and_not_over_other_audio() {
        let p = priority(&[XM4]);
        let mut a = Absence::default();
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let essence_playing = [output(ESSENCE, true, true)];
        let connect = [Action::Connect("02:00:00:00:00:B4".into())];
        assert_eq!(
            decide(&p, &essence_playing, &mut a, at(0)),
            [],
            "a moment to reconnect itself"
        );
        // The first attempts come even while another headset plays: that's the restart case.
        assert_eq!(decide(&p, &essence_playing, &mut a, at(3)), connect);
        assert_eq!(decide(&p, &essence_playing, &mut a, at(10)), []);
        assert_eq!(decide(&p, &essence_playing, &mut a, at(15)), connect);
        assert_eq!(decide(&p, &essence_playing, &mut a, at(45)), connect);
        assert_eq!(decide(&p, &essence_playing, &mut a, at(120)), connect);
        // Then only every 10 minutes, and only while no other Bluetooth output plays.
        assert_eq!(decide(&p, &essence_playing, &mut a, at(1000)), []);
        let quiet = [output(ESSENCE, true, false)];
        assert_eq!(decide(&p, &quiet, &mut a, at(1000)), connect);
        assert_eq!(decide(&p, &quiet, &mut a, at(1300)), []);
        assert_eq!(decide(&p, &quiet, &mut a, at(1600)), connect);
        // Back: selected, and the schedule starts over next time.
        let back = [output(ESSENCE, true, false), output(XM4, false, false)];
        assert_eq!(decide(&p, &back, &mut a, at(1700)), [Action::Select(XM4.into())]);
        assert_eq!(a.attempts, 0);
    }

    #[test]
    fn others_are_disconnected_only_when_asked() {
        let mut p = priority(&[XM4, ESSENCE]);
        let mut a = Absence::default();
        let now = Instant::now();
        let both = [output(XM4, true, true), output(ESSENCE, false, false)];
        assert_eq!(decide(&p, &both, &mut a, now), [], "default: keep it connected");
        p.disconnect_others = true;
        assert_eq!(
            decide(&p, &both, &mut a, now),
            [Action::Disconnect("00:11:22:33:44:55".into())]
        );
        // Without the XM4, the Essence is the one playing: nothing to disconnect.
        let alone = [output(ESSENCE, true, true)];
        assert!(
            !decide(&p, &alone, &mut a, now)
                .iter()
                .any(|x| matches!(x, Action::Disconnect(_)))
        );
    }

    #[test]
    fn the_settings_page_reads_and_saves_the_priority() {
        let hub = Hub::new();
        let memory = OutputMemory::new(Arc::clone(&hub), Arc::new(StateFile::in_memory()));
        hub.set_audio(pb::AudioState {
            outputs: vec![output(XM4, true, true)],
            ..Default::default()
        });
        let entry = |name: &str| pb::PriorityOutput {
            name: name.into(),
            label: "x".into(),
            ..Default::default()
        };
        let saved = pb::OutputPriority {
            // The same headset twice (another profile) counts once.
            outputs: vec![
                entry(XM4),
                entry(ANALOG),
                entry("bluez_output.02_00_00_00_00_B4.headset-head-unit"),
            ],
            disconnect_others: true,
            ..Default::default()
        };
        memory.set(&saved).unwrap();
        let read = memory.get();
        let names: Vec<(&str, bool)> = read
            .outputs
            .iter()
            .map(|o| (o.name.as_str(), o.present))
            .collect();
        assert_eq!(names, [(XM4, true), (ANALOG, false)]);
        assert_eq!(read.outputs[0].bluetooth, "02:00:00:00:00:B4");
        assert!(read.disconnect_others);
        let too_many = pb::OutputPriority {
            outputs: (0..=MAX_PRIORITIES).map(|i| entry(&format!("out{i}"))).collect(),
            ..Default::default()
        };
        assert!(memory.set(&too_many).is_err());
        assert!(
            memory
                .set(&pb::OutputPriority {
                    outputs: vec![entry(" ")],
                    ..Default::default()
                })
                .is_err()
        );
    }

    #[test]
    fn choosing_puts_an_output_first_once() {
        let mut p = priority(&[ANALOG, XM4]);
        p.prefer(Chosen::of(
            "bluez_output.02_00_00_00_00_B4.headset-head-unit",
            "WH-1000XM4",
        ));
        assert_eq!(p.outputs.len(), 2);
        assert_eq!(p.outputs[0].bluetooth.as_deref(), Some("02:00:00:00:00:B4"));
        assert_eq!(p.outputs[1].name, ANALOG);
    }
}

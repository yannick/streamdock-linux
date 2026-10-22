//! A simulated dock for UI development and tests, enabled with `fake_device = true` or
//! `STREAMDOCKD_FAKE_DEVICE=1`.
//!
//! It attaches as device `FAKE-0001`. Commands succeed without doing anything (images are only
//! checked for being non-empty). With `fake_input = true` it also presses every key and knob and
//! turns every knob in turn, forever, but **only while a demo mode (or no mode) is active**: input
//! to a real integration would switch real lights or play music (H15-33).

use std::sync::Arc;
use std::time::Duration;

use streamdock_hid::{InputEvent, Key, Knob};
use streamdock_proto::streamdock::v1 as pb;

use crate::hub::{Attached, Command, Hub};

/// Device id of the fake dock.
pub const FAKE_ID: &str = "FAKE-0001";

/// The fake dock as the API reports it.
#[must_use]
pub fn device() -> pb::Device {
    pb::Device {
        id: FAKE_ID.to_owned(),
        model: "StreamDock N3 (simulated)".to_owned(),
        firmware_version: "FAKE".to_owned(),
        vendor_id: u32::from(streamdock_hid::VENDOR_ID),
        product_id: u32::from(streamdock_hid::PRODUCT_ID),
        brightness: u32::from(streamdock_hid::DEFAULT_BRIGHTNESS),
        ..Default::default()
    }
}

/// One round over every control: each key and knob pressed and released, each knob turned two
/// detents clockwise and one counter-clockwise.
#[must_use]
pub fn input_cycle() -> Vec<InputEvent> {
    let mut events = Vec::new();
    for key in Key::all() {
        events.extend([InputEvent::KeyDown(key), InputEvent::KeyUp(key)]);
    }
    for knob in (1..=Knob::COUNT).filter_map(Knob::new) {
        events.extend([
            InputEvent::KnobTurn { knob, delta: 1 },
            InputEvent::KnobTurn { knob, delta: 1 },
            InputEvent::KnobTurn { knob, delta: -1 },
            InputEvent::KnobDown(knob),
            InputEvent::KnobUp(knob),
        ]);
    }
    events
}

/// Whether synthetic input may go to the active mode: only demo modes are harmless.
fn input_is_harmless(hub: &Hub) -> bool {
    let active = hub.screen().active_mode;
    active.is_empty() || active.starts_with("demo-")
}

/// Runs the fake dock until the task is dropped; with `input`, it generates input at that
/// interval (see the module docs).
pub async fn run(hub: Arc<Hub>, input: Option<Duration>) {
    let mut dock = hub.attach(device());
    let mut synthetic = Synthetic::default();
    let mut tick = tokio::time::interval(input.unwrap_or(Duration::from_secs(3600)));
    loop {
        tokio::select! {
            _ = tick.tick(), if input.is_some() => synthetic.tick(&hub, &dock),
            Some(req) = dock.commands.recv() => {
                let outcome = answer(dock.id(), &req.command);
                let _ = req.reply.send(outcome);
            }
        }
    }
}

/// The endless input cycle, paused while a real integration is active.
struct Synthetic {
    inputs: std::iter::Cycle<std::vec::IntoIter<InputEvent>>,
    /// Whether the current pause was logged already (once per pause).
    warned: bool,
}

impl Default for Synthetic {
    fn default() -> Self {
        Self {
            inputs: input_cycle().into_iter().cycle(),
            warned: false,
        }
    }
}

impl Synthetic {
    /// Publishes the next input event, unless the active mode is a real integration.
    fn tick(&mut self, hub: &Hub, dock: &Attached) {
        if input_is_harmless(hub) {
            self.warned = false;
            dock.publish_input(self.inputs.next().expect("cycle is endless"));
        } else if !self.warned {
            self.warned = true;
            tracing::warn!(mode = %hub.screen().active_mode, "fake dock: no synthetic input while a real integration is active");
        }
    }
}

/// The fake dock's reply to a command: success, except for an empty key image.
fn answer(device: &str, command: &Command) -> Result<(), String> {
    match command {
        Command::SetKeyImage { image, .. } if image.is_empty() => Err("empty image".to_owned()),
        Command::SetKeyImage { key, image } => {
            tracing::debug!(device, key, bytes = image.len(), "fake dock: key image");
            Ok(())
        }
        command => {
            tracing::info!(device, ?command, "fake dock: command");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::HubEvent;

    fn screen(active: &str) -> pb::Screen {
        pb::Screen {
            active_mode: active.to_owned(),
            ..Default::default()
        }
    }

    /// Counts the input events the fake dock publishes within `window`.
    async fn inputs_within(hub: &Arc<Hub>, window: Duration) -> usize {
        let mut rx = hub.subscribe().events;
        let mut n = 0;
        let _ = tokio::time::timeout(window, async {
            loop {
                if let Ok(HubEvent::Device(e)) = rx.recv().await
                    && matches!(
                        e.kind,
                        Some(
                            streamdock_proto::streamdock::v1::__buffa::oneof::device_event::Kind::Key(_)
                                | streamdock_proto::streamdock::v1::__buffa::oneof::device_event::Kind::Knob(
                                    _
                                )
                        )
                    )
                {
                    n += 1;
                }
            }
        })
        .await;
        n
    }

    /// H15-33: synthetic presses must never reach a real integration.
    #[tokio::test]
    async fn synthetic_input_only_reaches_demo_modes() {
        let hub = Hub::new();
        hub.set_screen(screen("homeassistant"));
        tokio::spawn(run(Arc::clone(&hub), Some(Duration::from_millis(5))));
        assert_eq!(inputs_within(&hub, Duration::from_millis(100)).await, 0);
        hub.set_screen(screen("demo-1"));
        assert!(inputs_within(&hub, Duration::from_millis(100)).await > 3);
    }

    #[tokio::test]
    async fn no_input_unless_asked() {
        let hub = Hub::new();
        tokio::spawn(run(Arc::clone(&hub), None));
        assert_eq!(inputs_within(&hub, Duration::from_millis(100)).await, 0);
    }
}

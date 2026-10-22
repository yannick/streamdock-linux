//! Headset mode (H15-91, decided on H15-89): Lyrion moves the queue and position from the
//! speaker's player to the built-in player, which plays into the Bluetooth headset, and back.
//!
//! The move itself is `transfer.rs`: through a hidden saved playlist, without syncing the
//! players.

use std::future::Future;
use std::sync::Arc;

use anyhow::Context as _;
use serde_json::{Value, json};

use crate::hub::Hub;
use crate::modules::lyrion::client::Client;

/// What the screen needs for the headset mode.
pub trait Music: Send + Sync + 'static {
    /// Whether the music is on the headset's player now.
    fn on_headset(&self) -> impl Future<Output = anyhow::Result<bool>> + Send;
    /// Moves the music to the headset (`Some(address)`: that headset becomes the output) or back
    /// to the speaker (`None`).
    fn switch(&self, headset: Option<&str>) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// No headset mode (no `speaker_player` configured).
#[derive(Debug, Clone, Copy)]
pub struct Off;

impl Music for Off {
    fn on_headset(&self) -> impl Future<Output = anyhow::Result<bool>> + Send {
        std::future::ready(Err(anyhow::anyhow!("no headset mode configured")))
    }

    fn switch(&self, _headset: Option<&str>) -> impl Future<Output = anyhow::Result<()>> + Send {
        std::future::ready(Err(anyhow::anyhow!("no headset mode configured")))
    }
}

/// The two Lyrion players.
#[derive(Debug, Clone)]
pub struct Lyrion {
    pub(super) client: Client,
    /// The speaker's player id (the SRS-X88's "airplay Bedroom").
    pub(super) speaker: String,
    /// The headset's player id; `None`: the built-in player's.
    pub(super) headset: Option<String>,
    pub(super) hub: Option<Arc<Hub>>,
}

impl Lyrion {
    fn headset_player(&self) -> anyhow::Result<String> {
        if let Some(id) = &self.headset {
            return Ok(id.clone());
        }
        self.hub
            .as_ref()
            .and_then(|h| h.player_state())
            .map(|p| p.player_id)
            .filter(|id| !id.is_empty())
            .context("the built-in player isn't running")
    }

    async fn command(&self, player: &str, command: Value) -> anyhow::Result<Value> {
        self.client
            .request(player, command.clone())
            .await
            .with_context(|| format!("{player}: {command}"))
    }

    /// Makes the connected headset `address` the output, so the built-in player plays into it.
    async fn select_output(&self, address: &str) -> anyhow::Result<()> {
        let Some(hub) = &self.hub else { return Ok(()) };
        let (Some(audio), Some(state)) = (hub.audio(), hub.audio_state()) else {
            return Ok(());
        };
        let needle = address.replace(':', "_").to_ascii_uppercase();
        let output = state
            .outputs
            .iter()
            .find(|o| o.name.to_ascii_uppercase().contains(&needle))
            .context("the headset has no audio output yet")?;
        if !output.selected {
            audio.switch_to(&output.name).await?;
        }
        Ok(())
    }
}

/// Whether a player's `status` says it has the music: on, with a queue.
pub(super) fn has_music(status: &Value) -> bool {
    let number = |key: &str| {
        status[key]
            .as_i64()
            .or_else(|| status[key].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0)
    };
    number("power") == 1 && number("playlist_tracks") > 0
}

impl Music for Lyrion {
    async fn on_headset(&self) -> anyhow::Result<bool> {
        let status = self
            .command(&self.headset_player()?, json!(["status", "-", "1", "tags:"]))
            .await?;
        Ok(has_music(&status))
    }

    async fn switch(&self, headset: Option<&str>) -> anyhow::Result<()> {
        let headset_player = self.headset_player()?;
        if let Some(address) = headset {
            self.select_output(address).await?;
        }
        let (from, to) = match headset {
            Some(_) => (self.speaker.as_str(), headset_player.as_str()),
            None => (headset_player.as_str(), self.speaker.as_str()),
        };
        super::transfer::move_queue(&self.client, from, to).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_player_with_power_and_a_queue_has_the_music() {
        assert!(has_music(
            &json!({ "power": 1, "playlist_tracks": 12, "mode": "pause" })
        ));
        assert!(
            has_music(&json!({ "power": "1", "playlist_tracks": "3" })),
            "LMS sends strings too"
        );
        assert!(
            !has_music(&json!({ "power": 0, "playlist_tracks": 12 })),
            "switched off"
        );
        assert!(
            !has_music(&json!({ "power": 1, "playlist_tracks": 0 })),
            "empty queue"
        );
        assert!(!has_music(&Value::Null));
    }
}

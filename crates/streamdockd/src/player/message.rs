//! `[player]` as the API's `PlayerSettings` and back (H15-52, H15-61).

use streamdock_proto::streamdock::v1 as pb;

use crate::config::{OutputChoice, PlayerSettings, ReplayGain};

#[must_use]
pub fn settings_message(s: &PlayerSettings) -> pb::PlayerSettings {
    pb::PlayerSettings {
        enabled: s.enabled,
        name: s.name.clone(),
        server: s.server.map(|a| a.to_string()).unwrap_or_default(),
        max_sample_rate: s.max_sample_rate,
        formats: s.formats.iter().map(|c| c.name().to_owned()).collect(),
        output_buffer_ms: u32::try_from(s.output_buffer.as_millis()).unwrap_or(u32::MAX),
        replay_gain: s.replay_gain.map(|g| g.name().to_owned()).unwrap_or_default(),
        output: s.output.as_str().to_owned(),
        transition: s.transition.map(|t| t.kind.name().to_owned()).unwrap_or_default(),
        transition_seconds: s
            .transition
            .map_or(PlayerSettings::DEFAULT_TRANSITION_SECONDS, |t| t.seconds),
        resampling: s.resampling_name().to_owned(),
        ..Default::default()
    }
}

/// Checks a `SetPlayer` request: every field as the config file would.
pub fn settings_from_message(
    s: &pb::__buffa::view::PlayerSettingsView<'_>,
) -> anyhow::Result<PlayerSettings> {
    let name = s.name.trim();
    anyhow::ensure!(!name.is_empty(), "the player needs a name");
    anyhow::ensure!(
        PlayerSettings::SAMPLE_RATES.contains(&s.max_sample_rate),
        "unsupported max sample rate {}",
        s.max_sample_rate
    );
    let formats: Vec<String> = s.formats.iter().map(|f| (*f).to_owned()).collect();
    Ok(PlayerSettings {
        enabled: s.enabled,
        name: name.to_owned(),
        server: PlayerSettings::parse_server(s.server)?,
        max_sample_rate: s.max_sample_rate,
        formats: PlayerSettings::parse_formats(&formats)?,
        output_buffer: PlayerSettings::output_buffer(s.output_buffer_ms)?,
        replay_gain: match s.replay_gain.trim() {
            "" => None,
            gain => Some(ReplayGain::parse(gain)?),
        },
        output: OutputChoice::parse(s.output),
        transition: PlayerSettings::parse_transition(s.transition, s.transition_seconds)?,
        resampling: PlayerSettings::parse_resampling(s.resampling)?,
        // Not on the settings page: the writer leaves the configured key alone (H15-91).
        max_volume: PlayerSettings::MAX_VOLUME,
    })
}

//! `[player]`: the built-in Lyrion player (H15-55, settings H15-52 / H15-61).

use std::net::SocketAddrV4;
use std::time::Duration;

use anyhow::Context as _;
use player::output::resample::Quality;
use serde::Deserialize;
use slimproto::Codec;

/// `[player]` as in the file.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct PlayerFile {
    enabled: Option<bool>,
    name: Option<String>,
    /// `ip` or `ip:port`.
    server: Option<String>,
    max_sample_rate: Option<u32>,
    /// `["flac", "mp3", "pcm"]` in preference order; `["pcm"]` = Lyrion transcodes everything.
    formats: Option<Vec<String>>,
    output_buffer_ms: Option<u32>,
    /// "off", "track", "album" or "smart"; unset = Lyrion's own setting.
    replay_gain: Option<String>,
    /// "dsp" (default), "default" (the default output, without the DSP) or an output's name.
    output: Option<String>,
    /// "none", "crossfade", "fade-in", "fade-out" or "fade-in-out"; unset = Lyrion's own setting.
    transition: Option<String>,
    transition_seconds: Option<u32>,
    /// "pipewire" (PipeWire converts), "fast", "balanced" (default) or "high" (the player converts).
    resampling: Option<String>,
    /// The highest Lyrion volume the player plays at, 1-100 (H15-91: a child's headphones).
    max_volume: Option<u32>,
}

/// Lyrion's track transition (`transitionType`, H15-64), played by the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionKind {
    None,
    Crossfade,
    FadeIn,
    FadeOut,
    FadeInOut,
}

impl TransitionKind {
    pub const ALL: [Self; 5] = [
        Self::None,
        Self::Crossfade,
        Self::FadeIn,
        Self::FadeOut,
        Self::FadeInOut,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Crossfade => "crossfade",
            Self::FadeIn => "fade-in",
            Self::FadeOut => "fade-out",
            Self::FadeInOut => "fade-in-out",
        }
    }

    pub fn parse(name: &str) -> anyhow::Result<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name).with_context(|| {
            format!("transition must be none, crossfade, fade-in, fade-out or fade-in-out, got {name:?}")
        })
    }

    /// Lyrion's `transitionType` value.
    #[must_use]
    pub fn lyrion_type(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Crossfade => 1,
            Self::FadeIn => 2,
            Self::FadeOut => 3,
            Self::FadeInOut => 4,
        }
    }
}

/// A transition and its length (`transitionDuration`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub kind: TransitionKind,
    pub seconds: u32,
}

/// Replay gain, Lyrion's player preference `replayGainMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayGain {
    Off,
    Track,
    Album,
    /// Album gain when the playlist plays one album, else track gain.
    Smart,
}

impl ReplayGain {
    pub const ALL: [Self; 4] = [Self::Off, Self::Track, Self::Album, Self::Smart];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Track => "track",
            Self::Album => "album",
            Self::Smart => "smart",
        }
    }

    pub fn parse(name: &str) -> anyhow::Result<Self> {
        Self::ALL
            .into_iter()
            .find(|g| g.name() == name)
            .with_context(|| format!("replay gain must be off, track, album or smart, got {name:?}"))
    }

    /// Lyrion's `replayGainMode` value.
    #[must_use]
    pub fn lyrion_mode(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Track => 1,
            Self::Album => 2,
            Self::Smart => 3,
        }
    }
}

/// Where the player plays.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OutputChoice {
    /// Through the native DSP into the selected output, so sound settings apply (default).
    #[default]
    Dsp,
    /// The default output, without the DSP.
    Default,
    /// This output (a PipeWire node name), without the DSP.
    Named(String),
}

impl OutputChoice {
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text.trim() {
            "" | "dsp" => Self::Dsp,
            "default" => Self::Default,
            name => Self::Named(name.to_owned()),
        }
    }

    /// As in the config.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Dsp => "dsp",
            Self::Default => "default",
            Self::Named(name) => name,
        }
    }
}

/// The built-in Lyrion player's settings (`[player]`), also while it is off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerSettings {
    pub enabled: bool,
    /// The name the player registers with; a rename in Lyrion overrides it (kept in the state file).
    pub name: String,
    /// The server, or `None` for discovery.
    pub server: Option<SocketAddrV4>,
    /// Highest sample rate offered to Lyrion; it resamples above.
    pub max_sample_rate: u32,
    /// The formats offered to Lyrion, in preference order (never empty).
    pub formats: Vec<Codec>,
    /// Decoded audio buffered before the output.
    pub output_buffer: Duration,
    /// `None`: leave Lyrion's setting alone.
    pub replay_gain: Option<ReplayGain>,
    pub output: OutputChoice,
    /// `None`: leave Lyrion's setting alone.
    pub transition: Option<Transition>,
    /// The player converts to the output's rate at this quality (H15-80); `None`: PipeWire does.
    pub resampling: Option<Quality>,
    /// Lyrion volumes above this play at this (H15-91), 1-100; 100 = no limit.
    pub max_volume: u32,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            name: Self::DEFAULT_NAME.to_owned(),
            server: None,
            max_sample_rate: Self::DEFAULT_MAX_SAMPLE_RATE,
            formats: Codec::ALL.to_vec(),
            output_buffer: Self::DEFAULT_OUTPUT_BUFFER,
            replay_gain: None,
            output: OutputChoice::Dsp,
            transition: None,
            resampling: Some(Quality::Balanced),
            max_volume: Self::MAX_VOLUME,
        }
    }
}

impl PlayerSettings {
    /// The default name in Lyrion.
    pub const DEFAULT_NAME: &str = "StreamDock";
    pub const DEFAULT_MAX_SAMPLE_RATE: u32 = 96_000;
    /// The sample rates `max_sample_rate` may take.
    pub const SAMPLE_RATES: [u32; 4] = [44_100, 48_000, 96_000, 192_000];
    pub const DEFAULT_OUTPUT_BUFFER: Duration = Duration::from_secs(4);
    /// The range of `output_buffer_ms`.
    pub const OUTPUT_BUFFER_MS: std::ops::RangeInclusive<u32> = 1_000..=20_000;

    /// `max_volume` without a limit.
    pub const MAX_VOLUME: u32 = 100;

    pub const DEFAULT_TRANSITION_SECONDS: u32 = 5;
    /// The range of `transition_seconds`.
    pub const TRANSITION_SECONDS: std::ops::RangeInclusive<u32> = 1..=10;

    /// The output buffer the player runs with: the configured one, raised to the crossfade's
    /// length plus a second (the crossfade mixes into what is still buffered, H15-64).
    #[must_use]
    pub fn effective_output_buffer(&self) -> Duration {
        match self.transition {
            Some(Transition {
                kind: TransitionKind::Crossfade,
                seconds,
            }) => self
                .output_buffer
                .max(Duration::from_secs(u64::from(seconds) + 1)),
            _ => self.output_buffer,
        }
    }

    /// A transition from its name and length; `""` = `None` (Lyrion's own setting).
    pub fn parse_transition(kind: &str, seconds: u32) -> anyhow::Result<Option<Transition>> {
        if kind.trim().is_empty() {
            return Ok(None);
        }
        anyhow::ensure!(
            Self::TRANSITION_SECONDS.contains(&seconds),
            "the transition must be {} to {} s, got {seconds}",
            Self::TRANSITION_SECONDS.start(),
            Self::TRANSITION_SECONDS.end()
        );
        Ok(Some(Transition {
            kind: TransitionKind::parse(kind.trim())?,
            seconds,
        }))
    }

    /// `ip` or `ip:port` (port 3483 by default); empty = `None` (discovery).
    pub fn parse_server(text: &str) -> anyhow::Result<Option<SocketAddrV4>> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        if let Ok(addr) = text.parse::<SocketAddrV4>() {
            return Ok(Some(addr));
        }
        let ip = text
            .parse()
            .with_context(|| format!("the Lyrion server must be an IPv4 address, got {text:?}"))?;
        Ok(Some(SocketAddrV4::new(ip, slimproto::messages::PORT)))
    }

    /// The resampling choices, as in the config: PipeWire's, then the player's qualities.
    pub const RESAMPLING: [&str; 4] = ["pipewire", "fast", "balanced", "high"];

    /// `"pipewire"` = `None`, else the player's quality; empty = the default (balanced).
    pub fn parse_resampling(text: &str) -> anyhow::Result<Option<Quality>> {
        match text.trim() {
            "" => Ok(Some(Quality::Balanced)),
            "pipewire" => Ok(None),
            other => Quality::parse(other)
                .map(Some)
                .with_context(|| format!("resampling must be one of {:?}, got {other:?}", Self::RESAMPLING)),
        }
    }

    #[must_use]
    pub fn resampling_name(&self) -> &'static str {
        self.resampling.map_or("pipewire", Quality::as_str)
    }

    /// Format names in preference order; each once, at least one.
    pub fn parse_formats(names: &[String]) -> anyhow::Result<Vec<Codec>> {
        let mut formats = Vec::new();
        for name in names {
            let codec = Codec::parse(name.trim())
                .with_context(|| format!("formats: {name:?} is not flac, mp3 or pcm"))?;
            anyhow::ensure!(!formats.contains(&codec), "formats: {name:?} is listed twice");
            formats.push(codec);
        }
        anyhow::ensure!(!formats.is_empty(), "formats: at least one format");
        Ok(formats)
    }

    /// Checks an output buffer length in milliseconds.
    pub fn output_buffer(ms: u32) -> anyhow::Result<Duration> {
        anyhow::ensure!(
            Self::OUTPUT_BUFFER_MS.contains(&ms),
            "the output buffer must be {} to {} ms, got {ms}",
            Self::OUTPUT_BUFFER_MS.start(),
            Self::OUTPUT_BUFFER_MS.end()
        );
        Ok(Duration::from_millis(u64::from(ms)))
    }

    pub(super) fn from_file(file: Option<PlayerFile>) -> anyhow::Result<Self> {
        let file = file.unwrap_or_default();
        let max_sample_rate = file.max_sample_rate.unwrap_or(Self::DEFAULT_MAX_SAMPLE_RATE);
        anyhow::ensure!(
            Self::SAMPLE_RATES.contains(&max_sample_rate),
            "[player] max_sample_rate must be one of {:?}, got {max_sample_rate}",
            Self::SAMPLE_RATES
        );
        let name = file
            .name
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| Self::DEFAULT_NAME.to_owned());
        Ok(Self {
            enabled: file.enabled == Some(true),
            name,
            server: Self::parse_server(file.server.as_deref().unwrap_or_default())
                .context("[player] server")?,
            max_sample_rate,
            formats: match &file.formats {
                Some(names) => Self::parse_formats(names).context("[player]")?,
                None => Codec::ALL.to_vec(),
            },
            output_buffer: match file.output_buffer_ms {
                Some(ms) => Self::output_buffer(ms).context("[player] output_buffer_ms")?,
                None => Self::DEFAULT_OUTPUT_BUFFER,
            },
            replay_gain: file
                .replay_gain
                .as_deref()
                .map(ReplayGain::parse)
                .transpose()
                .context("[player]")?,
            output: file
                .output
                .as_deref()
                .map(OutputChoice::parse)
                .unwrap_or_default(),
            transition: Self::parse_transition(
                file.transition.as_deref().unwrap_or_default(),
                file.transition_seconds
                    .unwrap_or(Self::DEFAULT_TRANSITION_SECONDS),
            )
            .context("[player]")?,
            resampling: Self::parse_resampling(file.resampling.as_deref().unwrap_or_default())
                .context("[player]")?,
            max_volume: match file.max_volume {
                Some(v @ 1..=Self::MAX_VOLUME) => v,
                Some(v) => anyhow::bail!("[player] max_volume must be 1 to 100, got {v}"),
                None => Self::MAX_VOLUME,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    #[test]
    fn resampling() {
        let parse = |t: &str| Settings::parse(t).map(|s| s.player.resampling);
        assert_eq!(
            parse("").unwrap(),
            Some(Quality::Balanced),
            "the player converts by default"
        );
        assert_eq!(parse("[player]\nresampling = \"pipewire\"\n").unwrap(), None);
        assert_eq!(
            parse("[player]\nresampling = \"fast\"\n").unwrap(),
            Some(Quality::Fast)
        );
        assert_eq!(
            parse("[player]\nresampling = \"high\"\n").unwrap(),
            Some(Quality::High)
        );
        assert!(parse("[player]\nresampling = \"best\"\n").is_err());
        for name in PlayerSettings::RESAMPLING {
            let s = PlayerSettings {
                resampling: PlayerSettings::parse_resampling(name).unwrap(),
                ..PlayerSettings::default()
            };
            assert_eq!(s.resampling_name(), name);
        }
    }

    #[test]
    fn max_volume() {
        let parse = |t: &str| Settings::parse(t).map(|s| s.player.max_volume);
        assert_eq!(parse("").unwrap(), 100, "no limit by default");
        assert_eq!(parse("[player]\nmax_volume = 60\n").unwrap(), 60);
        assert!(parse("[player]\nmax_volume = 0\n").is_err());
        assert!(parse("[player]\nmax_volume = 101\n").is_err());
    }

    #[test]
    fn player_is_opt_in() {
        let off = Settings::parse("").unwrap().player;
        assert!(!off.enabled);
        assert_eq!(off, PlayerSettings::default());
        let named = Settings::parse("[player]\nname = \"X\"\n").unwrap().player;
        assert!(!named.enabled, "not enabled, but kept for the settings screen");
        assert_eq!(named.name, "X");
        let player = Settings::parse("[player]\nenabled = true\n").unwrap().player;
        assert!(player.enabled);
        assert_eq!(player.name, "StreamDock");
        assert_eq!(player.server, None, "discovery");
        assert_eq!(player.max_sample_rate, 96_000);
        let text = "[player]\nenabled = true\nname = \" Kitchen \"\nserver = \"192.168.1.13\"\nmax_sample_rate = 48000\n";
        let player = Settings::parse(text).unwrap().player;
        assert_eq!(player.name, "Kitchen");
        assert_eq!(player.server, Some("192.168.1.13:3483".parse().unwrap()));
        assert_eq!(player.max_sample_rate, 48_000);
        let player = Settings::parse("[player]\nserver = \"10.0.0.1:3484\"\n")
            .unwrap()
            .player;
        assert_eq!(player.server, Some("10.0.0.1:3484".parse().unwrap()));
        assert!(Settings::parse("[player]\nserver = \"lyrion.local\"\n").is_err());
        assert!(Settings::parse("[player]\nmax_sample_rate = 22050\n").is_err());
    }

    #[test]
    fn audio_settings() {
        let player = Settings::parse(
            "[player]\nformats = [\"pcm\"]\noutput_buffer_ms = 8000\nreplay_gain = \"album\"\noutput = \"bluez_output.x\"\n",
        )
        .unwrap()
        .player;
        assert_eq!(player.formats, vec![Codec::Pcm], "PCM only");
        assert_eq!(player.output_buffer, Duration::from_secs(8));
        assert_eq!(player.replay_gain, Some(ReplayGain::Album));
        assert_eq!(player.output, OutputChoice::Named("bluez_output.x".into()));
        let defaults = PlayerSettings::default();
        assert_eq!(defaults.formats, Codec::ALL.to_vec());
        assert_eq!((defaults.replay_gain, defaults.output), (None, OutputChoice::Dsp));
        for bad in [
            "formats = []",
            "formats = [\"ogg\"]",
            "formats = [\"mp3\", \"mp3\"]",
            "output_buffer_ms = 100",
            "replay_gain = \"loud\"",
        ] {
            assert!(Settings::parse(&format!("[player]\n{bad}\n")).is_err(), "{bad}");
        }
        assert_eq!(OutputChoice::parse("default"), OutputChoice::Default);
        assert_eq!(defaults.transition, None);
        assert!(Settings::parse("[player]\ntransition = \"swirl\"\n").is_err());
        assert!(Settings::parse("[player]\ntransition = \"crossfade\"\ntransition_seconds = 30\n").is_err());
    }

    #[test]
    fn a_crossfade_raises_the_output_buffer() {
        let player = Settings::parse("[player]\ntransition = \"crossfade\"\ntransition_seconds = 8\n")
            .unwrap()
            .player;
        assert_eq!(
            player.transition,
            Some(Transition {
                kind: TransitionKind::Crossfade,
                seconds: 8
            })
        );
        assert_eq!(player.output_buffer, Duration::from_secs(4), "as configured");
        assert_eq!(
            player.effective_output_buffer(),
            Duration::from_secs(9),
            "crossfade + 1 s"
        );
        let fade = Settings::parse("[player]\ntransition = \"fade-in\"\ntransition_seconds = 8\n")
            .unwrap()
            .player;
        assert_eq!(
            fade.effective_output_buffer(),
            Duration::from_secs(4),
            "fades need no buffer"
        );
        assert_eq!(OutputChoice::parse(""), OutputChoice::Dsp);
    }
}

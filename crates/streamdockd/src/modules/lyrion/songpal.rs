//! A Sony speaker's own tone control over its Songpal API (H15-90): knob 2 on a music tab turns one
//! band of the speaker's custom equaliser (the bass, `100HzBandLevel`, −10…+10). Home Assistant's
//! Songpal integration can only pick presets (`soundField`), not the bands, so the dock talks to the
//! speaker itself. The bands apply only with the CUSTOM preset, which the first change selects.
//!
//! ```toml
//! [[modules.lyrion.tab]]
//! tone = { songpal = "http://192.168.1.195:54480/sony", band = "100HzBandLevel" }
//! ```

use std::time::Duration;

use anyhow::{Context as _, bail};
use serde_json::{Value, json};

/// A band level's range on the SRS-X88 (from `getCustomEqualizerSettings`).
pub const MIN: i32 = -10;
pub const MAX: i32 = 10;
const TIMEOUT: Duration = Duration::from_secs(4);

/// `tone = { … }` of a music tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The speaker's Songpal endpoint, e.g. `http://192.168.1.195:54480/sony`.
    pub endpoint: String,
    /// The custom equaliser band knob 2 turns.
    pub band: String,
}

impl Config {
    pub fn from_value(value: Option<&toml::Value>) -> anyhow::Result<Option<Self>> {
        let Some(value) = value else { return Ok(None) };
        let table = value
            .as_table()
            .context("tone must be a table, e.g. { songpal = \"http://…:54480/sony\" }")?;
        for key in table.keys() {
            if !["songpal", "band"].contains(&key.as_str()) {
                bail!("tone: unknown setting {key:?} (songpal, band)");
            }
        }
        let endpoint = table
            .get("songpal")
            .and_then(toml::Value::as_str)
            .context("tone.songpal is required: the speaker's Songpal endpoint, e.g. \"http://192.168.1.195:54480/sony\"")?
            .trim_end_matches('/')
            .to_owned();
        let band = table
            .get("band")
            .map(|b| b.as_str().context("tone.band must be a string"))
            .transpose()?
            .unwrap_or("100HzBandLevel")
            .to_owned();
        Ok(Some(Self { endpoint, band }))
    }
}

#[derive(Debug, Clone)]
pub struct Songpal {
    http: reqwest::Client,
    config: Config,
}

impl Songpal {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder().timeout(TIMEOUT).build()?,
            config,
        })
    }

    async fn call(&self, service: &str, method: &str, version: &str, params: Value) -> anyhow::Result<Value> {
        let body = json!({ "method": method, "id": 1, "params": [params], "version": version });
        let reply: Value = self
            .http
            .post(format!("{}/{service}", self.config.endpoint))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if let Some(error) = reply.get("error") {
            bail!("Songpal {method}: error {error}");
        }
        Ok(reply["result"][0].clone())
    }

    /// The band's current level.
    pub async fn level(&self) -> anyhow::Result<i32> {
        let bands = self
            .call(
                "audio",
                "getCustomEqualizerSettings",
                "1.0",
                json!({ "target": "" }),
            )
            .await?;
        band_level(&bands, &self.config.band)
            .with_context(|| format!("the speaker has no equaliser band {:?}", self.config.band))
    }

    /// Sets the band to `level` (clamped), selecting the CUSTOM preset first if needed.
    pub async fn set_level(&self, level: i32) -> anyhow::Result<i32> {
        let level = level.clamp(MIN, MAX);
        let sound = self
            .call(
                "audio",
                "getSoundSettings",
                "1.1",
                json!({ "target": "soundField" }),
            )
            .await?;
        if sound_field(&sound).as_deref() != Some("custom") {
            self.call(
                "audio",
                "setSoundSettings",
                "1.1",
                json!({ "settings": [{ "target": "soundField", "value": "custom" }] }),
            )
            .await?;
        }
        self.call(
            "audio",
            "setCustomEqualizerSettings",
            "1.0",
            json!({ "settings": [{ "target": self.config.band, "value": level.to_string() }] }),
        )
        .await?;
        Ok(level)
    }
}

/// `currentValue` of `band` in a `getCustomEqualizerSettings` result.
fn band_level(bands: &Value, band: &str) -> Option<i32> {
    bands.as_array()?.iter().find(|b| b["target"] == band)?["currentValue"]
        .as_str()?
        .parse()
        .ok()
}

/// The current `soundField` in a `getSoundSettings` result.
fn sound_field(settings: &Value) -> Option<String> {
    settings
        .as_array()?
        .iter()
        .find(|s| s["target"] == "soundField")?["currentValue"]
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_band_and_preset_from_the_srs_x88() {
        // As the SRS-X88 answered on 2026-10-19 (H15-90), shortened.
        let bands = json!([
            {"candidate": [{"max": 10, "min": -10, "step": 1}], "currentValue": "0", "target": "100HzBandLevel"},
            {"candidate": [{"max": 10, "min": -10, "step": 1}], "currentValue": "-3", "target": "330HzBandLevel"}
        ]);
        assert_eq!(band_level(&bands, "100HzBandLevel"), Some(0));
        assert_eq!(band_level(&bands, "330HzBandLevel"), Some(-3));
        assert_eq!(band_level(&bands, "10000HzBandLevel"), None);
        let sound = json!([{"currentValue": "hiphop", "target": "soundField"}, {"currentValue": "off", "target": "clearAudio"}]);
        assert_eq!(sound_field(&sound).as_deref(), Some("hiphop"));
    }

    #[test]
    fn config() {
        let v: toml::Value = toml::from_str("t = { songpal = \"http://10.0.0.5:54480/sony/\" }").unwrap();
        let c = Config::from_value(v.get("t")).unwrap().unwrap();
        assert_eq!(c.endpoint, "http://10.0.0.5:54480/sony");
        assert_eq!(c.band, "100HzBandLevel");
        assert_eq!(Config::from_value(None).unwrap(), None);
        let bad: toml::Value = toml::from_str("t = { band = \"x\" }").unwrap();
        assert!(Config::from_value(bad.get("t")).is_err(), "songpal is required");
    }
}

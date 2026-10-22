//! The sound settings of the Luckfox (H15-54) and how they become control values of the
//! native player DSP. Pure: no PipeWire here.
//!
//! B&O-style controls are built from standard parts: the sound character (a Beosonic-like pad,
//! warm↔bright and relaxed↔energetic) and loudness (bass and some treble that grow as the volume
//! goes down) move the bass/mid/treble shelves together with the user's own tone; a 10-band
//! correction EQ (`AutoEq` headphone targets or custom) follows; then crossfeed (bs2b) and a
//! lookahead limiter whose input gain is the preamp.

use serde::{Deserialize, Serialize};

/// Tone gains are limited to ± this many dB.
pub const MAX_TONE_DB: f64 = 12.0;
/// What the shelves get at most after character and loudness are folded in.
const MAX_SHELF_DB: f64 = 18.0;
/// Bands of the correction EQ: a low shelf, eight peaks, a high shelf (fixed by the graph).
pub const BANDS: usize = 10;

/// Every sound setting, as stored in the state file. Missing fields (older files) are defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dsp {
    pub bass_db: f64,
    pub mid_db: f64,
    pub treble_db: f64,
    /// -1 (left only) ..= 1 (right only).
    pub balance: f64,
    /// Sound character: -1 warm ..= 1 bright.
    pub warmth: f64,
    /// Sound character: -1 relaxed ..= 1 energetic.
    pub energy: f64,
    /// More bass (and a little treble) the lower the volume.
    pub loudness: bool,
    /// Correction preset id ([`PRESETS`]), "custom" (then [`Dsp::bands`]) or "" (off).
    pub correction: String,
    /// The custom correction bands.
    pub bands: Vec<Band>,
    /// Gain before the limiter, -20..=20 dB (a correction preset adds its own).
    pub preamp_db: f64,
    pub crossfeed: bool,
    /// bs2b feeding level, 1..=15 dB (lower = stronger).
    pub crossfeed_db: f64,
    /// Limiter ceiling, -20..=0 dB.
    pub limit_db: f64,
}

impl Default for Dsp {
    fn default() -> Self {
        Self {
            bass_db: 0.0,
            mid_db: 0.0,
            treble_db: 0.0,
            balance: 0.0,
            warmth: 0.0,
            energy: 0.0,
            loudness: false,
            correction: String::new(),
            bands: default_bands(),
            preamp_db: 0.0,
            crossfeed: false,
            crossfeed_db: 4.5,
            limit_db: -1.0,
        }
    }
}

/// One correction band; its kind (shelf or peak) comes from its position.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Band {
    pub freq_hz: f64,
    pub gain_db: f64,
    pub q: f64,
}

/// Flat bands at spread-out frequencies (custom starts from these).
#[must_use]
pub fn default_bands() -> Vec<Band> {
    let freqs = [
        105.0, 60.0, 150.0, 400.0, 1000.0, 2500.0, 4000.0, 6500.0, 9000.0, 10000.0,
    ];
    freqs
        .into_iter()
        .enumerate()
        .map(|(i, freq_hz)| Band {
            freq_hz,
            gain_db: 0.0,
            // Shelves (first and last) at 0.7, the peaks at 1.
            q: if i == 0 || i == BANDS - 1 { 0.7 } else { 1.0 },
        })
        .collect()
}

/// A correction target: a preamp and the ten bands (low shelf, 8 peaks, high shelf).
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub preamp_db: f64,
    pub bands: [(f64, f64, f64); BANDS],
}

/// Built-in correction presets.
#[allow(
    clippy::approx_constant,
    reason = "AutoEq's Q of 3.14 is a measured value, not π (H15-54)"
)]
pub const PRESETS: &[Preset] = &[Preset {
    id: "sony-wh-1000xm4",
    label: "Sony WH-1000XM4 (AutoEq, oratory1990)",
    // AutoEq results/oratory1990/over-ear/Sony WH-1000XM4, ParametricEQ.txt (Harman target):
    // filter 1 (LSC) first, filter 6 (HSC) last, the peaks in between.
    preamp_db: -6.1,
    bands: [
        (105.0, -4.2, 0.70),
        (143.0, -5.2, 1.10),
        (2289.0, 6.1, 1.57),
        (56.0, 1.2, 1.19),
        (5144.0, -3.2, 6.00),
        (407.0, 1.7, 3.14),
        (6715.0, 3.0, 5.99),
        (1007.0, 1.0, 3.41),
        (576.0, -1.2, 3.55),
        (10000.0, -1.0, 0.70),
    ],
}];

impl Dsp {
    /// Clamped to the allowed ranges; NaN becomes the default; always [`BANDS`] bands.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        let clamp =
            |v: f64, lo: f64, hi: f64, default: f64| if v.is_nan() { default } else { v.clamp(lo, hi) };
        self.bass_db = clamp(self.bass_db, -MAX_TONE_DB, MAX_TONE_DB, 0.0);
        self.mid_db = clamp(self.mid_db, -MAX_TONE_DB, MAX_TONE_DB, 0.0);
        self.treble_db = clamp(self.treble_db, -MAX_TONE_DB, MAX_TONE_DB, 0.0);
        self.balance = clamp(self.balance, -1.0, 1.0, 0.0);
        self.warmth = clamp(self.warmth, -1.0, 1.0, 0.0);
        self.energy = clamp(self.energy, -1.0, 1.0, 0.0);
        self.preamp_db = clamp(self.preamp_db, -20.0, 20.0, 0.0);
        self.crossfeed_db = clamp(self.crossfeed_db, 1.0, 15.0, 4.5);
        self.limit_db = clamp(self.limit_db, -20.0, 0.0, -1.0);
        if self.bands.len() != BANDS {
            self.bands = default_bands();
        }
        for b in &mut self.bands {
            b.freq_hz = clamp(b.freq_hz, 20.0, 20000.0, 1000.0);
            b.gain_db = clamp(b.gain_db, -24.0, 24.0, 0.0);
            b.q = clamp(b.q, 0.1, 10.0, 1.0);
        }
        if !self.correction.is_empty() && self.correction != "custom" && preset(&self.correction).is_none() {
            self.correction.clear();
        }
        self
    }

    /// The correction in effect: preamp and bands (flat when off).
    fn correction_bands(&self) -> (f64, Vec<Band>) {
        if let Some(p) = preset(&self.correction) {
            let bands = p
                .bands
                .iter()
                .map(|&(freq_hz, gain_db, q)| Band { freq_hz, gain_db, q })
                .collect();
            return (p.preamp_db, bands);
        }
        if self.correction == "custom" {
            return (0.0, self.bands.clone());
        }
        let flat = default_bands();
        (0.0, flat)
    }

    /// The loudness boost (bass, treble in dB) at an output volume in percent (wpctl's cubic
    /// scale): none at 100 % and above, growing as the volume goes down.
    #[must_use]
    pub fn loudness_db(&self, volume_percent: u32) -> (f64, f64) {
        if !self.loudness {
            return (0.0, 0.0);
        }
        // Attenuation in dB of the cubic volume: 20·log10((v/100)³).
        let v = (f64::from(volume_percent) / 100.0).clamp(0.01, 1.0);
        let attenuation = -60.0 * v.log10();
        ((attenuation * 0.3).min(10.0), (attenuation * 0.1).min(4.0))
    }

    /// The shelf gains (bass, mid, treble) after folding in character and loudness.
    #[must_use]
    pub fn shelves(&self, volume_percent: u32) -> (f64, f64, f64) {
        let (loud_bass, loud_treble) = self.loudness_db(volume_percent);
        let s = |db: f64| db.clamp(-MAX_SHELF_DB, MAX_SHELF_DB);
        (
            s(self.bass_db - 4.0 * self.warmth + 3.0 * self.energy + loud_bass),
            s(self.mid_db - 1.5 * self.energy),
            s(self.treble_db + 4.0 * self.warmth + 3.0 * self.energy + loud_treble),
        )
    }

    /// Native coefficients are compiled at the actual stream rate by the player's device.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "H15-67: clamped balance/ceiling become f32 audio controls"
    )]
    pub fn native_settings(&self, volume_percent: u32) -> player::dsp::Settings {
        use player::dsp::{CrossfeedSettings, Filter, FilterKind, Settings};
        let (bass, mid, treble) = self.shelves(volume_percent);
        let (preamp, bands) = self.correction_bands();
        let mut filters = [Filter::default(); player::dsp::FILTERS];
        for (filter, (kind, frequency, gain_db)) in filters.iter_mut().zip([
            (FilterKind::LowShelf, 120.0, bass),
            (FilterKind::Peak, 1000.0, mid),
            (FilterKind::HighShelf, 8000.0, treble),
        ]) {
            *filter = Filter {
                kind,
                frequency,
                q: 0.7,
                gain_db,
            };
        }
        for (i, band) in bands.iter().enumerate() {
            filters[i + 3] = Filter {
                kind: match i {
                    0 => FilterKind::LowShelf,
                    9 => FilterKind::HighShelf,
                    _ => FilterKind::Peak,
                },
                frequency: band.freq_hz,
                gain_db: band.gain_db,
                q: band.q,
            };
        }
        Settings {
            filters,
            preamp_db: (self.preamp_db + preamp).clamp(-20.0, 20.0),
            balance: self.balance as f32,
            ceiling_db: self.limit_db as f32,
            crossfeed: self.crossfeed.then_some(CrossfeedSettings {
                cutoff: 700.0,
                feed_db: self.crossfeed_db,
            }),
        }
    }
}

#[must_use]
pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_state_files_still_load() {
        let old: Dsp =
            toml::from_str("bass_db = 3.0\nmid_db = 0.0\ntreble_db = -2.0\nbalance = 0.1\n").unwrap();
        assert_eq!((old.bass_db, old.treble_db, old.balance), (3.0, -2.0, 0.1));
        assert_eq!(old.bands.len(), BANDS);
        assert!(!old.loudness && !old.crossfeed);
    }

    #[test]
    fn character_and_loudness_fold_into_the_shelves() {
        let flat = Dsp::default();
        assert_eq!(flat.shelves(50), (0.0, 0.0, 0.0), "loudness off");
        let warm = Dsp {
            warmth: -1.0,
            ..Dsp::default()
        };
        assert_eq!(warm.shelves(100), (4.0, 0.0, -4.0));
        let loud = Dsp {
            loudness: true,
            ..Dsp::default()
        };
        assert_eq!(loud.loudness_db(100), (0.0, 0.0), "no boost at full volume");
        let (bass, treble) = loud.loudness_db(50);
        assert!(
            (bass - 5.42).abs() < 0.01 && (treble - 1.81).abs() < 0.01,
            "{bass} {treble}"
        );
        assert_eq!(loud.loudness_db(1), (10.0, 4.0), "capped");
    }

    #[test]
    fn native_settings_preserve_tone_preset_and_volume_dependent_loudness() {
        let dsp = Dsp {
            correction: "sony-wh-1000xm4".into(),
            loudness: true,
            balance: 0.5,
            crossfeed: true,
            ..Default::default()
        };
        let settings = dsp.native_settings(50);
        assert_eq!(settings.filters[0].frequency, 120.0);
        assert_eq!(settings.filters[1].q, 0.7);
        assert_eq!(settings.filters[2].frequency, 8000.0);
        assert_eq!(settings.filters[3].kind, player::dsp::FilterKind::LowShelf);
        assert_eq!(settings.filters[5].gain_db, 6.1);
        assert_eq!(settings.filters[12].kind, player::dsp::FilterKind::HighShelf);
        assert_eq!(settings.preamp_db, -6.1);
        assert_eq!(settings.balance, 0.5);
        assert_eq!(settings.crossfeed.unwrap().cutoff, 700.0);
        assert!(settings.filters[0].gain_db > dsp.native_settings(100).filters[0].gain_db);
        for rate in [16_000, 22_050, 24_000, 44_100, 48_000] {
            assert!(player::dsp::Compiled::new(rate, 1, settings).is_ok());
        }
    }

    #[test]
    fn clamping_repairs_bad_values() {
        let d = Dsp {
            bass_db: 40.0,
            warmth: f64::NAN,
            correction: "nope".into(),
            bands: vec![],
            ..Dsp::default()
        }
        .clamped();
        assert_eq!((d.bass_db, d.warmth), (12.0, 0.0));
        assert_eq!(d.correction, "");
        assert_eq!(d.bands.len(), BANDS);
    }
}

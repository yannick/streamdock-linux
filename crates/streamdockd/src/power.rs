//! Dock power settings (H15-24): sleep after idle minutes, and an optional night window with its
//! own brightness. OpenDeck's `sleep_timeout_minutes` and brightness, for a headless daemon.
//!
//! ```toml
//! sleep_after_minutes = 10   # screen off after this long without input; 0 or absent = never
//!
//! [night]                    # optional: dimmed (or off, brightness = 0) in this window, local time
//! from = "23:00"
//! to = "07:00"
//! brightness = 10
//! ```

use std::time::Duration;

use anyhow::{Context as _, bail};
use chrono::Timelike as _;
use serde::Deserialize;

/// Power settings of every dock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Power {
    /// `None`: never sleep.
    pub sleep_after: Option<Duration>,
    pub night: Option<Night>,
}

/// A daily window (local time) with its own brightness; `to` may be past midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Night {
    /// Minutes after midnight.
    from: u16,
    to: u16,
    pub brightness: u8,
}

/// `[night]` as written in the config file.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NightFile {
    from: String,
    to: String,
    brightness: u8,
}

impl Power {
    pub fn from_file(sleep_after_minutes: Option<u32>, night: Option<&NightFile>) -> anyhow::Result<Self> {
        let sleep_after = match sleep_after_minutes {
            None | Some(0) => None,
            Some(m) => Some(Duration::from_secs(u64::from(m) * 60)),
        };
        let night = night
            .map(|n| -> anyhow::Result<Night> {
                anyhow::ensure!(n.brightness <= 100, "[night] brightness must be 0..=100");
                let from = parse_time(&n.from).context("[night] from")?;
                let to = parse_time(&n.to).context("[night] to")?;
                anyhow::ensure!(from != to, "[night] from and to must differ");
                Ok(Night {
                    from,
                    to,
                    brightness: n.brightness,
                })
            })
            .transpose()?;
        Ok(Self { sleep_after, night })
    }

    /// The brightness to show now: the night brightness inside the window, else `day`.
    #[must_use]
    pub fn brightness_at(&self, minute_of_day: u16, day: u8) -> u8 {
        match self.night {
            Some(n) if n.contains(minute_of_day) => n.brightness,
            _ => day,
        }
    }

    /// Whether the night window is active at `minute_of_day`.
    #[must_use]
    pub fn is_night(&self, minute_of_day: u16) -> bool {
        self.night.is_some_and(|n| n.contains(minute_of_day))
    }
}

impl Night {
    fn contains(self, minute: u16) -> bool {
        if self.from < self.to {
            (self.from..self.to).contains(&minute)
        } else {
            // Across midnight, e.g. 23:00-07:00.
            minute >= self.from || minute < self.to
        }
    }
}

/// `"HH:MM"` (24 h) to minutes after midnight.
fn parse_time(text: &str) -> anyhow::Result<u16> {
    let Some((h, m)) = text.split_once(':') else {
        bail!("{text:?} is not HH:MM");
    };
    let (h, m): (u16, u16) = (
        h.trim()
            .parse()
            .with_context(|| format!("{text:?} is not HH:MM"))?,
        m.trim()
            .parse()
            .with_context(|| format!("{text:?} is not HH:MM"))?,
    );
    anyhow::ensure!(h < 24 && m < 60, "{text:?} is not a time of day");
    Ok(h * 60 + m)
}

/// Minutes after local midnight, now.
#[must_use]
pub fn minute_now() -> u16 {
    let now = chrono::Local::now();
    u16::try_from(now.hour() * 60 + now.minute()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn night(from: &str, to: &str, brightness: u8) -> NightFile {
        NightFile {
            from: from.into(),
            to: to.into(),
            brightness,
        }
    }

    #[test]
    fn sleep_minutes() {
        assert_eq!(Power::from_file(None, None).unwrap().sleep_after, None);
        assert_eq!(Power::from_file(Some(0), None).unwrap().sleep_after, None);
        assert_eq!(
            Power::from_file(Some(10), None).unwrap().sleep_after,
            Some(Duration::from_secs(600))
        );
    }

    #[test]
    fn night_window_across_midnight() {
        let p = Power::from_file(None, Some(&night("23:00", "07:00", 10))).unwrap();
        assert_eq!(p.brightness_at(22 * 60 + 59, 80), 80);
        assert_eq!(p.brightness_at(23 * 60, 80), 10);
        assert_eq!(p.brightness_at(3 * 60, 80), 10);
        assert_eq!(p.brightness_at(7 * 60, 80), 80);
        assert!(p.is_night(0));
    }

    #[test]
    fn night_window_within_a_day() {
        let p = Power::from_file(None, Some(&night("12:30", "13:00", 0))).unwrap();
        assert_eq!(p.brightness_at(12 * 60 + 29, 70), 70);
        assert_eq!(p.brightness_at(12 * 60 + 30, 70), 0);
        assert_eq!(p.brightness_at(13 * 60, 70), 70);
    }

    #[test]
    fn rejects_bad_windows() {
        assert!(Power::from_file(None, Some(&night("25:00", "07:00", 10))).is_err());
        assert!(Power::from_file(None, Some(&night("23", "07:00", 10))).is_err());
        assert!(Power::from_file(None, Some(&night("07:00", "07:00", 10))).is_err());
        assert!(Power::from_file(None, Some(&night("23:00", "07:00", 101))).is_err());
    }
}

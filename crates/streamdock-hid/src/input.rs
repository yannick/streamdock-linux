//! Input report parsing (`docs/protocol.md`, "Input reports").
//!
//! The dock sends 512-byte input reports. Events start with `ACK\0\0OK\0\0`, followed by the input
//! code (byte 9) and the state (byte 10). Code 0 is not an input: it acknowledges an upload.

use std::fmt;

/// Length of one input report.
pub const REPORT_LEN: usize = 512;

/// A key: 1..=6 are the LCD keys (row-major from the top left, 3×2), 7..=9 the plain buttons
/// below them (left to right).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(u8);

impl Key {
    /// Number of keys (LCD keys and plain buttons).
    pub const COUNT: u8 = 9;
    /// Number of keys with a display.
    pub const LCD_COUNT: u8 = 6;

    /// Returns the key with this number, if it is in 1..=9.
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number <= Self::COUNT {
            Some(Self(number))
        } else {
            None
        }
    }

    /// The key number, 1..=9.
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }

    /// Whether the key has a display (keys 1..=6).
    #[must_use]
    pub const fn is_lcd(self) -> bool {
        self.0 <= Self::LCD_COUNT
    }

    /// All keys in order.
    pub fn all() -> impl Iterator<Item = Self> {
        (1..=Self::COUNT).map(Self)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "key {}", self.0)
    }
}

/// A knob, numbered 1..=3 from left to right: 1 is the left knob, 2 the top (middle) one, 3 the
/// right one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Knob(u8);

impl Knob {
    /// Number of knobs.
    pub const COUNT: u8 = 3;

    /// Returns the knob with this number, if it is in 1..=3.
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number <= Self::COUNT {
            Some(Self(number))
        } else {
            None
        }
    }

    /// The knob number, 1..=3.
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }
}

impl fmt::Display for Knob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "knob {}", self.0)
    }
}

/// Something the user did on the dock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputEvent {
    KeyDown(Key),
    KeyUp(Key),
    KnobDown(Knob),
    KnobUp(Knob),
    /// Detents turned: positive clockwise, negative counter-clockwise. The dock sends one report
    /// per detent, so this is always ±1.
    KnobTurn {
        knob: Knob,
        delta: i8,
    },
}

/// A parsed input report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    Input(InputEvent),
    /// `ACK` with code 0: the dock acknowledged an upload.
    UploadAck,
    /// An `ACK` report with a code we don't know.
    Unknown {
        code: u8,
        state: u8,
    },
    /// Not an `ACK…OK` event report (other replies, empty reads).
    Ignored,
}

const HEADER: &[u8; 9] = b"ACK\0\0OK\0\0";
const STATE_DOWN: u8 = 0x01;

/// Parses one input report.
#[must_use]
pub fn parse_report(report: &[u8]) -> Report {
    let (Some(header), Some(&code), Some(&state)) = (report.get(..9), report.get(9), report.get(10)) else {
        return Report::Ignored;
    };
    if header != HEADER {
        return Report::Ignored;
    }
    if code == 0 {
        return Report::UploadAck;
    }
    match decode(code, state) {
        Some(event) => Report::Input(event),
        None => Report::Unknown { code, state },
    }
}

/// Maps an input code and state byte to an event. The state byte is `0x01` for down; the vendor
/// SDKs disagree on the release value (`0x00` or `0x02`), so anything else counts as up.
#[must_use]
pub fn decode(code: u8, state: u8) -> Option<InputEvent> {
    let down = state == STATE_DOWN;
    let key = |n| {
        let key = Key(n);
        Some(if down {
            InputEvent::KeyDown(key)
        } else {
            InputEvent::KeyUp(key)
        })
    };
    let press = |n| {
        let knob = Knob(n);
        Some(if down {
            InputEvent::KnobDown(knob)
        } else {
            InputEvent::KnobUp(knob)
        })
    };
    let turn = |n, delta| Some(InputEvent::KnobTurn { knob: Knob(n), delta });

    match code {
        0x01..=0x06 => key(code),
        0x25 => key(7),
        0x30 => key(8),
        0x31 => key(9),
        0x33 => press(1),
        0x35 => press(2),
        0x34 => press(3),
        0x90 => turn(1, -1),
        0x91 => turn(1, 1),
        0x50 => turn(2, -1),
        0x51 => turn(2, 1),
        0x60 => turn(3, -1),
        0x61 => turn(3, 1),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(code: u8, state: u8) -> Vec<u8> {
        let mut r = vec![0u8; REPORT_LEN];
        r[..9].copy_from_slice(HEADER);
        r[9] = code;
        r[10] = state;
        r
    }

    fn key(n: u8) -> Key {
        Key::new(n).unwrap()
    }

    fn knob(n: u8) -> Knob {
        Knob::new(n).unwrap()
    }

    #[test]
    fn lcd_keys_down_and_up() {
        for n in 1..=6 {
            assert_eq!(
                parse_report(&report(n, 1)),
                Report::Input(InputEvent::KeyDown(key(n)))
            );
            assert_eq!(
                parse_report(&report(n, 0)),
                Report::Input(InputEvent::KeyUp(key(n)))
            );
        }
    }

    #[test]
    fn plain_buttons() {
        assert_eq!(decode(0x25, 1), Some(InputEvent::KeyDown(key(7))));
        assert_eq!(decode(0x30, 1), Some(InputEvent::KeyDown(key(8))));
        assert_eq!(decode(0x31, 0), Some(InputEvent::KeyUp(key(9))));
        assert!(!key(7).is_lcd());
        assert!(key(6).is_lcd());
    }

    #[test]
    fn release_state_0x02_is_up() {
        assert_eq!(decode(0x02, 0x02), Some(InputEvent::KeyUp(key(2))));
    }

    #[test]
    fn knob_press_and_turn() {
        assert_eq!(decode(0x33, 1), Some(InputEvent::KnobDown(knob(1))));
        assert_eq!(decode(0x35, 0), Some(InputEvent::KnobUp(knob(2))));
        assert_eq!(decode(0x34, 1), Some(InputEvent::KnobDown(knob(3))));
        assert_eq!(
            decode(0x90, 0),
            Some(InputEvent::KnobTurn {
                knob: knob(1),
                delta: -1
            })
        );
        assert_eq!(
            decode(0x91, 0),
            Some(InputEvent::KnobTurn {
                knob: knob(1),
                delta: 1
            })
        );
        assert_eq!(
            decode(0x50, 0),
            Some(InputEvent::KnobTurn {
                knob: knob(2),
                delta: -1
            })
        );
        assert_eq!(
            decode(0x61, 0),
            Some(InputEvent::KnobTurn {
                knob: knob(3),
                delta: 1
            })
        );
    }

    #[test]
    fn upload_ack_unknown_and_garbage() {
        assert_eq!(parse_report(&report(0, 0)), Report::UploadAck);
        assert_eq!(
            parse_report(&report(0x77, 1)),
            Report::Unknown { code: 0x77, state: 1 }
        );
        assert_eq!(parse_report(&[0u8; REPORT_LEN]), Report::Ignored);
        assert_eq!(parse_report(b"ACK"), Report::Ignored);
        let mut not_ok = report(1, 1);
        not_ok[5] = b'X';
        assert_eq!(parse_report(&not_ok), Report::Ignored);
    }

    #[test]
    fn key_and_knob_ranges() {
        assert!(Key::new(0).is_none());
        assert!(Key::new(10).is_none());
        assert_eq!(Key::all().count(), 9);
        assert!(Knob::new(4).is_none());
    }
}

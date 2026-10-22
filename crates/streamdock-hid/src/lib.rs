//! USB HID driver for the StreamDock "293s" sold by FHOOU / Mirabox (USB `6603:1003`).
//!
//! Reverse engineering showed it is the Mirabox N3 / Ajazz AKP03 family: 6 LCD keys (3×2), 3 plain
//! buttons and 3 knobs that turn and press. The protocol is in `docs/protocol.md` (H15-2, H15-9).
//!
//! ```no_run
//! # async fn demo() -> Result<(), streamdock_hid::Error> {
//! let dock = streamdock_hid::Dock::open_first().await?;
//! while let Some(event) = dock.next_event(None).await? {
//!     println!("{event:?}");
//! }
//! # Ok(()) }
//! ```

mod dock;
mod image;
mod input;

pub use dock::{DEFAULT_BRIGHTNESS, Dock, DockInfo, Error, KEEP_ALIVE_INTERVAL, USAGE_ID, USAGE_PAGE, list};
pub use image::{JPEG_QUALITY, KEY_IMAGE_SIZE, encode_key_image};
pub use input::{InputEvent, Key, Knob, Report, decode, parse_report};

/// USB vendor id of the attached dock.
pub const VENDOR_ID: u16 = 0x6603;
/// USB product id of the attached dock.
pub const PRODUCT_ID: u16 = 0x1003;

//! An opened dock: commands and the input event stream, on top of `mirajazz` (ADR-001).
//!
//! `mirajazz` handles discovery, the `CRT` command framing and image chunking. Input reports are
//! parsed here (`crate::input`), so key down and up come straight from the state byte.

use std::sync::Arc;
use std::time::Duration;

use futures_lite::StreamExt;
use image::DynamicImage;
use mirajazz::device::{Device, DeviceQuery, list_devices};
use mirajazz::error::MirajazzError;
use mirajazz::state::DeviceStateReader;
use mirajazz::types::{DeviceInput, HidDevice};

use crate::image::encode_key_image;
use crate::input::{InputEvent, Key, Knob, REPORT_LEN, Report, parse_report};
use crate::{PRODUCT_ID, VENDOR_ID};

/// HID usage page of the dock's vendor interface.
pub const USAGE_PAGE: u16 = 0xFFA0;
/// HID usage of the dock's vendor interface.
pub const USAGE_ID: u16 = 0x01;
/// `mirajazz` protocol version of the dock: `CRT` commands in 1024-byte reports, key up and down.
const PROTOCOL_VERSION: usize = 3;
/// Send a keep-alive at least this often while the dock is open.
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// Brightness set when a dock is opened.
pub const DEFAULT_BRIGHTNESS: u8 = 80;

const QUERY: DeviceQuery = DeviceQuery::new(USAGE_PAGE, USAGE_ID, VENDOR_ID, PRODUCT_ID);

/// Errors from the dock.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no StreamDock {VENDOR_ID:04x}:{PRODUCT_ID:04x} found")]
    NotFound,
    /// Reading or writing failed; the dock was most likely unplugged. Drop the `Dock` and open it
    /// again once it is back.
    #[error("dock disconnected: {0}")]
    Disconnected(MirajazzError),
    #[error("{0} has no display")]
    NoDisplay(Key),
    #[error("encoding the key image failed: {0}")]
    Image(#[from] image::ImageError),
    #[error(transparent)]
    Device(MirajazzError),
}

impl From<MirajazzError> for Error {
    fn from(e: MirajazzError) -> Self {
        match e {
            MirajazzError::HidError(_) => Self::Disconnected(e),
            MirajazzError::DeviceNotFoundError => Self::NotFound,
            e => Self::Device(e),
        }
    }
}

/// A dock found on the system, not opened yet.
#[derive(Debug)]
pub struct DockInfo {
    device: HidDevice,
}

impl DockInfo {
    /// The USB serial number, which identifies the dock.
    #[must_use]
    pub fn serial_number(&self) -> Option<&str> {
        self.device.serial_number.as_deref()
    }
}

/// Lists the attached docks (their vendor HID interface).
pub async fn list() -> Result<Vec<DockInfo>, Error> {
    let devices = list_devices(&[QUERY]).await?;
    let mut docks: Vec<_> = devices.into_iter().map(|device| DockInfo { device }).collect();
    docks.sort_by(|a, b| a.serial_number().cmp(&b.serial_number()));
    Ok(docks)
}

/// An opened dock.
///
/// Methods take `&self`, so one task can wait in [`Dock::next_event`] while others send commands.
/// It needs a multi-threaded tokio runtime.
pub struct Dock {
    device: Device,
    reader: Arc<DeviceStateReader>,
}

impl std::fmt::Debug for Dock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dock")
            .field("serial_number", &self.device.serial_number)
            .field("firmware_version", &self.firmware_version())
            .finish_non_exhaustive()
    }
}

impl Dock {
    /// Opens the first attached dock.
    pub async fn open_first() -> Result<Self, Error> {
        let info = list().await?.into_iter().next().ok_or(Error::NotFound)?;
        Self::open(&info).await
    }

    /// Opens a dock: wakes the screen, sets [`DEFAULT_BRIGHTNESS`] and clears all keys.
    pub async fn open(info: &DockInfo) -> Result<Self, Error> {
        let device = Device::connect(
            &info.device,
            PROTOCOL_VERSION,
            usize::from(Key::COUNT),
            usize::from(Knob::COUNT),
        )
        .await?;
        let reader = device.get_reader(ignore_input);
        let dock = Self { device, reader };
        dock.set_brightness(DEFAULT_BRIGHTNESS).await?;
        dock.clear_all().await?;
        tracing::info!(serial = %dock.serial_number(), firmware = ?dock.firmware_version(), "dock opened");
        Ok(dock)
    }

    /// The USB serial number.
    #[must_use]
    pub fn serial_number(&self) -> &str {
        &self.device.serial_number
    }

    /// The firmware version string, e.g. `V25.293N3_E.01.004`.
    #[must_use]
    pub fn firmware_version(&self) -> Option<&str> {
        // The feature report is NUL-padded.
        self.device
            .firmware_version
            .as_deref()
            .map(|v| v.trim_end_matches('\0'))
            .filter(|v| !v.is_empty())
    }

    /// Sets the LCD brightness, 0..=100.
    pub async fn set_brightness(&self, percent: u8) -> Result<(), Error> {
        Ok(self.device.set_brightness(percent.min(100)).await?)
    }

    /// Shows `image` on an LCD key (1..=6). It is scaled to 64×64.
    pub async fn set_key_image(&self, key: Key, image: &DynamicImage) -> Result<(), Error> {
        if !key.is_lcd() {
            return Err(Error::NoDisplay(key));
        }
        let jpeg = encode_key_image(image)?;
        self.device.write_image(key.number() - 1, &jpeg).await?;
        Ok(self.device.flush().await?)
    }

    /// Clears an LCD key.
    pub async fn clear_key(&self, key: Key) -> Result<(), Error> {
        if !key.is_lcd() {
            return Err(Error::NoDisplay(key));
        }
        self.device.clear_button_image(key.number() - 1).await?;
        Ok(self.device.flush().await?)
    }

    /// Clears all LCD keys.
    pub async fn clear_all(&self) -> Result<(), Error> {
        Ok(self.device.clear_all_button_images().await?)
    }

    /// Tells the dock the host is still there. Call it every [`KEEP_ALIVE_INTERVAL`].
    pub async fn keep_alive(&self) -> Result<(), Error> {
        Ok(self.device.keep_alive().await?)
    }

    /// Turns the screen off (`HAN`). Input still arrives; [`Dock::wake`] turns it back on.
    pub async fn sleep(&self) -> Result<(), Error> {
        Ok(self.device.sleep().await?)
    }

    /// Turns the screen back on after [`Dock::sleep`]: `DIS` (the same wake command the dock gets
    /// when it is opened), then `brightness`. Key images may have to be uploaded again.
    pub async fn wake(&self, brightness: u8) -> Result<(), Error> {
        // mirajazz frames commands with a leading report-id byte; it pads to the packet size.
        let mut dis = vec![0x00, b'C', b'R', b'T', 0x00, 0x00, b'D', b'I', b'S'];
        self.device.write_extended_data(&mut dis).await?;
        self.set_brightness(brightness).await
    }

    /// Clears the screen and turns it off, for a clean exit.
    pub async fn shutdown(&self) -> Result<(), Error> {
        Ok(self.device.shutdown().await?)
    }

    /// Waits for the next input event. With a `timeout`, returns `None` when it passes first.
    pub async fn next_event(&self, timeout: Option<Duration>) -> Result<Option<InputEvent>, Error> {
        loop {
            let report = match timeout {
                Some(t) => match self.reader.raw_read_data_with_timeout(REPORT_LEN, t).await? {
                    Some(r) => r,
                    None => return Ok(None),
                },
                None => self.reader.raw_read_data(REPORT_LEN).await?,
            };
            match parse_report(&report) {
                Report::Input(event) => return Ok(Some(event)),
                Report::UploadAck | Report::Ignored => {}
                Report::Unknown { code, state } => {
                    tracing::debug!(code, state, "unknown input code");
                }
            }
        }
    }

    /// Input events as a stream; it ends after the first error.
    pub fn events(&self) -> impl futures_lite::Stream<Item = Result<InputEvent, Error>> + '_ {
        futures_lite::stream::unfold(Some(self), |dock| async move {
            let dock = dock?;
            match dock.next_event(None).await {
                Ok(Some(event)) => Some((Ok(event), Some(dock))),
                Ok(None) => None,
                Err(e) => Some((Err(e), None)),
            }
        })
        .boxed_local()
    }
}

/// `mirajazz` wants an input mapper for its state reader; we read raw reports instead.
#[allow(clippy::unnecessary_wraps)]
fn ignore_input(_code: u8, _state: u8) -> Result<DeviceInput, MirajazzError> {
    Ok(DeviceInput::NoData)
}

//! Opens the dock, paints each LCD key and logs every input event.
//!
//! ```sh
//! cargo run -p streamdock-hid --example probe            # paint keys, log events
//! cargo run -p streamdock-hid --example probe -- --info  # only show the dock and exit
//! ```

use anyhow::Context;
use image::{DynamicImage, Rgb, RgbImage};
use streamdock_hid::{Dock, InputEvent, KEEP_ALIVE_INTERVAL, Key};

const COLORS: [[u8; 3]; 6] = [
    [255, 0, 80],
    [0, 255, 200],
    [255, 220, 0],
    [140, 0, 255],
    [0, 140, 255],
    [255, 120, 0],
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let info_only = std::env::args().any(|a| a == "--info");

    for dock in streamdock_hid::list().await? {
        tracing::info!(serial = ?dock.serial_number(), "found dock");
    }
    if info_only {
        return Ok(());
    }

    let dock = Dock::open_first().await.context("opening the dock")?;
    tracing::info!(?dock, "opened");

    for (key, color) in Key::all().filter(|k| k.is_lcd()).zip(COLORS) {
        dock.set_key_image(key, &key_tile(key, color)).await?;
    }

    let mut keep_alive = tokio::time::interval(KEEP_ALIVE_INTERVAL);
    loop {
        tokio::select! {
            _ = keep_alive.tick() => dock.keep_alive().await?,
            event = dock.next_event(None) => match event? {
                Some(event) => log_event(event),
                None => break,
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    dock.shutdown().await?;
    Ok(())
}

#[allow(
    clippy::cognitive_complexity,
    reason = "one log line per event kind; clippy counts the tracing macros (H15-36)"
)]
fn log_event(event: InputEvent) {
    match event {
        InputEvent::KeyDown(key) => tracing::info!(%key, "down"),
        InputEvent::KeyUp(key) => tracing::info!(%key, "up"),
        InputEvent::KnobDown(knob) => tracing::info!(%knob, "pressed"),
        InputEvent::KnobUp(knob) => tracing::info!(%knob, "released"),
        InputEvent::KnobTurn { knob, delta } => tracing::info!(%knob, delta, "turned"),
    }
}

/// A colored tile with a white triangle in the top-left corner and one white bar per key number
/// along the bottom, so orientation and key order are visible on the device.
fn key_tile(key: Key, [r, g, b]: [u8; 3]) -> DynamicImage {
    let mut img = RgbImage::from_pixel(64, 64, Rgb([r, g, b]));
    for y in 0..20 {
        for x in 0..20 - y {
            img.put_pixel(x, y, Rgb([255, 255, 255]));
        }
    }
    for n in 0..u32::from(key.number()) {
        for y in 48..58 {
            for x in 0..6 {
                img.put_pixel(8 + n * 9 + x, y, Rgb([255, 255, 255]));
            }
        }
    }
    DynamicImage::ImageRgb8(img)
}

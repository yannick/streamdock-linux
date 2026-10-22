//! The built-in key icons (H15-48): line icons for the dock's 64×64 keys, in one style (24-unit
//! grid, 2-unit round strokes), compiled into the daemon. Keys use them like uploaded images
//! (`image = "light-on"`); an upload with the same name wins. They are drawn white on transparent
//! and tinted by the key's state when rendered (`render.rs`), so they always match the theme.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};

use image::RgbaImage;

/// Size of the icon in the asset list and the review sheet. Keys rasterise the SVG at the size
/// they draw it (48, 36 or 30 px) instead of scaling a bitmap, so strokes stay crisp.
pub const RASTER: u32 = 96;

/// `(name, group, SVG body)`. Bodies use the defaults below; filled shapes say `fill="#fff"`.
#[allow(
    clippy::needless_raw_string_hashes,
    reason = "one quoting style for every icon body; some contain \"#fff\""
)]
pub const ICONS: &[(&str, &str, &str)] = &[
    // Lights
    (
        "light-on",
        "Lights",
        r##"<path d="M9 18.5h6M10 21.5h4M12 6.5a5 5 0 0 0-3.5 8.6c.7.7 1 1.4 1 2.4h5c0-1 .3-1.7 1-2.4A5 5 0 0 0 12 6.5z"/><path d="M12 2v1.5M4.5 5l1 1M19.5 5l-1 1M2 11.5h1.5M20.5 11.5H22"/>"##,
    ),
    (
        "light-off",
        "Lights",
        r##"<path d="M9 18h6M10 21h4M12 4a6 6 0 0 0-4 10.5c.8.8 1 1.5 1 2.5h6c0-1 .2-1.7 1-2.5A6 6 0 0 0 12 4z"/><path d="M3 3l18 18"/>"##,
    ),
    (
        "brightness-up",
        "Lights",
        r##"<circle cx="12" cy="12" r="4"/><path d="M12 1.5v3M12 19.5v3M1.5 12h3M19.5 12h3M4.6 4.6l2.1 2.1M17.3 17.3l2.1 2.1M4.6 19.4l2.1-2.1M17.3 6.7l2.1-2.1"/>"##,
    ),
    (
        "brightness-down",
        "Lights",
        r##"<circle cx="12" cy="12" r="4"/><path d="M12 5v1M12 18v1M5 12h1M18 12h1M7 7l.7.7M16.3 16.3l.7.7M7 17l.7-.7M16.3 7.7l.7-.7"/>"##,
    ),
    (
        "color-temp",
        "Lights",
        r##"<path d="M12 14.8V4.5a2 2 0 0 0-4 0v10.3a4 4 0 1 0 4 0z"/><path d="M10 10v7"/><path d="M15.5 5h3M15.5 9h3M15.5 13h2"/>"##,
    ),
    (
        "warm",
        "Lights",
        r##"<path d="M12 2.5c.5 3.5 6.5 6 6.5 12a6.5 6.5 0 0 1-13 0c0-2.5 1-4.5 3-6.5 0 2.5 1.2 4 2.5 4.5-.5-3.5.5-7 1-10z"/><path d="M12 21a2.5 2.5 0 0 1-2.5-2.5c0-1.5 1-2.5 2.5-4 1.5 1.5 2.5 2.5 2.5 4A2.5 2.5 0 0 1 12 21z" fill="#fff"/>"##,
    ),
    (
        "cold",
        "Lights",
        r##"<path d="M12 2v20M3.3 7l17.4 10M3.3 17L20.7 7M9 4l3 2 3-2M9 20l3-2 3 2M4 10.5l3.5-.5-1-3.5M20 13.5l-3.5.5 1 3.5M4 13.5l3.5.5-1 3.5M20 10.5l-3.5-.5 1-3.5"/>"##,
    ),
    (
        "color",
        "Lights",
        r##"<path d="M12 21a9 9 0 1 1 9-9c0 2-1.5 3.5-3.5 3.5H16a2 2 0 0 0-1.5 3.3A1.6 1.6 0 0 1 12 21z"/><circle cx="7.5" cy="11" r="1.3" fill="#fff" stroke="none"/><circle cx="10" cy="7" r="1.3" fill="#fff" stroke="none"/><circle cx="15" cy="7" r="1.3" fill="#fff" stroke="none"/>"##,
    ),
    (
        "scene",
        "Lights",
        r##"<path d="M12 3l1.8 4.7L18.5 9.5l-4.7 1.8L12 16l-1.8-4.7L5.5 9.5l4.7-1.8z"/><path d="M19 15l.8 2.2 2.2.8-2.2.8L19 21l-.8-2.2-2.2-.8 2.2-.8zM5 15l.6 1.4 1.4.6-1.4.6L5 19l-.6-1.4L3 17l1.4-.6z"/>"##,
    ),
    (
        "night",
        "Lights",
        r##"<path d="M19.5 14.5A8 8 0 1 1 10 4.5a6.5 6.5 0 0 0 9.5 10z"/>"##,
    ),
    // Blinds
    (
        "blinds",
        "Blinds",
        r##"<path d="M2 3h20M4 3v14h16V3M4 7.5h16M4 12h16M12 17v3"/><circle cx="12" cy="20.5" r="1" fill="#fff"/>"##,
    ),
    (
        "blinds-up",
        "Blinds",
        r##"<path d="M2 3h20M4 3v7h16V3M4 6.5h16"/><path d="M12 21v-7M8.5 17.5L12 14l3.5 3.5"/>"##,
    ),
    (
        "blinds-down",
        "Blinds",
        r##"<path d="M2 3h20M4 3v7h16V3M4 6.5h16"/><path d="M12 13.5v7M8.5 17l3.5 3.5 3.5-3.5"/>"##,
    ),
    (
        "blinds-stop",
        "Blinds",
        r##"<path d="M2 3h20M4 3v7h16V3M4 6.5h16"/><rect x="8.5" y="14" width="7" height="7" rx="1.5" fill="#fff"/>"##,
    ),
    ("up", "Blinds", r##"<path d="M4.5 16l7.5-7.5 7.5 7.5"/>"##),
    ("down", "Blinds", r##"<path d="M4.5 8l7.5 7.5 7.5-7.5"/>"##),
    // Power and home
    (
        "power",
        "Home",
        r##"<path d="M12 2v10M18.4 6.6a9 9 0 1 1-12.8 0"/>"##,
    ),
    (
        "home",
        "Home",
        r##"<path d="M3 11l9-8 9 8M5 9.5V21h5v-6h4v6h5V9.5"/>"##,
    ),
    (
        "plug",
        "Home",
        r##"<path d="M9 2v5M15 2v5M6 7h12v4a6 6 0 0 1-12 0zM12 17v5"/>"##,
    ),
    (
        "fan",
        "Home",
        r##"<circle cx="12" cy="12" r="2"/><path d="M12 10c0-4 1-7 4-7 2 0 3 2 2 4-1 1.5-3.5 2.5-6 3zM14 12c4 0 7 1 7 4 0 2-2 3-4 2-1.5-1-2.5-3.5-3-6zM12 14c0 4-1 7-4 7-2 0-3-2-2-4 1-1.5 3.5-2.5 6-3zM10 12c-4 0-7-1-7-4 0-2 2-3 4-2 1.5 1 2.5 3.5 3 6z"/>"##,
    ),
    (
        "heat",
        "Home",
        r##"<rect x="3" y="4" width="18" height="13.5" rx="2"/><path d="M7.5 4v13.5M12 4v13.5M16.5 4v13.5M6 17.5V20M18 17.5V20"/>"##,
    ),
    (
        "lock",
        "Home",
        r##"<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4M12 15v2"/>"##,
    ),
    (
        "unlock",
        "Home",
        r##"<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 7.5-2M12 15v2"/>"##,
    ),
    (
        "door",
        "Home",
        r##"<path d="M5 21V3h11v18M3 21h18M16 5l3 1v15"/><circle cx="13" cy="12" r="1" fill="#fff" stroke="none"/>"##,
    ),
    (
        "bell",
        "Home",
        r##"<path d="M6 14.5V10a6 6 0 0 1 12 0v4.5l2 2.5H4zM10 20h4"/>"##,
    ),
    // Audio
    (
        "play",
        "Audio",
        r##"<path d="M7.25 4.5l12 7.5-12 7.5z" fill="#fff"/>"##,
    ),
    (
        "pause",
        "Audio",
        r##"<rect x="6" y="5" width="4" height="14" rx="1" fill="#fff"/><rect x="14" y="5" width="4" height="14" rx="1" fill="#fff"/>"##,
    ),
    (
        "play-pause",
        "Audio",
        r##"<path d="M3.5 6l8 6-8 6z" fill="#fff"/><path d="M15.5 6v12M20.5 6v12"/>"##,
    ),
    (
        "stop",
        "Audio",
        r##"<rect x="6" y="6" width="12" height="12" rx="1.5" fill="#fff"/>"##,
    ),
    (
        "next",
        "Audio",
        r##"<path d="M5 5l10 7-10 7z" fill="#fff"/><path d="M19 5v14"/>"##,
    ),
    (
        "previous",
        "Audio",
        r##"<path d="M19 5L9 12l10 7z" fill="#fff"/><path d="M5 5v14"/>"##,
    ),
    (
        "fast-forward",
        "Audio",
        r##"<path d="M2.5 6l9 6-9 6zM12.5 6l9 6-9 6z" fill="#fff"/>"##,
    ),
    (
        "rewind",
        "Audio",
        r##"<path d="M21.5 6l-9 6 9 6zM11.5 6l-9 6 9 6z" fill="#fff"/>"##,
    ),
    (
        "seek-forward",
        "Audio",
        r##"<path d="M19 13a7 7 0 1 1-2.05-4.95"/><path d="M18.5 4v4.5H14"/><path d="M10 10.5l4.5 2.5-4.5 2.5z" fill="#fff"/>"##,
    ),
    (
        "seek-back",
        "Audio",
        r##"<path d="M5 13a7 7 0 1 0 2.05-4.95"/><path d="M5.5 4v4.5H10"/><path d="M14 10.5L9.5 13l4.5 2.5z" fill="#fff"/>"##,
    ),
    (
        "record",
        "Audio",
        r##"<circle cx="12" cy="12" r="7" fill="#fff"/>"##,
    ),
    (
        "eject",
        "Audio",
        r##"<path d="M12 4.5l8 9H4z" fill="#fff"/><rect x="4" y="16.5" width="16" height="3" rx="1" fill="#fff"/>"##,
    ),
    (
        "volume",
        "Audio",
        r##"<path d="M11 5L6 9H3v6h3l5 4z"/><path d="M15.5 8.5a5 5 0 0 1 0 7M18.5 5.5a9 9 0 0 1 0 13"/>"##,
    ),
    (
        "volume-up",
        "Audio",
        r##"<path d="M11 5L6 9H3v6h3l5 4z"/><path d="M15 12h7M18.5 8.5v7"/>"##,
    ),
    (
        "volume-down",
        "Audio",
        r##"<path d="M11 5L6 9H3v6h3l5 4z"/><path d="M15.5 12h6"/>"##,
    ),
    (
        "mute",
        "Audio",
        r##"<path d="M11 5L6 9H3v6h3l5 4z"/><path d="M15.5 9l6 6M21.5 9l-6 6"/>"##,
    ),
    (
        "bass",
        "Audio",
        r##"<path d="M2 12c2.5-8 7.5-8 10 0s7.5 8 10 0"/>"##,
    ),
    (
        "treble",
        "Audio",
        r##"<path d="M2 12c.8-3.5 2.5-3.5 3.3 0s2.5 3.5 3.3 0 2.5-3.5 3.4 0 2.5 3.5 3.3 0 2.5-3.5 3.3 0 2.5 3.5 3.4 0"/>"##,
    ),
    (
        "equalizer",
        "Audio",
        r##"<path d="M5 21v-7M5 10V3M12 21v-9M12 8V3M19 21v-5M19 12V3M2 14h6M9 8h6M16 16h6"/>"##,
    ),
    (
        "shuffle",
        "Audio",
        r##"<path d="M16 3h5v5M3 21L21 3M21 16v5h-5M15 15l6 6M3 3l6 6"/>"##,
    ),
    (
        "repeat",
        "Audio",
        r##"<path d="M17 2l4 4-4 4M3 11V9a3 3 0 0 1 3-3h15M7 22l-4-4 4-4M21 13v2a3 3 0 0 1-3 3H3"/>"##,
    ),
    (
        "music",
        "Audio",
        r##"<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>"##,
    ),
    (
        "playlist",
        "Audio",
        r##"<path d="M3 5h12M3 10h12M3 15h7"/><path d="M17 17.5v-10l4-1"/><circle cx="15" cy="17.5" r="2"/>"##,
    ),
    (
        "radio",
        "Audio",
        r##"<rect x="2" y="8" width="20" height="13" rx="2"/><path d="M6 8l12-5"/><circle cx="15" cy="14.5" r="3"/><path d="M5 12h4M5 16h4"/>"##,
    ),
    (
        "headphones",
        "Audio",
        r##"<path d="M4 16v-4a8 8 0 0 1 16 0v4"/><path d="M4 15h2.5a1 1 0 0 1 1 1v4a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1zM20 15h-2.5a1 1 0 0 0-1 1v4a1 1 0 0 0 1 1H19a1 1 0 0 0 1-1z"/>"##,
    ),
    (
        "speaker",
        "Audio",
        r##"<rect x="5" y="2" width="14" height="20" rx="2"/><circle cx="12" cy="14" r="4"/><circle cx="12" cy="6" r="1" fill="#fff" stroke="none"/>"##,
    ),
    (
        "mic",
        "Audio",
        r##"<rect x="9" y="2" width="6" height="12" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v4M8 22h8"/>"##,
    ),
    // Connections and tools
    ("bluetooth", "Tools", r##"<path d="M7 7l10 10-5 5V2l5 5L7 17"/>"##),
    (
        "wifi",
        "Tools",
        r##"<path d="M2 8.3a15 15 0 0 1 20 0M5 12.1a10 10 0 0 1 14 0M8.5 15.9a5 5 0 0 1 7 0"/><circle cx="12" cy="19.5" r="1.5" fill="#fff" stroke="none"/>"##,
    ),
    (
        "link",
        "Tools",
        r##"<path d="M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.7 1.7M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.7-1.7"/>"##,
    ),
    (
        "gear",
        "Tools",
        r##"<path d="M10.26 5.01L10.44 2.12H13.56L13.74 5.01L15.71 5.83L17.88 3.91L20.09 6.12L18.17 8.29L18.99 10.26L21.88 10.44V13.56L18.99 13.74L18.17 15.71L20.09 17.88L17.88 20.09L15.71 18.17L13.74 18.99L13.56 21.88H10.44L10.26 18.99L8.29 18.17L6.12 20.09L3.91 17.88L5.83 15.71L5.01 13.74L2.12 13.56V10.44L5.01 10.26L5.83 8.29L3.91 6.12L6.12 3.91L8.29 5.83Z"/><circle cx="12" cy="12" r="3"/>"##,
    ),
    (
        "timer",
        "Tools",
        r##"<circle cx="12" cy="13.5" r="7.5"/><path d="M12 10v3.5l2.5 2M10 3h4M12 3v2.5"/>"##,
    ),
    ("check", "Tools", r##"<path d="M3.5 12.5l5.5 5.5L20.5 6.5"/>"##),
];

/// `(name, group)` of every built-in icon.
pub fn names() -> impl Iterator<Item = (&'static str, &'static str)> {
    ICONS.iter().map(|(name, group, _)| (*name, *group))
}

/// A small PNG of icon `name` as a key shows it (tinted, on the key's dark fill), for the UI.
#[must_use]
pub fn preview(name: &str) -> Option<Vec<u8>> {
    exists(name).then_some(())?;
    let face = crate::modules::face(1, "", "", crate::modules::FaceState::FACE_STATE_IDLE);
    let img = crate::render::render_with(&face, Some(crate::render::KeyImage::Icon(name)));
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// Whether `name` is a built-in icon.
#[must_use]
pub fn exists(name: &str) -> bool {
    ICONS.iter().any(|(n, _, _)| *n == name)
}

/// The full SVG document of `name`, `side` pixels square.
#[must_use]
pub fn svg(name: &str, side: u32) -> Option<String> {
    let body = ICONS.iter().find(|(n, _, _)| *n == name)?.2;
    Some(format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="{side}" height="{side}" fill="none" stroke="#fff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"##
    ))
}

type Cache = BTreeMap<(String, u32), Arc<RgbaImage>>;
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);

/// `name` rasterised at `side`² (white on transparent), cached.
#[must_use]
pub fn get_at(name: &str, side: u32) -> Option<Arc<RgbaImage>> {
    let key = (name.to_owned(), side);
    if let Some(img) = CACHE.lock().expect("icon cache").get(&key) {
        return Some(Arc::clone(img));
    }
    let img = Arc::new(rasterise(&svg(name, side)?, side)?);
    CACHE.lock().expect("icon cache").insert(key, Arc::clone(&img));
    Some(img)
}

fn rasterise(svg: &str, side: u32) -> Option<RgbaImage> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(side, side)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    // tiny-skia stores premultiplied RGBA; the icons are white, so the alpha channel is the shape.
    let mut out = RgbaImage::new(side, side);
    for (px, src) in out.pixels_mut().zip(pixmap.pixels()) {
        *px = image::Rgba([255, 255, 255, src.alpha()]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_renders_something_inside_the_frame() {
        for (name, _) in names() {
            let img = get_at(name, RASTER).unwrap_or_else(|| panic!("{name} does not render"));
            let ink = img.pixels().filter(|p| p[3] > 128).count();
            let total = (RASTER * RASTER) as usize;
            assert!(ink > total / 100, "{name} is (almost) empty: {ink} px");
            assert!(ink < total / 2, "{name} is mostly filled: {ink} px");
        }
    }

    #[test]
    fn icons_are_centred() {
        // Play triangles sit right of the box centre on purpose, so their mass looks centred.
        for (name, _) in names().filter(|(n, _)| *n != "play") {
            let (b, _) = ink(name);
            let (dx, dy) = (f64::midpoint(b[0], b[2]) - 12.0, f64::midpoint(b[1], b[3]) - 12.0);
            assert!(
                dx.abs() <= 0.8 && dy.abs() <= 0.8,
                "{name} is off centre by {dx:+.2} {dy:+.2}"
            );
            assert!(
                b[0] >= 0.5 && b[1] >= 0.5 && b[2] <= 23.5 && b[3] <= 23.5,
                "{name} touches the edge: {b:?}"
            );
        }
    }

    #[test]
    fn names_are_unique_and_valid_image_names() {
        let mut seen = std::collections::BTreeSet::new();
        for (name, _) in names() {
            assert!(seen.insert(name), "{name} twice");
            assert!(
                crate::assets::valid_name(name),
                "{name} is not a valid image name"
            );
        }
        assert!(exists("volume-up") && !exists("nope"));
    }
}

/// Ink box and centre of mass of every icon in grid units (centre = 12), for the review:
/// `cargo test -p streamdockd icons::audit -- --ignored --nocapture`.
#[cfg(test)]
#[test]
#[ignore = "prints a report"]
fn audit() {
    for (name, _, _) in ICONS {
        let (bbox, centroid) = ink(name);
        println!(
            "{name:16} box {:5.1} {:5.1} {:5.1} {:5.1}  box-centre {:+5.2} {:+5.2}  mass {:+5.2} {:+5.2}",
            bbox[0],
            bbox[1],
            bbox[2],
            bbox[3],
            f64::midpoint(bbox[0], bbox[2]) - 12.0,
            f64::midpoint(bbox[1], bbox[3]) - 12.0,
            centroid.0 - 12.0,
            centroid.1 - 12.0
        );
    }
}

/// `([left, top, right, bottom], (x, y) centre of mass)` of the ink, in grid units.
#[cfg(test)]
fn ink(name: &str) -> ([f64; 4], (f64, f64)) {
    let img = get_at(name, RASTER).unwrap();
    let unit = f64::from(RASTER) / 24.0;
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    let (mut mass, mut mx, mut my) = (0.0, 0.0, 0.0);
    for (x, y, p) in img.enumerate_pixels() {
        if p[3] > 64 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
        let a = f64::from(p[3]) / 255.0;
        mass += a;
        mx += a * (f64::from(x) + 0.5);
        my += a * (f64::from(y) + 0.5);
    }
    let u = |v: u32| f64::from(v) / unit;
    ([u(x0), u(y0), u(x1), u(y1)], (mx / mass / unit, my / mass / unit))
}

/// A contact sheet of every icon as a key face, cycling through the three sizes: idle without
/// label (48 px), on with a label (36 px), unavailable with label and detail (30 px); at 2× for review: `ICON_SHEET=/tmp/icons.png cargo test -p streamdockd
/// icons::sheet -- --ignored`.
#[cfg(test)]
#[test]
#[ignore = "writes a review image"]
fn sheet() {
    use crate::modules::{FaceState, face};
    let path = std::env::var("ICON_SHEET").expect("set ICON_SHEET to the output path");
    let cols = 10u32;
    let cell = 140u32;
    let rows = u32::try_from(ICONS.len()).unwrap().div_ceil(cols);
    let mut out = image::RgbImage::from_pixel(cols * cell, rows * cell, image::Rgb([0x02, 0x07, 0x0a]));
    for (i, (name, _, _)) in ICONS.iter().enumerate() {
        let n = u32::try_from(i).unwrap();
        let (label, detail, state) = match i % 3 {
            0 => ("", "", FaceState::FACE_STATE_IDLE),
            1 => (*name, "", FaceState::FACE_STATE_ON),
            _ => (*name, "50 %", FaceState::FACE_STATE_UNAVAILABLE),
        };
        let key = crate::render::render_with(
            &face(1, label, detail, state),
            Some(crate::render::KeyImage::Icon(name)),
        );
        let big = image::imageops::resize(&key, 128, 128, image::imageops::FilterType::Nearest);
        image::imageops::replace(
            &mut out,
            &big,
            i64::from((n % cols) * cell + 6),
            i64::from((n / cols) * cell + 6),
        );
    }
    out.save(path).unwrap();
}

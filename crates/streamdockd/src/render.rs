//! Renders [`KeyFace`]s to 64×64 key images in the design system's look (H15-11 tokens: dark ink,
//! cyan/green accents, cut corners, Fira Sans Condensed; never red). Images are upright PNGs: the
//! device task scales, rotates and JPEG-encodes them for the dock (`Command::SetKeyImage`).

// Pixel math on a 64×64 image: every value fits f32 and u8/i32 exactly, so the cast lints are noise.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use std::sync::LazyLock;

use ab_glyph::{Font as _, FontRef, PxScale, ScaleFont as _, point};
use image::{ImageEncoder as _, Rgb, RgbImage, RgbaImage, imageops::FilterType};

use crate::modules::{FaceState, KeyFace, LevelStyle};

/// Edge length of a key image.
pub const SIZE: u32 = 64;
/// Inner padding left and right of the text.
const PAD: f32 = 4.0;
/// Size of the cut corners (top right, bottom left).
const CUT: u32 = 9;

/// Fira Sans Condensed `SemiBold` (SIL OFL 1.1, see `assets/fonts/OFL.txt`), the design system's font.
static FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../assets/fonts/FiraSansCondensed-SemiBold.ttf"))
        .expect("embedded font is valid")
});

// Design tokens (web/public/design/tokens.css), as RGB.
const INK_950: Rgb<u8> = Rgb([0x02, 0x07, 0x0a]);
const INK_900: Rgb<u8> = Rgb([0x05, 0x0f, 0x14]);
const INK_800: Rgb<u8> = Rgb([0x0c, 0x1f, 0x27]);
const INK_600: Rgb<u8> = Rgb([0x1d, 0x40, 0x4b]);
const INK_400: Rgb<u8> = Rgb([0x5f, 0x98, 0xa3]);
const CYAN_100: Rgb<u8> = Rgb([0xb9, 0xf6, 0xfb]);
const CYAN_400: Rgb<u8> = Rgb([0x2e, 0xe6, 0xf2]);
const CYAN_700: Rgb<u8> = Rgb([0x0d, 0x67, 0x73]);
const GREEN_300: Rgb<u8> = Rgb([0x8d, 0xff, 0xbf]);
const GREEN_400: Rgb<u8> = Rgb([0x3b, 0xff, 0x95]);
/// Colour temperature bar only (H15-97): warm white, neutral white, cold white.
const WARM: Rgb<u8> = Rgb([0xff, 0xa8, 0x3d]);
const NEUTRAL: Rgb<u8> = Rgb([0xff, 0xf1, 0xd8]);
const COLD: Rgb<u8> = Rgb([0x9f, 0xd8, 0xff]);

struct Palette {
    fill: Rgb<u8>,
    line: Rgb<u8>,
    label: Rgb<u8>,
    detail: Rgb<u8>,
}

fn palette(state: Option<FaceState>) -> Palette {
    match state {
        Some(FaceState::FACE_STATE_ON) => Palette {
            fill: CYAN_400,
            line: CYAN_100,
            label: INK_950,
            detail: INK_800,
        },
        Some(FaceState::FACE_STATE_ACTIVE) => Palette {
            fill: INK_800,
            line: GREEN_400,
            label: GREEN_300,
            detail: GREEN_400,
        },
        Some(FaceState::FACE_STATE_UNAVAILABLE) => Palette {
            fill: INK_950,
            line: INK_600,
            label: INK_400,
            detail: INK_600,
        },
        _ => Palette {
            fill: INK_900,
            line: CYAN_700,
            label: CYAN_100,
            detail: INK_400,
        },
    }
}

/// Renders a face to an upright 64×64 image.
#[cfg(test)]
#[must_use]
pub fn render(face: &KeyFace) -> RgbImage {
    render_with(face, None)
}

/// A key's image, looked up by the caller from `KeyFace.image`.
#[derive(Debug, Clone, Copy)]
pub enum KeyImage<'a> {
    /// An uploaded picture (H15-28): fills the key.
    Picture(&'a RgbaImage),
    /// A built-in icon by name (H15-48), tinted with the state's colour.
    Icon(&'a str),
}

/// Renders a face, with its image if it has one.
#[must_use]
pub fn render_with(face: &KeyFace, image: Option<KeyImage<'_>>) -> RgbImage {
    let strip = face
        .level
        .filter(|_| face.level_style.as_known() == Some(LevelStyle::LEVEL_STYLE_STRIP));
    if let Some(level) = face.level
        && strip.is_none()
    {
        return render_level(face, level);
    }
    let mut img = render_face(face, image);
    if let Some(level) = strip {
        draw_strip(&mut img, level);
    }
    img
}

/// A face without a level bar: its picture, its icon, or label and detail.
fn render_face(face: &KeyFace, image: Option<KeyImage<'_>>) -> RgbImage {
    match image {
        Some(KeyImage::Picture(picture)) => return render_image(face, picture),
        Some(KeyImage::Icon(name)) => {
            if let Some(img) = render_icon(face, name) {
                return img;
            }
        }
        None => {}
    }
    let p = palette(face.state.as_known());
    let mut img = RgbImage::from_pixel(SIZE, SIZE, p.line);
    // A 2 px frame, then the fill.
    for y in 2..SIZE - 2 {
        for x in 2..SIZE - 2 {
            img.put_pixel(x, y, p.fill);
        }
    }
    cut_corners(&mut img, p.line);

    let detail = face.detail.trim();
    let label_bottom = if detail.is_empty() {
        SIZE as f32 - PAD
    } else {
        SIZE as f32 - 18.0
    };
    draw_label(&mut img, face.label.trim(), label_bottom, p.label);
    if !detail.is_empty() {
        let line = fit_line(detail, 11.0, SIZE as f32 - 2.0 * PAD);
        let width = text_width(&line, 11.0);
        draw_text(
            &mut img,
            &line,
            11.0,
            (SIZE as f32 - width) / 2.0,
            SIZE as f32 - 7.0,
            p.detail,
        );
    }
    img
}

/// Renders a face to PNG bytes.
#[cfg(test)]
#[must_use]
pub fn render_png(face: &KeyFace) -> Vec<u8> {
    render_png_with(face, None)
}

/// [`render_with`], as PNG bytes.
#[must_use]
pub fn render_png_with(face: &KeyFace, image: Option<KeyImage<'_>>) -> Vec<u8> {
    png(&render_with(face, image))
}

/// An icon face (H15-48): the key's normal frame and fill for its state, the icon tinted in the
/// label colour, and the label (one line) and detail below it. Without text the icon is larger.
fn render_icon(face: &KeyFace, name: &str) -> Option<RgbImage> {
    let label = face.label.trim();
    let detail = face.detail.trim();
    // Rasterised at the drawn size; 48 and 36 px give 2 and 1.5 px per grid unit (24 units).
    let (side, top) = match (label.is_empty(), detail.is_empty()) {
        (true, true) => (48, 8),
        (false, true) | (true, false) => (36, 5),
        (false, false) => (30, 4),
    };
    let icon = crate::icons::get_at(name, side)?;
    let p = palette(face.state.as_known());
    let mut img = RgbImage::from_pixel(SIZE, SIZE, p.line);
    for y in 2..SIZE - 2 {
        for x in 2..SIZE - 2 {
            img.put_pixel(x, y, p.fill);
        }
    }
    cut_corners(&mut img, p.line);

    let left = (SIZE - side) / 2;
    for (x, y, px) in icon.enumerate_pixels() {
        let alpha = f32::from(px[3]) / 255.0;
        if alpha > 0.0 {
            let (x, y) = (x + left, y + top);
            let bg = *img.get_pixel(x, y);
            img.put_pixel(x, y, blend(bg, p.label, alpha));
        }
    }

    let max_width = SIZE as f32 - 2.0 * PAD;
    let mut baseline = SIZE as f32 - 6.0;
    if !detail.is_empty() {
        let line = fit_line(detail, 10.0, max_width);
        draw_text(
            &mut img,
            &line,
            10.0,
            (SIZE as f32 - text_width(&line, 10.0)) / 2.0,
            baseline,
            p.detail,
        );
        baseline -= 11.0;
    }
    if !label.is_empty() {
        let line = fit_line(label, 11.0, max_width);
        draw_text(
            &mut img,
            &line,
            11.0,
            (SIZE as f32 - text_width(&line, 11.0)) / 2.0,
            baseline,
            p.label,
        );
    }
    Some(img)
}

/// Frame width of an image face, so its state stays visible around the picture.
const IMAGE_FRAME: u32 = 3;

/// An image face (H15-28): the picture covers the key inside a frame in the state's colour (green
/// when on), and the label and detail sit on a dark band at the bottom.
fn render_image(face: &KeyFace, image: &RgbaImage) -> RgbImage {
    let state = face.state.as_known();
    let (frame, dim) = match state {
        Some(FaceState::FACE_STATE_ON) => (GREEN_400, false),
        Some(FaceState::FACE_STATE_ACTIVE) => (CYAN_400, false),
        Some(FaceState::FACE_STATE_UNAVAILABLE) => (INK_600, true),
        _ => (CYAN_700, false),
    };
    let mut img = RgbImage::from_pixel(SIZE, SIZE, frame);
    let inner = SIZE - 2 * IMAGE_FRAME;
    // Cover: scale so the shorter side fills, then crop the middle.
    let (w, h) = (image.width().max(1), image.height().max(1));
    let scale = (inner as f32 / w as f32).max(inner as f32 / h as f32);
    let (sw, sh) = (
        ((w as f32 * scale).round() as u32).max(inner),
        ((h as f32 * scale).round() as u32).max(inner),
    );
    let scaled = image::imageops::resize(image, sw, sh, FilterType::Triangle);
    let (ox, oy) = ((sw - inner) / 2, (sh - inner) / 2);
    for y in 0..inner {
        for x in 0..inner {
            let p = scaled.get_pixel(x + ox, y + oy).0;
            let alpha = f32::from(p[3]) / 255.0;
            let mut px = blend(INK_950, Rgb([p[0], p[1], p[2]]), alpha);
            if dim {
                px = blend(INK_950, px, 0.4);
            }
            img.put_pixel(x + IMAGE_FRAME, y + IMAGE_FRAME, px);
        }
    }

    let label = face.label.trim();
    let detail = face.detail.trim();
    let lines = u32::from(!label.is_empty()) + u32::from(!detail.is_empty());
    if lines > 0 {
        let band_top = SIZE - IMAGE_FRAME - 4 - lines * 13;
        for y in band_top..SIZE - IMAGE_FRAME {
            for x in IMAGE_FRAME..SIZE - IMAGE_FRAME {
                let bg = *img.get_pixel(x, y);
                img.put_pixel(x, y, blend(bg, INK_950, 0.75));
            }
        }
        let max_width = SIZE as f32 - 2.0 * PAD;
        let mut baseline = SIZE as f32 - IMAGE_FRAME as f32 - 4.0;
        if !detail.is_empty() {
            let line = fit_line(detail, 11.0, max_width);
            let x = (SIZE as f32 - text_width(&line, 11.0)) / 2.0;
            draw_text(&mut img, &line, 11.0, x, baseline, INK_400);
            baseline -= 13.0;
        }
        if !label.is_empty() {
            let line = fit_line(label, 12.0, max_width);
            let x = (SIZE as f32 - text_width(&line, 12.0)) / 2.0;
            draw_text(&mut img, &line, 12.0, x, baseline, CYAN_100);
        }
    }
    cut_corners(&mut img, frame);
    img
}

/// The chamfer of the design system: top-right and bottom-left corners cut, with a 2 px edge.
fn cut_corners(img: &mut RgbImage, line: Rgb<u8>) {
    for y in 0..CUT {
        for x in 0..CUT {
            let d = x + y; // distance along the diagonal
            let (tr, bl) = ((SIZE - 1 - x, y), (x, SIZE - 1 - y));
            if d < CUT - 2 {
                img.put_pixel(tr.0, tr.1, INK_950);
                img.put_pixel(bl.0, bl.1, INK_950);
            } else if d < CUT {
                img.put_pixel(tr.0, tr.1, line);
                img.put_pixel(bl.0, bl.1, line);
            }
        }
    }
}

/// Label sizes tried, largest first.
const LABEL_SIZES: [f32; 5] = [16.0, 14.0, 12.0, 11.0, 10.0];

/// The label: up to two centered lines at the largest size where every word fits whole;
/// truncated with "…" only if even the smallest size doesn't fit.
fn draw_label(img: &mut RgbImage, label: &str, bottom: f32, color: Rgb<u8>) {
    let max_width = SIZE as f32 - 2.0 * PAD;
    let words_fit = |size: f32| label.split_whitespace().all(|w| text_width(w, size) <= max_width);
    let (size, lines) = LABEL_SIZES
        .iter()
        .filter(|&&size| words_fit(size))
        .map(|&size| (size, wrap(label, size, max_width)))
        .find(|(_, lines)| lines.len() <= 2)
        .unwrap_or_else(|| {
            let mut lines = wrap(label, 12.0, max_width);
            lines.truncate(2);
            // One word too long for any size (e.g. "brightness-down") wraps to one line.
            if let Some(last) = lines.last_mut() {
                *last = fit_line(&format!("{last}…"), 12.0, max_width);
            }
            (12.0, lines)
        });
    let line_height = size * 1.05;
    let block = line_height * lines.len() as f32;
    let top = 3.0 + ((bottom - 3.0) - block) / 2.0;
    for (i, line) in lines.iter().enumerate() {
        let width = text_width(line, size);
        let baseline = top + line_height * (i as f32 + 1.0) - size * 0.22;
        draw_text(img, line, size, (SIZE as f32 - width) / 2.0, baseline, color);
    }
}

/// Greedy word wrap; words wider than a line are cut with an ellipsis.
fn wrap(text: &str, size: f32, max_width: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let word = fit_line(word, size, max_width);
        match lines.last_mut() {
            Some(line) if text_width(&format!("{line} {word}"), size) <= max_width => {
                line.push(' ');
                line.push_str(&word);
            }
            _ => lines.push(word),
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// `text`, cut and ending in "…" if it is wider than `max_width`.
fn fit_line(text: &str, size: f32, max_width: f32) -> String {
    if text_width(text, size) <= max_width {
        return text.to_owned();
    }
    let mut out: String = text.to_owned();
    while !out.is_empty() && text_width(&format!("{out}…"), size) > max_width {
        out.pop();
    }
    format!("{}…", out.trim_end())
}

fn text_width(text: &str, size: f32) -> f32 {
    let font = FONT.as_scaled(PxScale::from(size));
    let mut width = 0.0;
    let mut prev = None;
    for c in text.chars() {
        let id = font.glyph_id(c);
        if let Some(p) = prev {
            width += font.kern(p, id);
        }
        width += font.h_advance(id);
        prev = Some(id);
    }
    width
}

fn draw_text(img: &mut RgbImage, text: &str, size: f32, x: f32, baseline: f32, color: Rgb<u8>) {
    let scale = PxScale::from(size);
    let font = FONT.as_scaled(scale);
    let mut caret = x;
    let mut prev = None;
    for c in text.chars() {
        let id = font.glyph_id(c);
        if let Some(p) = prev {
            caret += font.kern(p, id);
        }
        let glyph = id.with_scale_and_position(scale, point(caret, baseline));
        caret += font.h_advance(id);
        prev = Some(id);
        let Some(outline) = FONT.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, coverage| {
            let px = bounds.min.x as i32 + gx as i32;
            let py = bounds.min.y as i32 + gy as i32;
            if let (Ok(px), Ok(py)) = (u32::try_from(px), u32::try_from(py))
                && px < SIZE
                && py < SIZE
            {
                let bg = *img.get_pixel(px, py);
                img.put_pixel(px, py, blend(bg, color, coverage));
            }
        });
    }
}

/// Text size of the now-playing band (H15-88).
pub const BAND_TEXT: f32 = 22.0;
/// Height of the band's progress bar.
const BAND_BAR: u32 = 3;

/// Width of the band's line of text at [`BAND_TEXT`], in pixels.
#[must_use]
pub fn band_text_width(text: &str) -> f32 {
    text_width(text, BAND_TEXT)
}

/// The text area of a band `count` keys wide.
#[must_use]
pub fn band_inner_width(count: u32) -> f32 {
    (count * SIZE) as f32 - 2.0 * PAD
}

/// `text` cut with "…" to fit a band `count` keys wide (the non-scrolling mode).
#[must_use]
pub fn band_fit(text: &str, count: u32) -> String {
    fit_line(text, BAND_TEXT, band_inner_width(count))
}

/// Decoded covers for the band, by key (URL and band width): the last few only.
static COVERS: LazyLock<std::sync::Mutex<Vec<(String, RgbImage)>>> = LazyLock::new(Default::default);
const COVERS_KEPT: usize = 4;

/// The cache key of a cover for a band `count` keys wide.
#[must_use]
pub fn cover_key(url: &str, count: u32) -> String {
    format!("{count}#{url}")
}

/// Whether a cover is cached under `key`.
#[must_use]
pub fn has_cover(key: &str) -> bool {
    COVERS
        .lock()
        .expect("cover cache poisoned")
        .iter()
        .any(|(k, _)| k == key)
}

/// Decodes `bytes`, crops it to the band's shape (`count` keys wide, filled), darkens it so text
/// stays readable, and caches it under `key`.
pub fn store_cover(key: &str, count: u32, bytes: &[u8]) -> anyhow::Result<()> {
    let image = image::load_from_memory(bytes)?.to_rgb8();
    let (bw, bh) = (count.max(1) * SIZE, SIZE);
    let (w, h) = (image.width().max(1), image.height().max(1));
    let scale = (bw as f32 / w as f32).max(bh as f32 / h as f32);
    let (sw, sh) = (
        ((w as f32 * scale).round() as u32).max(bw),
        ((h as f32 * scale).round() as u32).max(bh),
    );
    let scaled = image::imageops::resize(&image, sw, sh, FilterType::Triangle);
    let cropped = image::imageops::crop_imm(&scaled, (sw - bw) / 2, (sh - bh) / 2, bw, bh).to_image();
    let dark = RgbImage::from_fn(bw, bh, |x, y| blend(INK_950, *cropped.get_pixel(x, y), 0.35));
    let mut covers = COVERS.lock().expect("cover cache poisoned");
    covers.retain(|(k, _)| k != key);
    covers.push((key.to_owned(), dark));
    let excess = covers.len().saturating_sub(COVERS_KEPT);
    covers.drain(..excess);
    Ok(())
}

/// Size of the percentage on a level key (H15-95).
const LEVEL_TEXT: f32 = 22.0;

/// A level key (H15-95): the key fills from the left like a bar up to `level` %, the number large on
/// it, the label small at the top. Readable from across a room.
#[must_use]
fn render_level(face: &KeyFace, level: u32) -> RgbImage {
    let level = level.min(100);
    let temperature = face.level_style.as_known() == Some(LevelStyle::LEVEL_STYLE_TEMPERATURE);
    let mut img = RgbImage::from_pixel(SIZE, SIZE, INK_900);
    // The bar: below the label, across the whole width, with a 2 px margin.
    let top = 18;
    let filled = 2 + ((SIZE - 4) * level + 50) / 100;
    for y in top..SIZE - 2 {
        for x in 2..SIZE - 2 {
            let color = if temperature {
                // Warm to cold over the width; the part past the level dimmed (H15-97).
                let hue = temperature_color((x - 2) * 100 / (SIZE - 5));
                if x < filled {
                    hue
                } else {
                    blend(INK_900, hue, 0.25)
                }
            } else if x < filled {
                CYAN_700
            } else {
                INK_800
            };
            img.put_pixel(x, y, color);
        }
    }
    // A bright edge where the fill ends, so small changes are visible.
    if (3..SIZE - 2).contains(&filled) {
        let edge = if temperature { CYAN_100 } else { GREEN_400 };
        for y in top..SIZE - 2 {
            img.put_pixel(filled - 1, y, edge);
        }
    }
    let label = fit_line(face.label.trim(), 11.0, SIZE as f32 - 2.0 * PAD);
    let width = text_width(&label, 11.0);
    draw_text(&mut img, &label, 11.0, (SIZE as f32 - width) / 2.0, 13.0, INK_400);
    let number = match face.detail.trim() {
        "" => format!("{level}%"),
        detail => detail.to_owned(),
    };
    let size = fit_size(&number, LEVEL_TEXT, SIZE as f32 - 3.0 * PAD);
    let x = (SIZE as f32 - text_width(&number, size)) / 2.0;
    let baseline = top as f32 + (SIZE - 2 - top) as f32 / 2.0 + size * 0.35;
    if temperature {
        // A dark outline keeps the number readable on the bright warm and cold ends.
        for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
            draw_text(&mut img, &number, size, x + dx, baseline + dy, INK_950);
        }
    }
    draw_text(&mut img, &number, size, x, baseline, CYAN_100);
    img
}

/// The largest text size up to `size` at which `text` fits `width` ("3200 K" on a key).
fn fit_size(text: &str, size: f32, width: f32) -> f32 {
    let natural = text_width(text, size);
    if natural <= width {
        size
    } else {
        (size * width / natural).max(11.0)
    }
}

/// Warm (amber, 0) to cold (cyan-white, 100): the one place the keys show a colour that is not
/// blue or green, because it previews the light itself (H15-97, docs/design-system.md).
fn temperature_color(at: u32) -> Rgb<u8> {
    let t = at.min(100) as f32 / 100.0;
    if t < 0.5 {
        blend(WARM, NEUTRAL, t * 2.0)
    } else {
        blend(NEUTRAL, COLD, t * 2.0 - 1.0)
    }
}

/// A light's brightness on its own key (`LEVEL_STYLE_STRIP`, H15-97): a 3 px strip over the
/// bottom edge, filled from the left.
fn draw_strip(img: &mut RgbImage, level: u32) {
    let filled = 2 + ((SIZE - 4) * level.min(100) + 50) / 100;
    for y in SIZE - 4..SIZE - 1 {
        for x in 2..SIZE - 2 {
            img.put_pixel(x, y, if x < filled { GREEN_400 } else { INK_950 });
        }
    }
}

/// One key of the now-playing band (H15-88): its part of the cover (or the dark fill), of the
/// line of text at the band's scroll offset, and of the progress bar.
#[must_use]
pub fn render_band(slice: &streamdock_proto::streamdock::v1::BandSlice) -> RgbImage {
    let left = slice.index * SIZE;
    let mut img = RgbImage::from_pixel(SIZE, SIZE, INK_900);
    if !slice.cover.is_empty() {
        let covers = COVERS.lock().expect("cover cache poisoned");
        if let Some((_, cover)) = covers.iter().find(|(k, _)| *k == slice.cover)
            && cover.width() >= left + SIZE
        {
            for y in 0..SIZE {
                for x in 0..SIZE {
                    img.put_pixel(x, y, *cover.get_pixel(left + x, y));
                }
            }
        }
    }
    // A thin accent line on top, like the frame of the other faces.
    let accent = if slice.dim { INK_600 } else { CYAN_700 };
    for y in 0..2 {
        for x in 0..SIZE {
            img.put_pixel(x, y, accent);
        }
    }
    let color = if slice.dim { INK_400 } else { CYAN_100 };
    let x = PAD - slice.offset as f32 - left as f32;
    draw_text_clipped(
        &mut img,
        &slice.text,
        BAND_TEXT,
        x,
        SIZE as f32 / 2.0 + BAND_TEXT * 0.35,
        color,
    );
    if slice.show_progress {
        let total = (slice.count.max(1) * SIZE) as f32;
        let filled = (total * slice.progress.min(1000) as f32 / 1000.0).round() as u32;
        let (bar, rest) = if slice.dim {
            (INK_400, INK_800)
        } else {
            (GREEN_400, INK_800)
        };
        for y in SIZE - BAND_BAR..SIZE {
            for x in 0..SIZE {
                img.put_pixel(x, y, if left + x < filled { bar } else { rest });
            }
        }
    }
    img
}

/// [`render_band`], as PNG bytes.
#[must_use]
pub fn render_band_png(slice: &streamdock_proto::streamdock::v1::BandSlice) -> Vec<u8> {
    png(&render_band(slice))
}

fn png(img: &RgbImage) -> Vec<u8> {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(img.as_raw(), SIZE, SIZE, image::ExtendedColorType::Rgb8)
        .expect("encoding a PNG into memory cannot fail");
    png
}

/// [`draw_text`] for long lines: glyphs outside the key are skipped, not rasterised (the band
/// draws a whole line for every 64 px slice and frame).
fn draw_text_clipped(img: &mut RgbImage, text: &str, size: f32, x: f32, baseline: f32, color: Rgb<u8>) {
    let scale = PxScale::from(size);
    let font = FONT.as_scaled(scale);
    let mut caret = x;
    let mut prev = None;
    for c in text.chars() {
        let id = font.glyph_id(c);
        if let Some(p) = prev {
            caret += font.kern(p, id);
        }
        let advance = font.h_advance(id);
        prev = Some(id);
        if caret > SIZE as f32 {
            break;
        }
        if caret + advance + size < 0.0 {
            caret += advance;
            continue;
        }
        let glyph = id.with_scale_and_position(scale, point(caret, baseline));
        caret += advance;
        let Some(outline) = FONT.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, coverage| {
            let px = bounds.min.x as i32 + gx as i32;
            let py = bounds.min.y as i32 + gy as i32;
            if let (Ok(px), Ok(py)) = (u32::try_from(px), u32::try_from(py))
                && px < SIZE
                && py < SIZE
            {
                let bg = *img.get_pixel(px, py);
                img.put_pixel(px, py, blend(bg, color, coverage));
            }
        });
    }
}

fn blend(bg: Rgb<u8>, fg: Rgb<u8>, alpha: f32) -> Rgb<u8> {
    let a = alpha.clamp(0.0, 1.0);
    let mix = |b: u8, f: u8| (f32::from(b) * (1.0 - a) + f32::from(f) * a).round() as u8;
    Rgb([mix(bg[0], fg[0]), mix(bg[1], fg[1]), mix(bg[2], fg[2])])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::face;

    /// CPU of one band frame (H15-88): for each of 3 keys, render + PNG (daemon), PNG decode +
    /// rotate + JPEG (device task), as for the dock; USB transfer not included.
    #[test]
    #[ignore = "CPU: cargo test -p streamdockd band_frame_cpu -- --ignored --nocapture (also the armv7 test binary)"]
    fn band_frame_cpu() {
        let text = "Kapitel 20 - Folge 10: Die Nebelinsel · Die Nebelbande · Folge 10: Die Nebelinsel";
        let frames = 200;
        let t = std::time::Instant::now();
        let mut bytes = 0;
        for f in 0..frames {
            for i in 0..3 {
                let slice = streamdock_proto::streamdock::v1::BandSlice {
                    text: text.into(),
                    offset: f * 7,
                    index: i,
                    count: 3,
                    progress: 400,
                    show_progress: true,
                    ..Default::default()
                };
                let png = render_band_png(&slice);
                let decoded = image::load_from_memory(&png).unwrap();
                bytes += streamdock_hid::encode_key_image(&decoded).unwrap().len();
            }
        }
        let per_frame = t.elapsed().as_secs_f64() / f64::from(frames);
        println!(
            "band frame (3 keys): {:.2} ms; at 6 fps {:.1} % of one core; {} bytes JPEG per frame",
            per_frame * 1000.0,
            per_frame * 6.0 * 100.0,
            bytes / frames as usize
        );
    }

    /// Writes a few band frames side by side to `$BAND_PNG` for a look (H15-88).
    #[test]
    #[ignore = "visual check: BAND_PNG=/tmp/band.png cargo test -p streamdockd band_preview -- --ignored"]
    fn band_preview() {
        let path = std::env::var("BAND_PNG").expect("BAND_PNG");
        let text = "Kapitel 20 - Folge 10: Die Nebelinsel · Die Nebelbande";
        let rows: [(i32, u32, bool); 3] = [(0, 250, false), (120, 600, false), (0, 300, true)];
        let mut out = RgbImage::from_pixel(
            3 * SIZE + 2 * 8,
            rows.len() as u32 * (SIZE + 8),
            Rgb([40, 40, 40]),
        );
        for (r, (offset, progress, dim)) in rows.iter().enumerate() {
            for i in 0..3 {
                let slice = streamdock_proto::streamdock::v1::BandSlice {
                    text: text.into(),
                    offset: *offset,
                    index: i,
                    count: 3,
                    progress: *progress,
                    show_progress: true,
                    dim: *dim,
                    ..Default::default()
                };
                image::imageops::overlay(
                    &mut out,
                    &render_band(&slice),
                    i64::from(i * (SIZE + 8)),
                    r as i64 * i64::from(SIZE + 8),
                );
            }
        }
        out.save(path).unwrap();
    }

    #[test]
    fn a_level_key_fills_like_a_bar() {
        let face = |level| KeyFace {
            key: 3,
            label: "Volume".into(),
            level: Some(level),
            ..Default::default()
        };
        let half = render(&face(50));
        // Left of the middle is filled, right of it empty (sampled below the number's row).
        assert_eq!(*half.get_pixel(10, SIZE - 4), CYAN_700);
        assert_eq!(*half.get_pixel(SIZE - 10, SIZE - 4), INK_800);
        assert_eq!(*render(&face(0)).get_pixel(10, SIZE - 4), INK_800, "0 %: empty");
        assert_ne!(render(&face(40)), render(&face(41)), "every percent shows");
        assert_eq!(render(&face(100)), render(&face(250)), "clamped to 100 %");
    }

    #[test]
    fn level_styles_temperature_and_strip() {
        let temp = |level, detail: &str| {
            render(&KeyFace {
                key: 3,
                label: "Farbe".into(),
                detail: detail.into(),
                level: Some(level),
                level_style: LevelStyle::LEVEL_STYLE_TEMPERATURE.into(),
                ..Default::default()
            })
        };
        let full = temp(100, "6500 K");
        // Warm on the left, cold on the right (sampled below the number).
        assert_eq!(*full.get_pixel(2, SIZE - 4), WARM);
        assert!(
            full.get_pixel(SIZE - 4, SIZE - 4)[2] > full.get_pixel(SIZE - 4, SIZE - 4)[0],
            "cold = blue"
        );
        assert_ne!(
            temp(30, "3000 K"),
            temp(30, "3100 K"),
            "the detail replaces the percentage"
        );
        let dim = *temp(0, "2200 K").get_pixel(SIZE - 4, SIZE - 4);
        assert!(dim[2] < 100, "past the level: dimmed");

        let light = |level| KeyFace {
            key: 1,
            label: "Decke".into(),
            detail: "60 %".into(),
            state: FaceState::FACE_STATE_ON.into(),
            level,
            level_style: LevelStyle::LEVEL_STYLE_STRIP.into(),
            ..Default::default()
        };
        let plain = render(&light(None));
        let strip = render(&light(Some(50)));
        assert_eq!(*strip.get_pixel(10, SIZE - 3), GREEN_400);
        assert_eq!(*strip.get_pixel(SIZE - 10, SIZE - 3), INK_950);
        assert_eq!(
            strip.get_pixel(20, 20),
            plain.get_pixel(20, 20),
            "an ordinary face above the strip"
        );
    }

    #[test]
    fn band_slices_scroll_and_show_progress() {
        let slice = |index, offset, progress| streamdock_proto::streamdock::v1::BandSlice {
            text: "Kapitel 20 - Folge 10: Die Nebelinsel · Die Nebelbande".into(),
            offset,
            index,
            count: 3,
            progress,
            show_progress: true,
            ..Default::default()
        };
        let a = render_band(&slice(0, 0, 500));
        assert_ne!(a, render_band(&slice(0, 30, 500)), "scrolling changes the image");
        assert_ne!(a, render_band(&slice(1, 0, 500)), "each key shows its part");
        // Half the progress: the first key's bar is full, the third key's is empty.
        assert_eq!(*a.get_pixel(32, SIZE - 1), GREEN_400);
        assert_eq!(*render_band(&slice(2, 0, 500)).get_pixel(32, SIZE - 1), INK_800);
        // Far past the text: nothing but the fill (glyphs outside are skipped).
        let empty = render_band(&slice(0, 2000, 0));
        assert!((4..SIZE - 4).all(|y| *empty.get_pixel(30, y) == INK_900));
        // A cached cover shows behind the text.
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[200u8; 16 * 16 * 3], 16, 16, image::ExtendedColorType::Rgb8)
            .unwrap();
        let key = cover_key("http://lms/cover.png", 3);
        store_cover(&key, 3, &png).unwrap();
        assert!(has_cover(&key));
        let covered = render_band(&streamdock_proto::streamdock::v1::BandSlice {
            cover: key,
            ..slice(1, 2000, 0)
        });
        assert_ne!(*covered.get_pixel(30, 30), INK_900);
    }

    fn has_color(img: &RgbImage, color: Rgb<u8>) -> bool {
        img.pixels().any(|p| *p == color)
    }

    #[test]
    fn renders_states_with_their_fill() {
        let on = render(&face(1, "Desk", "60 %", FaceState::FACE_STATE_ON));
        assert_eq!(*on.get_pixel(32, 4), CYAN_400, "on = cyan fill");
        let idle = render(&face(1, "Desk", "", FaceState::FACE_STATE_IDLE));
        assert_eq!(*idle.get_pixel(32, 4), INK_900);
        let active = render(&face(1, "Focus", "playing", FaceState::FACE_STATE_ACTIVE));
        assert!(has_color(&active, GREEN_400), "active = green frame");
    }

    #[test]
    fn icon_faces_are_tinted_by_state() {
        let icon = "light-on";
        let idle = render_with(
            &face(1, "Spots", "", FaceState::FACE_STATE_IDLE),
            Some(KeyImage::Icon(icon)),
        );
        let on = render_with(
            &face(1, "Spots", "", FaceState::FACE_STATE_ON),
            Some(KeyImage::Icon(icon)),
        );
        assert!(has_color(&idle, CYAN_100), "idle icon in the label colour");
        assert_eq!(*on.get_pixel(32, 4), CYAN_400, "on = the usual cyan fill");
        assert!(has_color(&on, INK_950), "on: icon in the dark label colour");
        assert!(
            idle.pixels().all(|p| !(p[0] > 150 && p[1] < 100 && p[2] < 100)),
            "never red"
        );
    }

    #[test]
    fn a_long_single_word_label_does_not_panic() {
        let img = render(&face(
            1,
            "brightness-downwards-extra",
            "",
            FaceState::FACE_STATE_IDLE,
        ));
        assert!(img.pixels().any(|p| *p == CYAN_100 || p[2] > 200));
    }

    #[test]
    fn image_faces_keep_the_state_in_the_frame() {
        let picture = RgbaImage::from_pixel(100, 50, image::Rgba([200, 120, 20, 255]));
        let on = render_with(
            &face(1, "Lamp", "", FaceState::FACE_STATE_ON),
            Some(KeyImage::Picture(&picture)),
        );
        assert_eq!(*on.get_pixel(32, 1), GREEN_400, "on = green frame");
        assert_eq!(
            *on.get_pixel(32, 20),
            Rgb([200, 120, 20]),
            "the picture fills the middle"
        );
        let idle = render_with(
            &face(1, "", "", FaceState::FACE_STATE_IDLE),
            Some(KeyImage::Picture(&picture)),
        );
        assert_eq!(*idle.get_pixel(32, 1), CYAN_700);
        assert_eq!(*idle.get_pixel(32, 55), Rgb([200, 120, 20]), "no label, no band");
        let labelled = render_with(
            &face(1, "Lamp", "", FaceState::FACE_STATE_IDLE),
            Some(KeyImage::Picture(&picture)),
        );
        let band = labelled.get_pixel(10, 55);
        assert!(band[0] < 100, "a dark band behind the label: {band:?}");
        let unavailable = render_with(
            &face(1, "", "", FaceState::FACE_STATE_UNAVAILABLE),
            Some(KeyImage::Picture(&picture)),
        );
        assert!(
            unavailable.get_pixel(32, 20)[0] < 200,
            "unavailable dims the picture"
        );
    }

    #[test]
    fn draws_text_and_never_red() {
        let img = render(&face(
            2,
            "Very long playlist name that wraps",
            "detail",
            FaceState::FACE_STATE_IDLE,
        ));
        // Some pixels carry the label color (text was drawn).
        assert!(img.pixels().filter(|p| **p == CYAN_100 || p[2] > 200).count() > 20);
        // No red-dominant pixel anywhere.
        assert!(img.pixels().all(|p| !(p[0] > 150 && p[1] < 100 && p[2] < 100)));
    }

    #[test]
    fn wraps_and_truncates() {
        let lines = wrap("Living room ceiling lamp left", 16.0, 56.0);
        assert!(lines.len() >= 2);
        assert!(lines.iter().all(|l| text_width(l, 16.0) <= 56.0), "{lines:?}");
        assert!(fit_line("Supercalifragilistic", 16.0, 56.0).ends_with('…'));
    }

    /// Writes example faces to `$RENDER_OUT` (default: the system temp dir) for a visual check:
    /// `RENDER_OUT=/tmp/faces cargo test -p streamdockd render_gallery -- --ignored`.
    #[test]
    #[ignore = "writes files; run on demand"]
    fn render_gallery() {
        let dir = std::env::var_os("RENDER_OUT").map_or_else(std::env::temp_dir, Into::into);
        std::fs::create_dir_all(&dir).unwrap();
        let faces = [
            face(1, "Desk lamp", "60 %", FaceState::FACE_STATE_ON),
            face(2, "Deckenspots", "1 %", FaceState::FACE_STATE_ON),
            face(3, "evening jazz", "playing", FaceState::FACE_STATE_ACTIVE),
            face(
                4,
                "Window strip",
                "unavailable",
                FaceState::FACE_STATE_UNAVAILABLE,
            ),
            face(
                5,
                "A very long playlist name indeed",
                "",
                FaceState::FACE_STATE_IDLE,
            ),
            face(6, "Lights", "mode", FaceState::FACE_STATE_ACTIVE),
        ];
        for f in &faces {
            let path = dir.join(format!("face-{}.png", f.key));
            std::fs::write(&path, render_png(f)).unwrap();
        }
        // One 3x2 sheet at 2x for easy viewing.
        let mut sheet = RgbImage::from_pixel(3 * 2 * SIZE + 4 * 8, 2 * 2 * SIZE + 3 * 8, INK_950);
        for (i, f) in faces.iter().enumerate() {
            let tile = image::imageops::resize(
                &render(f),
                2 * SIZE,
                2 * SIZE,
                image::imageops::FilterType::Nearest,
            );
            let (col, row) = (u32::try_from(i % 3).unwrap(), u32::try_from(i / 3).unwrap());
            image::imageops::overlay(
                &mut sheet,
                &tile,
                i64::from(8 + col * (2 * SIZE + 8)),
                i64::from(8 + row * (2 * SIZE + 8)),
            );
        }
        sheet.save(dir.join("faces-sheet.png")).unwrap();
    }

    #[test]
    fn long_words_shrink_before_they_are_cut() {
        // "Deckenspots" doesn't fit at 16 px but does at a smaller size: no ellipsis.
        let max = SIZE as f32 - 2.0 * PAD;
        assert!(text_width("Deckenspots", 16.0) > max);
        let size = LABEL_SIZES
            .iter()
            .copied()
            .find(|&s| text_width("Deckenspots", s) <= max)
            .unwrap();
        assert!(size < 16.0);
        assert_eq!(wrap("Deckenspots", size, max), ["Deckenspots"]);
    }

    #[test]
    fn png_is_64x64() {
        let png = render_png(&face(3, "Hi", "", FaceState::FACE_STATE_IDLE));
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!((img.width(), img.height()), (SIZE, SIZE));
    }
}

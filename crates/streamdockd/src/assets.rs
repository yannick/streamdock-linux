//! Key images (H15-28): pictures the user uploads for custom keys, stored as one PNG per image in
//! `images/` next to the config file (default `~/.config/streamdockd/images/<name>.png`). Keys refer
//! to them by name (`image = "lamp"`), `render.rs` draws them.
//!
//! Uploads are decoded and re-encoded (PNG, at most [`MAX_SIDE`] px), so the directory only ever
//! holds images we made. Every change bumps a generation, so the dock re-sends faces that use one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, bail};
use image::{DynamicImage, ImageFormat, RgbaImage, imageops::FilterType};
use tokio::sync::watch;

/// Largest upload accepted, in bytes.
pub const MAX_UPLOAD: usize = 4 * 1024 * 1024;
/// Stored images are scaled down to fit this many pixels per side.
pub const MAX_SIDE: u32 = 256;
/// Side of the list previews.
pub const PREVIEW_SIDE: u32 = 64;

/// One stored image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Size of the stored PNG.
    pub bytes: u64,
}

/// The image store. Cheap to share through an `Arc`.
#[derive(Debug)]
pub struct Assets {
    dir: PathBuf,
    /// Decoded images by name, loaded on first use.
    cache: Mutex<BTreeMap<String, Arc<RgbaImage>>>,
    generation: watch::Sender<u64>,
}

impl Assets {
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            cache: Mutex::default(),
            generation: watch::Sender::new(0),
        }
    }

    /// `images/` next to `config`.
    #[must_use]
    pub fn beside(config: &Path) -> Self {
        Self::new(config.parent().unwrap_or_else(|| Path::new(".")).join("images"))
    }

    /// Changes whenever an image is added, replaced or deleted.
    #[must_use]
    pub fn generation(&self) -> watch::Receiver<u64> {
        self.generation.subscribe()
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.png"))
    }

    /// The stored images, by name.
    pub fn list(&self) -> anyhow::Result<Vec<Asset>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", self.dir.display())),
        };
        let mut assets = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".png"))
                .filter(|n| valid_name(n))
            else {
                continue;
            };
            let Ok((width, height)) = image::image_dimensions(&path) else {
                tracing::warn!(path = %path.display(), "skipping an unreadable key image");
                continue;
            };
            let bytes = entry.metadata().map_or(0, |m| m.len());
            assets.push(Asset {
                name: name.to_owned(),
                width,
                height,
                bytes,
            });
        }
        assets.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(assets)
    }

    /// Stores `data` (PNG or JPEG) as `name`, replacing an image of that name.
    pub fn put(&self, name: &str, data: &[u8]) -> anyhow::Result<Asset> {
        check_name(name)?;
        anyhow::ensure!(!data.is_empty(), "the image is empty");
        anyhow::ensure!(
            data.len() <= MAX_UPLOAD,
            "the image is too large ({} KB, at most {} KB)",
            data.len() / 1024,
            MAX_UPLOAD / 1024
        );
        let format = image::guess_format(data).context("not an image")?;
        anyhow::ensure!(
            matches!(format, ImageFormat::Png | ImageFormat::Jpeg),
            "only PNG and JPEG images are supported"
        );
        let decoded = image::load_from_memory_with_format(data, format).context("cannot decode the image")?;
        let decoded = if decoded.width() > MAX_SIDE || decoded.height() > MAX_SIDE {
            decoded.resize(MAX_SIDE, MAX_SIDE, FilterType::Lanczos3)
        } else {
            decoded
        };
        let rgba = decoded.to_rgba8();
        let png = encode_png(&DynamicImage::ImageRgba8(rgba.clone()))?;

        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("cannot create {}", self.dir.display()))?;
        let path = self.path(name);
        let tmp = path.with_extension("png.tmp");
        std::fs::write(&tmp, &png).with_context(|| format!("cannot write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("cannot write {}", path.display()))?;

        self.cache
            .lock()
            .expect("asset cache poisoned")
            .insert(name.to_owned(), Arc::new(rgba.clone()));
        self.generation.send_modify(|g| *g += 1);
        Ok(Asset {
            name: name.to_owned(),
            width: rgba.width(),
            height: rgba.height(),
            bytes: png.len() as u64,
        })
    }

    /// Deletes `name`; `Ok(false)` if there was none.
    pub fn delete(&self, name: &str) -> anyhow::Result<bool> {
        check_name(name)?;
        self.cache.lock().expect("asset cache poisoned").remove(name);
        match std::fs::remove_file(self.path(name)) {
            Ok(()) => {
                self.generation.send_modify(|g| *g += 1);
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e).with_context(|| format!("cannot delete the image {name:?}")),
        }
    }

    /// The decoded image `name`, or `None` if there is no such image.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<RgbaImage>> {
        if !valid_name(name) {
            return None;
        }
        if let Some(img) = self.cache.lock().expect("asset cache poisoned").get(name) {
            return Some(Arc::clone(img));
        }
        let img = match image::open(self.path(name)) {
            Ok(img) => Arc::new(img.to_rgba8()),
            Err(image::ImageError::IoError(e)) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::warn!(name, error = %e, "cannot read a key image");
                return None;
            }
        };
        self.cache
            .lock()
            .expect("asset cache poisoned")
            .insert(name.to_owned(), Arc::clone(&img));
        Some(img)
    }

    /// A small PNG of `name` for the settings screen.
    #[must_use]
    pub fn preview(&self, name: &str) -> Option<Vec<u8>> {
        let img = self.get(name)?;
        let small = DynamicImage::ImageRgba8((*img).clone()).thumbnail(PREVIEW_SIDE, PREVIEW_SIDE);
        encode_png(&small).ok()
    }
}

/// Whether `name` can name an image: `[a-z0-9][a-z0-9_-]{0,63}`.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 64
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn check_name(name: &str) -> anyhow::Result<()> {
    if !valid_name(name) {
        bail!("{name:?} is not a valid image name (lowercase letters, digits, - and _, at most 64)");
    }
    Ok(())
}

fn encode_png(img: &DynamicImage) -> anyhow::Result<Vec<u8>> {
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)
        .context("cannot encode the image")?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> (Assets, PathBuf) {
        let dir = std::env::temp_dir().join(format!("streamdockd-assets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (Assets::new(dir.clone()), dir)
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        encode_png(&DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([10, 200, 30, 255]),
        )))
        .unwrap()
    }

    #[test]
    fn stores_lists_and_deletes() {
        let (assets, dir) = store("crud");
        let mut generation = assets.generation();
        assert!(
            assets.list().unwrap().is_empty(),
            "a missing directory is an empty store"
        );

        let stored = assets.put("lamp", &png(40, 20)).unwrap();
        assert_eq!((stored.width, stored.height), (40, 20));
        assert!(generation.has_changed().unwrap());
        generation.borrow_and_update();
        assert_eq!(assets.list().unwrap(), vec![stored]);
        assert_eq!(assets.get("lamp").unwrap().dimensions(), (40, 20));
        assert!(assets.preview("lamp").is_some());

        // Read back from disk by a fresh store.
        assert_eq!(
            Assets::new(dir.clone()).get("lamp").unwrap().dimensions(),
            (40, 20)
        );

        assert!(assets.delete("lamp").unwrap());
        assert!(!assets.delete("lamp").unwrap());
        assert!(generation.has_changed().unwrap());
        assert!(assets.get("lamp").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scales_large_images_down() {
        let (assets, dir) = store("scale");
        let stored = assets.put("big", &png(1024, 512)).unwrap();
        assert_eq!((stored.width, stored.height), (256, 128));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_bad_names_and_data() {
        let (assets, dir) = store("bad");
        for name in ["", "Lamp", "../x", "a b", "-x", &"x".repeat(65)] {
            assert!(assets.put(name, &png(4, 4)).is_err(), "{name:?}");
        }
        assert!(assets.put("x", b"not an image").is_err());
        assert!(assets.put("x", &[]).is_err());
        assert!(assets.get("../etc/passwd").is_none());
        assert!(valid_name("lamp_2-on"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

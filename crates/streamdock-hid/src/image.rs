//! Key image encoding (`docs/protocol.md`, "Image upload").
//!
//! LCD keys take a 64×64 baseline JPEG, rotated 90° clockwise (verified on the device).

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, ImageError};

/// Edge length of a key image in pixels.
pub const KEY_IMAGE_SIZE: u32 = 64;
/// JPEG quality used for key images.
pub const JPEG_QUALITY: u8 = 90;

/// Resizes `image` to the key size, rotates it into device orientation and encodes it as JPEG.
pub fn encode_key_image(image: &DynamicImage) -> Result<Vec<u8>, ImageError> {
    // The daemon renders its faces at key size: no resampling pass for those (H15-88).
    let sized = if image.width() == KEY_IMAGE_SIZE && image.height() == KEY_IMAGE_SIZE {
        image.rotate90()
    } else {
        image
            .resize_exact(KEY_IMAGE_SIZE, KEY_IMAGE_SIZE, FilterType::Lanczos3)
            .rotate90()
    };
    let rgb = sized.into_rgb8();
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY).encode(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, Rgb, RgbImage};

    #[test]
    fn encodes_64x64_jpeg() {
        let src = DynamicImage::ImageRgb8(RgbImage::from_pixel(200, 100, Rgb([0, 128, 255])));
        let jpeg = encode_key_image(&src).unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "JPEG SOI marker");
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!(decoded.dimensions(), (KEY_IMAGE_SIZE, KEY_IMAGE_SIZE));
        // Two chunks of 1024 bytes at most for a key image.
        assert!(jpeg.len() <= 2048, "{} bytes", jpeg.len());
    }

    #[test]
    fn rotates_clockwise() {
        // Red top half: after a 90° clockwise rotation the red half is on the right.
        let mut src = RgbImage::from_pixel(64, 64, Rgb([0, 0, 0]));
        for y in 0..32 {
            for x in 0..64 {
                src.put_pixel(x, y, Rgb([255, 0, 0]));
            }
        }
        let jpeg = encode_key_image(&DynamicImage::ImageRgb8(src)).unwrap();
        let out = image::load_from_memory(&jpeg).unwrap().into_rgb8();
        let right = out.get_pixel(56, 32);
        let left = out.get_pixel(8, 32);
        assert!(right[0] > 200 && left[0] < 50, "right {right:?}, left {left:?}");
    }
}

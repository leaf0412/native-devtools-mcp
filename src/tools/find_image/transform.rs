//! Pure image-decode and pixel-space transform helpers for `find_image`.
//!
//! All functions in this file are CPU-only, no async, no locks, no caches —
//! safe to call inside `spawn_blocking`.

use crate::tools::find_image::params::SearchRegion;
use base64::Engine;
use image::{GrayImage, ImageReader};
use std::io::Cursor;

/// Decode base64 image data to grayscale.
pub(super) fn decode_base64_to_gray(b64: &str) -> Result<GrayImage, String> {
    let data = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("Invalid base64: {}", e))?;
    decode_png_to_gray(&data)
}

/// Decode PNG/JPEG bytes to grayscale.
pub(super) fn decode_png_to_gray(data: &[u8]) -> Result<GrayImage, String> {
    let img = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| format!("Failed to read image format: {}", e))?
        .decode()
        .map_err(|e| format!("Failed to decode image: {}", e))?;
    Ok(img.to_luma8())
}

/// Extract a region from an image.
pub(super) fn extract_region(img: &GrayImage, region: &SearchRegion) -> GrayImage {
    let x = region.x.min(img.width().saturating_sub(1));
    let y = region.y.min(img.height().saturating_sub(1));
    let w = region.w.min(img.width() - x);
    let h = region.h.min(img.height() - y);

    let sub = image::imageops::crop_imm(img, x, y, w, h);
    sub.to_image()
}

/// Resize image by scale factor.
pub(super) fn resize_image(img: &GrayImage, scale: f64) -> GrayImage {
    if (scale - 1.0).abs() < f64::EPSILON {
        return img.clone();
    }

    let new_width = ((img.width() as f64) * scale).round() as u32;
    let new_height = ((img.height() as f64) * scale).round() as u32;

    if new_width == 0 || new_height == 0 {
        return GrayImage::new(1, 1);
    }

    image::imageops::resize(
        img,
        new_width,
        new_height,
        image::imageops::FilterType::Triangle,
    )
}

/// Simple rotation (only supports 0, 90, 180, 270).
/// Expects normalized input from validation (exact 0.0, 90.0, 180.0, or 270.0).
pub(super) fn rotate_image(img: &GrayImage, degrees: f64) -> GrayImage {
    // Use rounding to handle any floating point imprecision
    let rounded = degrees.round() as i32;
    match rounded {
        90 => image::imageops::rotate90(img),
        180 => image::imageops::rotate180(img),
        270 => image::imageops::rotate270(img),
        _ => img.clone(), // 0 or fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Luma;

    #[test]
    fn test_extract_region_clamps_to_bounds() {
        let img = GrayImage::from_fn(100, 100, |x, y| Luma([(x + y) as u8]));

        // Region exceeds image bounds
        let region = SearchRegion {
            x: 80,
            y: 90,
            w: 50,
            h: 50,
        };
        let extracted = extract_region(&img, &region);

        // Should be clamped to available space
        assert_eq!(extracted.width(), 20); // 100 - 80
        assert_eq!(extracted.height(), 10); // 100 - 90
    }

    #[test]
    fn test_rotate_image_90_degrees() {
        // Create a simple 2x3 image
        let img = GrayImage::from_vec(2, 3, vec![1, 2, 3, 4, 5, 6]).unwrap();
        let rotated = rotate_image(&img, 90.0);

        // After 90 degree rotation, 2x3 becomes 3x2
        assert_eq!(rotated.width(), 3);
        assert_eq!(rotated.height(), 2);
    }

    #[test]
    fn test_resize_image_zero_scale() {
        let img = GrayImage::from_fn(10, 10, |_, _| Luma([128]));

        // Very small scale should produce 1x1 minimum
        let tiny = resize_image(&img, 0.01);
        assert!(tiny.width() >= 1);
        assert!(tiny.height() >= 1);
    }
}

//! Integration tests for the `find_image` orchestrator (`handler::execute`).
//!
//! These exercise the full pipeline: cache lookup → seam fetch → spawn_blocking
//! decode + run_matching → finalize. Algorithm-only tests live next to the
//! algorithm in `algorithm.rs`.

use super::execute;
use crate::tools::find_image::params::{FindImageParams, FindImageResponse, ScaleRange};
use crate::tools::find_image::source::Caches;
use crate::tools::image_cache::{ImageCache, ImageMetadata};
use crate::tools::screenshot_cache::ScreenshotCache;
use base64::Engine;
use image::GenericImage;
use std::io::Cursor;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Create a test PNG in memory and return bytes
fn create_test_png_bytes(width: u32, height: u32) -> Vec<u8> {
    let img = image::DynamicImage::new_rgb8(width, height);
    let mut cursor = Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
    cursor.into_inner()
}

fn make_image_metadata(width: u32, height: u32) -> ImageMetadata {
    ImageMetadata {
        source_path: None,
        width,
        height,
        channels: 3,
        mime: "image/png".to_string(),
        sha256: None,
        is_mask: false,
    }
}

fn fresh_caches() -> Caches {
    Caches {
        screenshot: Arc::new(RwLock::new(ScreenshotCache::default())),
        image: Arc::new(RwLock::new(ImageCache::default())),
    }
}

#[tokio::test]
async fn test_find_image_missing_template_id_errors() {
    let caches = fresh_caches();

    let params = FindImageParams {
        screenshot_id: None,
        screenshot_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(create_test_png_bytes(100, 100)),
        ),
        template_id: Some("nonexistent-id".to_string()),
        template_image_base64: None,
        mask_id: None,
        mask_image_base64: None,
        mode: "fast".to_string(),
        threshold: None,
        max_results: None,
        scales: None,
        rotations: None,
        search_region: None,
        stride: None,
        return_screen_coords: false,
    };

    let result = execute(params, caches).await;
    assert!(result.is_error.unwrap_or(false));
    // Check error message mentions template ID
    if let rmcp::model::RawContent::Text(rmcp::model::RawTextContent { text }) =
        &result.content[0].raw
    {
        assert!(text.contains("Template ID") && text.contains("not found"));
    }
}

#[tokio::test]
async fn test_find_image_missing_mask_id_errors() {
    let caches = fresh_caches();

    // Add a template to the cache
    {
        let mut cache = caches.image.write().await;
        cache.store(
            create_test_png_bytes(32, 32),
            make_image_metadata(32, 32),
            Some("template"),
        );
    }

    let params = FindImageParams {
        screenshot_id: None,
        screenshot_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(create_test_png_bytes(100, 100)),
        ),
        template_id: None,
        template_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(create_test_png_bytes(16, 16)),
        ),
        mask_id: Some("nonexistent-mask".to_string()),
        mask_image_base64: None,
        mode: "fast".to_string(),
        threshold: None,
        max_results: None,
        scales: None,
        rotations: None,
        search_region: None,
        stride: None,
        return_screen_coords: false,
    };

    let result = execute(params, caches).await;
    assert!(result.is_error.unwrap_or(false));
    // Check error message mentions mask ID
    if let rmcp::model::RawContent::Text(rmcp::model::RawTextContent { text }) =
        &result.content[0].raw
    {
        assert!(text.contains("Mask ID") && text.contains("not found"));
    }
}

#[tokio::test]
async fn test_find_image_requires_template_source() {
    let caches = fresh_caches();

    let params = FindImageParams {
        screenshot_id: None,
        screenshot_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(create_test_png_bytes(100, 100)),
        ),
        template_id: None,
        template_image_base64: None, // Neither template_id nor template_image_base64
        mask_id: None,
        mask_image_base64: None,
        mode: "fast".to_string(),
        threshold: None,
        max_results: None,
        scales: None,
        rotations: None,
        search_region: None,
        stride: None,
        return_screen_coords: false,
    };

    let result = execute(params, caches).await;
    assert!(result.is_error.unwrap_or(false));
    if let rmcp::model::RawContent::Text(rmcp::model::RawTextContent { text }) =
        &result.content[0].raw
    {
        assert!(text.contains("template_id") || text.contains("template_image_base64"));
    }
}

#[tokio::test]
async fn test_find_image_with_template_id_from_cache() {
    let caches = fresh_caches();

    // Create a pattern with variation (gradient) that NCC can match
    let mut screenshot_img = image::DynamicImage::new_rgb8(100, 100);
    // Fill with gray background
    for y in 0..100u32 {
        for x in 0..100u32 {
            screenshot_img.put_pixel(x, y, image::Rgba([128, 128, 128, 255]));
        }
    }
    // Draw a diagonal gradient pattern at (30, 40)
    for y in 0..10u32 {
        for x in 0..10u32 {
            let val = ((x + y) * 12 + 50) as u8;
            screenshot_img.put_pixel(30 + x, 40 + y, image::Rgba([val, val, val, 255]));
        }
    }
    let mut screenshot_bytes = Cursor::new(Vec::new());
    screenshot_img
        .write_to(&mut screenshot_bytes, image::ImageFormat::Png)
        .unwrap();

    // Create template matching the gradient pattern
    let mut template_img = image::DynamicImage::new_rgb8(10, 10);
    for y in 0..10u32 {
        for x in 0..10u32 {
            let val = ((x + y) * 12 + 50) as u8;
            template_img.put_pixel(x, y, image::Rgba([val, val, val, 255]));
        }
    }
    let mut template_bytes = Cursor::new(Vec::new());
    template_img
        .write_to(&mut template_bytes, image::ImageFormat::Png)
        .unwrap();

    // Add template to cache
    let template_id = {
        let mut cache = caches.image.write().await;
        cache.store(
            template_bytes.into_inner(),
            make_image_metadata(10, 10),
            Some("template"),
        )
    };

    let params = FindImageParams {
        screenshot_id: None,
        screenshot_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(screenshot_bytes.into_inner()),
        ),
        template_id: Some(template_id),
        template_image_base64: None,
        mask_id: None,
        mask_image_base64: None,
        mode: "fast".to_string(),
        threshold: Some(0.9),
        max_results: None,
        scales: Some(ScaleRange {
            min: 1.0,
            max: 1.0,
            step: 0.1,
        }), // Exact scale only
        rotations: None,
        search_region: None,
        stride: Some(1),
        return_screen_coords: false,
    };

    let result = execute(params, caches).await;
    assert!(
        !result.is_error.unwrap_or(true),
        "find_image should succeed"
    );

    // Parse response and verify we got a match
    if let rmcp::model::RawContent::Text(rmcp::model::RawTextContent { text }) =
        &result.content[0].raw
    {
        let response: FindImageResponse = serde_json::from_str(text).unwrap();
        assert!(
            !response.matches.is_empty(),
            "Should find at least one match"
        );
        // The match should be near (30, 40)
        let best = &response.matches[0];
        assert!(
            best.bbox.x >= 28 && best.bbox.x <= 32,
            "x should be near 30, got {}",
            best.bbox.x
        );
        assert!(
            best.bbox.y >= 38 && best.bbox.y <= 42,
            "y should be near 40, got {}",
            best.bbox.y
        );
    }
}

#[tokio::test]
async fn test_find_image_stale_template_id_falls_back_to_base64() {
    let caches = fresh_caches();

    // Provide a stale template_id but valid base64 fallback
    let params = FindImageParams {
        screenshot_id: None,
        screenshot_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(create_test_png_bytes(50, 50)),
        ),
        template_id: Some("stale-id-not-in-cache".to_string()),
        template_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(create_test_png_bytes(8, 8)),
        ),
        mask_id: None,
        mask_image_base64: None,
        mode: "fast".to_string(),
        threshold: Some(0.5),
        max_results: None,
        scales: Some(ScaleRange {
            min: 1.0,
            max: 1.0,
            step: 0.1,
        }),
        rotations: None,
        search_region: None,
        stride: None,
        return_screen_coords: false,
    };

    // Should succeed using base64 fallback with a warning
    let result = execute(params, caches).await;
    assert!(
        !result.is_error.unwrap_or(true),
        "Should succeed using base64 fallback"
    );

    // Verify warning is present
    if let rmcp::model::RawContent::Text(rmcp::model::RawTextContent { text }) =
        &result.content[0].raw
    {
        let response: FindImageResponse = serde_json::from_str(text).unwrap();
        assert!(
            response.warning.is_some(),
            "Should have warning about stale ID"
        );
        assert!(
            response
                .warning
                .as_ref()
                .unwrap()
                .contains("not found in cache"),
            "Warning should mention cache miss"
        );
    }
}

#[tokio::test]
async fn test_find_image_stale_mask_id_falls_back_to_base64() {
    let caches = fresh_caches();

    // Provide valid template but stale mask_id with base64 fallback
    let params = FindImageParams {
        screenshot_id: None,
        screenshot_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(create_test_png_bytes(50, 50)),
        ),
        template_id: None,
        template_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(create_test_png_bytes(8, 8)),
        ),
        mask_id: Some("stale-mask-id".to_string()),
        mask_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(create_test_png_bytes(8, 8)),
        ),
        mode: "fast".to_string(),
        threshold: Some(0.5),
        max_results: None,
        scales: Some(ScaleRange {
            min: 1.0,
            max: 1.0,
            step: 0.1,
        }),
        rotations: None,
        search_region: None,
        stride: None,
        return_screen_coords: false,
    };

    // Should succeed using base64 fallback
    let result = execute(params, caches).await;
    assert!(
        !result.is_error.unwrap_or(true),
        "Should succeed using mask base64 fallback"
    );

    // Verify warning is present
    if let rmcp::model::RawContent::Text(rmcp::model::RawTextContent { text }) =
        &result.content[0].raw
    {
        let response: FindImageResponse = serde_json::from_str(text).unwrap();
        assert!(
            response.warning.is_some(),
            "Should have warning about stale mask ID"
        );
        assert!(
            response.warning.as_ref().unwrap().contains("Mask ID"),
            "Warning should mention mask"
        );
    }
}

#[tokio::test]
async fn test_find_image_mask_dimension_mismatch_errors() {
    let caches = fresh_caches();

    // Template is 10x10, mask is 8x8 - dimension mismatch
    let params = FindImageParams {
        screenshot_id: None,
        screenshot_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(create_test_png_bytes(50, 50)),
        ),
        template_id: None,
        template_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(create_test_png_bytes(10, 10)),
        ),
        mask_id: None,
        mask_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(create_test_png_bytes(8, 8)),
        ),
        mode: "fast".to_string(),
        threshold: None,
        max_results: None,
        scales: Some(ScaleRange {
            min: 1.0,
            max: 1.0,
            step: 0.1,
        }),
        rotations: None,
        search_region: None,
        stride: None,
        return_screen_coords: false,
    };

    let result = execute(params, caches).await;
    assert!(
        result.is_error.unwrap_or(false),
        "Should error on mask dimension mismatch"
    );

    // Verify error message mentions dimension mismatch
    if let rmcp::model::RawContent::Text(rmcp::model::RawTextContent { text }) =
        &result.content[0].raw
    {
        assert!(
            text.contains("Mask dimensions") && text.contains("must match"),
            "Error should mention mask dimension mismatch, got: {}",
            text
        );
    }
}

/// New for T3: ensure that a Screenshot id miss with no base64 fallback
/// warns via the seam and lets `run_matching` produce the hard error
/// "Either screenshot_id or screenshot_image_base64 must be provided".
/// Before the seam, no test explicitly exercised this combined path.
#[tokio::test]
async fn test_image_source_screenshot_id_miss_then_no_b64_warns_and_lets_match_fail() {
    let caches = fresh_caches();

    let params = FindImageParams {
        screenshot_id: Some("missing-screenshot-id".to_string()),
        screenshot_image_base64: None,
        template_id: None,
        template_image_base64: Some(
            base64::engine::general_purpose::STANDARD.encode(create_test_png_bytes(8, 8)),
        ),
        mask_id: None,
        mask_image_base64: None,
        mode: "fast".to_string(),
        threshold: None,
        max_results: None,
        scales: None,
        rotations: None,
        search_region: None,
        stride: None,
        return_screen_coords: false,
    };

    let result = execute(params, caches).await;

    assert!(
        result.is_error.unwrap_or(false),
        "Missing screenshot id + no base64 must surface as a hard error"
    );

    if let rmcp::model::RawContent::Text(rmcp::model::RawTextContent { text }) =
        &result.content[0].raw
    {
        assert!(
            text.contains("Either screenshot_id or screenshot_image_base64 must be provided"),
            "Expected the post-resolve hard error, got: {}",
            text
        );
    } else {
        panic!("Expected text content with the hard error");
    }
}

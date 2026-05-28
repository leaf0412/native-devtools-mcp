//! Algorithm-level unit tests for `find_image`.
//!
//! Exercises functions in `algorithm.rs` directly: NCC kernels,
//! downscale heuristic, work-item builder, process_work_item, and
//! `run_matching` with hand-built `GrayImage`s. End-to-end integration
//! tests live in `handler_tests.rs`.

use super::*;
use crate::tools::find_image::params::ScaleRange;
use image::Luma;

#[test]
fn test_ncc_matching_finds_exact_template() {
    // Create a 100x100 image with a distinct gradient pattern at (30, 40)
    let mut image = GrayImage::from_fn(100, 100, |_, _| Luma([128]));

    // Draw a gradient pattern at position (30, 40) - needs variation for NCC
    for y in 0..10u32 {
        for x in 0..10u32 {
            // Create a diagonal gradient pattern
            let val = ((x + y) * 12 + 50) as u8;
            image.put_pixel(30 + x, 40 + y, Luma([val]));
        }
    }

    // Create template that matches the pattern exactly
    let template = GrayImage::from_fn(10, 10, |x, y| {
        let val = ((x + y) * 12 + 50) as u8;
        Luma([val])
    });

    // Run NCC matching
    let matches = match_template_ncc(&image, &template, None, 0.9, 1);

    // Should find the match
    assert!(!matches.is_empty(), "Should find at least one match");

    // Best match should be near (30, 40)
    let (best_x, best_y, best_score) = matches
        .iter()
        .max_by(|a, b| a.2.partial_cmp(&b.2).unwrap())
        .cloned()
        .unwrap();

    assert!(
        best_score > 0.95,
        "Score should be very high for exact match, got {}",
        best_score
    );
    assert_eq!(best_x, 30, "X should be 30, got {}", best_x);
    assert_eq!(best_y, 40, "Y should be 40, got {}", best_y);
}

#[test]
fn test_ncc_no_match_for_different_pattern() {
    // Create image with a horizontal gradient
    let image = GrayImage::from_fn(50, 50, |x, _| Luma([(x * 5) as u8]));

    // Template with vertical gradient (orthogonal pattern)
    let template = GrayImage::from_fn(10, 10, |_, y| Luma([(y * 25) as u8]));

    // High threshold, should find no good matches for orthogonal patterns
    let matches = match_template_ncc(&image, &template, None, 0.9, 1);

    // NCC can produce negative correlation for orthogonal patterns
    // so we check that high-threshold matches are not found
    assert!(
        matches.is_empty(),
        "Should not find match for orthogonal pattern at high threshold, got {} matches",
        matches.len()
    );
}

#[test]
fn test_compute_downscale_factor_small_image() {
    // Image smaller than 1200px max dimension - no downscale
    let img = GrayImage::new(800, 600);
    let template = GrayImage::new(32, 32);
    let factor = compute_downscale_factor(&img, &template);
    assert!(
        (factor - 1.0).abs() < f64::EPSILON,
        "Small images should not be downscaled"
    );
}

#[test]
fn test_compute_downscale_factor_large_image() {
    // 1920x1080 image - should be downscaled
    let img = GrayImage::new(1920, 1080);
    let template = GrayImage::new(32, 32);
    let factor = compute_downscale_factor(&img, &template);
    // 1200 / 1920 = 0.625
    assert!(factor < 1.0, "Large images should be downscaled");
    assert!(factor >= 0.5, "Downscale should not go below 0.5");
    assert!(
        (factor - 0.625).abs() < 0.01,
        "Expected ~0.625, got {}",
        factor
    );
}

#[test]
fn test_compute_downscale_factor_very_large_image() {
    // 4K image - should be capped at 0.5
    let img = GrayImage::new(3840, 2160);
    let template = GrayImage::new(32, 32);
    let factor = compute_downscale_factor(&img, &template);
    // 1200 / 3840 = 0.3125, but capped at 0.5
    assert!(
        (factor - 0.5).abs() < f64::EPSILON,
        "Very large images should cap at 0.5"
    );
}

#[test]
fn test_build_work_items() {
    let rotations = vec![0.0, 90.0];
    let template = GrayImage::new(10, 10);
    let search_img = GrayImage::new(100, 100);
    let rotated_templates = build_rotated_templates(&template, None, &rotations);
    let scales = ScaleRange {
        min: 0.8,
        max: 1.2,
        step: 0.2,
    };
    let items = build_work_items(&rotations, &scales, &rotated_templates, &search_img);

    // Should have: 0.8, 1.0, 1.2 for each rotation = 3 * 2 = 6 items
    assert_eq!(items.len(), 6, "Expected 6 work items, got {}", items.len());

    // Verify first rotation (0.0) items
    assert!((items[0].rotation - 0.0).abs() < f64::EPSILON);
    assert!((items[0].scale - 0.8).abs() < f64::EPSILON);
    assert!((items[1].rotation - 0.0).abs() < f64::EPSILON);
    assert!((items[1].scale - 1.0).abs() < f64::EPSILON);
    assert!((items[2].rotation - 0.0).abs() < f64::EPSILON);
    assert!((items[2].scale - 1.2).abs() < f64::EPSILON);

    // Verify second rotation (90.0) items
    assert!((items[3].rotation - 90.0).abs() < f64::EPSILON);
    assert!((items[3].scale - 0.8).abs() < f64::EPSILON);
}

#[test]
fn test_build_work_items_single_scale() {
    let rotations = vec![0.0];
    let template = GrayImage::new(10, 10);
    let search_img = GrayImage::new(100, 100);
    let rotated_templates = build_rotated_templates(&template, None, &rotations);
    let scales = ScaleRange {
        min: 1.0,
        max: 1.0,
        step: 0.1,
    };
    let items = build_work_items(&rotations, &scales, &rotated_templates, &search_img);

    assert_eq!(items.len(), 1, "Expected 1 work item for single scale");
    assert!((items[0].scale - 1.0).abs() < f64::EPSILON);
}

#[test]
fn test_process_work_item_template_too_large() {
    let search_img = GrayImage::new(50, 50);
    let template = GrayImage::new(100, 100); // Larger than search image

    let item = WorkItem {
        rotation: 0.0,
        rotation_idx: 0,
        scale: 1.0,
    };

    let result = process_work_item(
        &item,
        &search_img,
        &template,
        None,
        0.88,
        1,
        (0, 0),
        1.0,
        None,
        false,
    );

    assert!(
        result.is_none(),
        "Should return None when template exceeds image"
    );
}

#[test]
fn test_process_work_item_downscale_coordinate_mapping() {
    // Create a search image and template
    let mut search_img = GrayImage::from_fn(100, 100, |_, _| Luma([128]));

    // Place a distinct pattern at position (40, 50)
    for y in 0..10u32 {
        for x in 0..10u32 {
            let val = ((x + y) * 12 + 50) as u8;
            search_img.put_pixel(40 + x, 50 + y, Luma([val]));
        }
    }

    // Create matching template
    let template = GrayImage::from_fn(10, 10, |x, y| {
        let val = ((x + y) * 12 + 50) as u8;
        Luma([val])
    });

    let item = WorkItem {
        rotation: 0.0,
        rotation_idx: 0,
        scale: 1.0,
    };

    // Test with downscale_factor = 1.0 (no downscaling)
    let result = process_work_item(
        &item,
        &search_img,
        &template,
        None,
        0.9,
        1,
        (0, 0),
        1.0, // No downscale
        None,
        false,
    );

    assert!(result.is_some(), "Should find matches");
    let matches = result.unwrap();
    assert!(!matches.is_empty(), "Should have at least one match");

    // Best match should be near (40, 50)
    let best = matches
        .iter()
        .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap())
        .unwrap();
    assert!(
        (best.bbox.x as i32 - 40).abs() <= 2,
        "X should be near 40, got {}",
        best.bbox.x
    );
    assert!(
        (best.bbox.y as i32 - 50).abs() <= 2,
        "Y should be near 50, got {}",
        best.bbox.y
    );
}

#[cfg(feature = "find_image_simd")]
#[test]
fn test_simd_ncc_matches_scalar() {
    // Create test images
    let image = GrayImage::from_fn(100, 100, |x, y| {
        Luma([((x.wrapping_mul(3).wrapping_add(y.wrapping_mul(7))) % 255) as u8])
    });
    let template = GrayImage::from_fn(20, 20, |x, y| {
        Luma([((x.wrapping_mul(3).wrapping_add(y.wrapping_mul(7))) % 255) as u8])
    });

    let tpl_stats = compute_template_stats(&template, None);

    // Compute scalar result
    let scalar_score = compute_ncc_at(&image, &template, None, 0, 0, &tpl_stats);

    // Compute SIMD result
    let simd_score = compute_ncc_at_simd(&image, &template, 0, 0, &tpl_stats);

    // Results should be very close (within floating point tolerance)
    assert!(
        (scalar_score - simd_score).abs() < 0.001,
        "SIMD and scalar should match: scalar={}, simd={}",
        scalar_score,
        simd_score
    );
}

/// New for T3: exercise `run_matching` directly with hand-built
/// `GrayImage`s. Before the source seam, the algorithm could only be
/// driven through `find_image()` with PNG-encoded base64 strings and
/// a `spawn_blocking` boundary; this test would have been
/// structurally impossible.
#[test]
fn test_run_matching_with_inline_grayimage() {
    // 100x100 search image: gray field with a distinct gradient patch at (40, 50)
    let mut screenshot = GrayImage::from_fn(100, 100, |_, _| Luma([128]));
    for y in 0..10u32 {
        for x in 0..10u32 {
            let val = ((x + y) * 12 + 50) as u8;
            screenshot.put_pixel(40 + x, 50 + y, Luma([val]));
        }
    }

    // Template that matches that patch exactly
    let template = GrayImage::from_fn(10, 10, |x, y| {
        let val = ((x + y) * 12 + 50) as u8;
        Luma([val])
    });

    let req = MatchRequest {
        screenshot,
        template,
        mask: None,
        screenshot_meta: None,
        search_region: None,
        threshold: 0.9,
        max_results: 3,
        scales: ScaleRange {
            min: 1.0,
            max: 1.0,
            step: 0.1,
        },
        stride: 1,
        rotations: vec![0.0],
        return_screen_coords: false,
        is_fast_mode: false,
    };

    let outcome = run_matching(req);
    match outcome {
        MatchOutcome::Success(matches) => {
            assert!(!matches.is_empty(), "Should find at least one match");
            let best = &matches[0];
            assert!(
                (best.bbox.x as i32 - 40).abs() <= 2,
                "x should be near 40, got {}",
                best.bbox.x
            );
            assert!(
                (best.bbox.y as i32 - 50).abs() <= 2,
                "y should be near 50, got {}",
                best.bbox.y
            );
        }
        MatchOutcome::Error(e) => panic!("run_matching errored unexpectedly: {}", e),
    }
}

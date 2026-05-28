//! CPU-bound matching algorithm for `find_image`.
//!
//! Owns the blocking `run_matching` pipeline (region crop → optional
//! downscale → rotated templates → work items → NCC → NMS), the SIMD
//! and scalar NCC kernels, and the `MatchRequest` / `MatchOutcome`
//! contract that crosses the `spawn_blocking` boundary.
//!
//! Decode (PNG/Base64 → `GrayImage`) and the async resolve flow live
//! in `handler::execute`; this file does not touch caches, params, or
//! locks. The handler hands us already-decoded `GrayImage`s.

use crate::tools::find_image::nms::non_maximum_suppression;
use crate::tools::find_image::params::{BoundingBox, MatchResult, Point, ScaleRange, SearchRegion};
use crate::tools::find_image::transform::{extract_region, resize_image, rotate_image};
use crate::tools::screenshot_cache::ScreenshotMetadata;
use image::GrayImage;
use std::borrow::Cow;

#[cfg(feature = "find_image_parallel")]
use rayon::prelude::*;

#[cfg(feature = "find_image_parallel")]
use std::sync::OnceLock;

/// Static thread pool for parallel find_image operations.
/// Uses half of available CPUs to avoid oversubscribing when running inside spawn_blocking.
#[cfg(feature = "find_image_parallel")]
static FIND_IMAGE_POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();

#[cfg(feature = "find_image_parallel")]
fn get_thread_pool() -> &'static rayon::ThreadPool {
    FIND_IMAGE_POOL.get_or_init(|| {
        let num_threads = (rayon::current_num_threads() / 2).max(1);
        rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .build()
            .expect("failed to create rayon thread pool")
    })
}

/// Already-decoded inputs `run_matching` needs to do its work.
///
/// The handler resolves cache lookups and decodes PNG/Base64 to
/// `GrayImage` BEFORE constructing this — the algorithm is decoupled
/// from `Caches`, `ImageSource`, and `RawImageBytes`, which means it
/// can be exercised in unit tests with hand-built gradient images.
pub(super) struct MatchRequest {
    pub(super) screenshot: GrayImage,
    pub(super) template: GrayImage,
    pub(super) mask: Option<GrayImage>,
    pub(super) screenshot_meta: Option<ScreenshotMetadata>,
    pub(super) search_region: Option<SearchRegion>,
    pub(super) threshold: f64,
    pub(super) max_results: usize,
    pub(super) scales: ScaleRange,
    pub(super) stride: u32,
    pub(super) rotations: Vec<f64>,
    pub(super) return_screen_coords: bool,
    /// Whether this is "fast" mode (enables downscaling and early exit).
    pub(super) is_fast_mode: bool,
}

/// Work item for parallel processing of scale/rotation combinations.
#[derive(Clone)]
pub(super) struct WorkItem {
    pub(super) rotation: f64,
    pub(super) rotation_idx: usize,
    pub(super) scale: f64,
}

/// Pre-rotated template and mask for a specific rotation angle.
pub(super) struct RotatedTemplates {
    pub(super) template: GrayImage,
    pub(super) mask: Option<GrayImage>,
}

/// Outcome of `run_matching`. `Error` carries a user-facing message
/// (e.g. the mask-dimension-mismatch literal); `Success` carries the
/// final NMS-filtered match list.
pub(super) enum MatchOutcome {
    Success(Vec<MatchResult>),
    Error(String),
}

/// Compute the downscale factor for fast mode.
///
/// In fast mode, if the search image max dimension exceeds 1200px, we downscale
/// to reduce NCC computation. The downscale factor is capped at 0.5 to avoid
/// losing too much detail.
pub(super) fn compute_downscale_factor(search_img: &GrayImage, _template: &GrayImage) -> f64 {
    let max_dim = search_img.width().max(search_img.height()) as f64;
    const TARGET_MAX_DIM: f64 = 1200.0;
    const MIN_DOWNSCALE: f64 = 0.5;

    if max_dim <= TARGET_MAX_DIM {
        1.0
    } else {
        (TARGET_MAX_DIM / max_dim).max(MIN_DOWNSCALE)
    }
}

/// Build a list of work items from rotations and scales, pruning scales that
/// would make the template larger than the search image.
pub(super) fn build_work_items(
    rotations: &[f64],
    scales: &ScaleRange,
    rotated_templates: &[RotatedTemplates],
    search_img: &GrayImage,
) -> Vec<WorkItem> {
    let mut items = Vec::new();
    for (rotation_idx, &rotation) in rotations.iter().enumerate() {
        let tpl = &rotated_templates[rotation_idx].template;
        let max_scale_w = search_img.width() as f64 / tpl.width() as f64;
        let max_scale_h = search_img.height() as f64 / tpl.height() as f64;
        let max_scale = max_scale_w.min(max_scale_h);

        let mut scale = scales.min;
        while scale <= scales.max + f64::EPSILON && scale <= max_scale + f64::EPSILON {
            items.push(WorkItem {
                rotation,
                rotation_idx,
                scale,
            });
            scale += scales.step;
        }
    }
    items
}

/// Pre-compute rotated templates for each unique rotation angle.
/// Returns a Vec indexed by rotation_idx.
pub(super) fn build_rotated_templates(
    template: &GrayImage,
    mask: Option<&GrayImage>,
    rotations: &[f64],
) -> Vec<RotatedTemplates> {
    rotations
        .iter()
        .map(|&rotation| RotatedTemplates {
            template: rotate_image(template, rotation),
            mask: mask.map(|m| rotate_image(m, rotation)),
        })
        .collect()
}

/// Process a single work item (rotation + scale combination).
/// Returns matches for this specific configuration.
/// The `rotated_template` and `rotated_mask` should be pre-rotated for this work item's rotation.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_work_item(
    item: &WorkItem,
    search_img: &GrayImage,
    rotated_template: &GrayImage,
    rotated_mask: Option<&GrayImage>,
    threshold: f64,
    stride: u32,
    region_offset: (u32, u32),
    downscale_factor: f64,
    screenshot_metadata: Option<&ScreenshotMetadata>,
    return_screen_coords: bool,
) -> Option<Vec<MatchResult>> {
    // Scale the pre-rotated template and mask
    let scaled_template = resize_image(rotated_template, item.scale);
    let scaled_mask = rotated_mask.map(|m| resize_image(m, item.scale));

    // Check if template fits in search image
    if scaled_template.width() > search_img.width()
        || scaled_template.height() > search_img.height()
    {
        return None; // Template too large
    }

    // Run NCC matching
    let matches = match_template_ncc(
        search_img,
        &scaled_template,
        scaled_mask.as_ref(),
        threshold,
        stride,
    );

    if matches.is_empty() {
        return Some(Vec::new());
    }

    // Convert to MatchResult with adjusted coordinates
    let results: Vec<MatchResult> = matches
        .into_iter()
        .map(|(x, y, score)| {
            // Map coordinates back from downscaled space to original space
            let full_x = if downscale_factor < 1.0 {
                (x as f64 / downscale_factor).round() as u32
            } else {
                x
            };
            let full_y = if downscale_factor < 1.0 {
                (y as f64 / downscale_factor).round() as u32
            } else {
                y
            };
            let full_tw = if downscale_factor < 1.0 {
                (scaled_template.width() as f64 / downscale_factor).round() as u32
            } else {
                scaled_template.width()
            };
            let full_th = if downscale_factor < 1.0 {
                (scaled_template.height() as f64 / downscale_factor).round() as u32
            } else {
                scaled_template.height()
            };

            let adjusted_x = full_x + region_offset.0;
            let adjusted_y = full_y + region_offset.1;

            let center_x = adjusted_x as f64 + full_tw as f64 / 2.0;
            let center_y = adjusted_y as f64 + full_th as f64 / 2.0;

            // Convert to screen coordinates if metadata available
            let (screen_x, screen_y) = if return_screen_coords {
                if let Some(meta) = screenshot_metadata {
                    let sx = meta.origin_x + center_x / meta.scale;
                    let sy = meta.origin_y + center_y / meta.scale;
                    (Some(sx), Some(sy))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };

            MatchResult {
                score,
                bbox: BoundingBox {
                    x: adjusted_x,
                    y: adjusted_y,
                    w: full_tw,
                    h: full_th,
                },
                center: Point {
                    x: center_x,
                    y: center_y,
                },
                scale: item.scale,
                rotation: item.rotation,
                screen_x,
                screen_y,
            }
        })
        .collect();

    Some(results)
}

/// CPU-intensive matching pipeline, runs on a blocking thread.
///
/// The handler decodes images and resolves caches before calling this;
/// the only error this path emits today is the mask-dimension-mismatch
/// literal — preserved verbatim from the pre-split code.
pub(super) fn run_matching(req: MatchRequest) -> MatchOutcome {
    let MatchRequest {
        screenshot: screenshot_gray,
        template: template_gray,
        mask,
        screenshot_meta: screenshot_metadata,
        search_region,
        threshold,
        max_results,
        scales,
        stride,
        rotations,
        return_screen_coords,
        is_fast_mode,
    } = req;

    // Validate mask dimensions match template
    if let Some(mask_img) = &mask {
        if mask_img.width() != template_gray.width() || mask_img.height() != template_gray.height()
        {
            return MatchOutcome::Error(format!(
                "Mask dimensions ({}x{}) must match template dimensions ({}x{})",
                mask_img.width(),
                mask_img.height(),
                template_gray.width(),
                template_gray.height()
            ));
        }
    }

    // Extract search region if specified (use Cow to avoid cloning large screenshot)
    let (search_img_region, region_offset) = if let Some(region) = &search_region {
        (
            Cow::Owned(extract_region(&screenshot_gray, region)),
            (region.x, region.y),
        )
    } else {
        (Cow::Borrowed(&screenshot_gray), (0, 0))
    };

    // Apply dynamic downscale in fast mode
    let downscale_factor = if is_fast_mode {
        compute_downscale_factor(&search_img_region, &template_gray)
    } else {
        1.0
    };

    // Prepare images for matching
    let (search_img, template_for_matching, mask_for_matching) = if downscale_factor < 1.0 {
        (
            resize_image(&search_img_region, downscale_factor),
            resize_image(&template_gray, downscale_factor),
            mask.as_ref().map(|m| resize_image(m, downscale_factor)),
        )
    } else {
        (
            search_img_region.into_owned(),
            template_gray.clone(),
            mask.clone(),
        )
    };

    // Pre-compute rotated templates once per rotation, then build pruned work items
    let rotated_templates = build_rotated_templates(
        &template_for_matching,
        mask_for_matching.as_ref(),
        &rotations,
    );
    let work_items = build_work_items(&rotations, &scales, &rotated_templates, &search_img);

    // Process work items (parallel or sequential based on feature flag)
    #[cfg(feature = "find_image_parallel")]
    let all_matches: Vec<MatchResult> = {
        let results: Vec<Vec<MatchResult>> = get_thread_pool().install(|| {
            work_items
                .par_iter()
                .filter_map(|item| {
                    let rotated = &rotated_templates[item.rotation_idx];
                    let matches = process_work_item(
                        item,
                        &search_img,
                        &rotated.template,
                        rotated.mask.as_ref(),
                        threshold,
                        stride,
                        region_offset,
                        downscale_factor,
                        screenshot_metadata.as_ref(),
                        return_screen_coords,
                    )?;

                    Some(matches)
                })
                .collect()
        });

        results.into_iter().flatten().collect()
    };

    #[cfg(not(feature = "find_image_parallel"))]
    let all_matches: Vec<MatchResult> = {
        let mut matches = Vec::new();
        let mut high_conf_matches: Vec<MatchResult> = Vec::new();

        // Early exit threshold: stop when we have enough unique high-confidence matches
        let early_exit_threshold = if is_fast_mode {
            threshold.max(0.95)
        } else {
            1.1 // Effectively disabled in accurate mode
        };

        for item in &work_items {
            let rotated = &rotated_templates[item.rotation_idx];
            match process_work_item(
                item,
                &search_img,
                &rotated.template,
                rotated.mask.as_ref(),
                threshold,
                stride,
                region_offset,
                downscale_factor,
                screenshot_metadata.as_ref(),
                return_screen_coords,
            ) {
                Some(item_matches) => {
                    // Track high-confidence matches for early-exit (after NMS)
                    if is_fast_mode {
                        for m in &item_matches {
                            if m.score >= early_exit_threshold {
                                high_conf_matches.push(m.clone());
                            }
                        }
                        if high_conf_matches.len() >= max_results {
                            let nms = non_maximum_suppression(
                                high_conf_matches.clone(),
                                0.3,
                                max_results,
                            );
                            if nms.len() >= max_results {
                                break;
                            }
                        }
                    }
                    matches.extend(item_matches);
                }
                None => {}
            }
        }

        matches
    };

    // Sort by score for deterministic NMS (especially important for parallel execution)
    let mut sorted_matches = all_matches;
    sorted_matches.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Apply Non-Maximum Suppression
    let final_matches = non_maximum_suppression(sorted_matches, 0.3, max_results);

    MatchOutcome::Success(final_matches)
}

/// Normalized Cross-Correlation template matching.
///
/// Returns a list of (x, y, score) for matches above threshold.
///
/// When the `find_image_simd` feature is enabled and no mask is present,
/// this function uses SIMD-accelerated NCC computation for templates
/// with width >= 16 pixels.
///
/// This function is public for benchmarking purposes.
pub fn match_template_ncc(
    image: &GrayImage,
    template: &GrayImage,
    mask: Option<&GrayImage>,
    threshold: f64,
    stride: u32,
) -> Vec<(u32, u32, f64)> {
    let img_w = image.width();
    let img_h = image.height();
    let tpl_w = template.width();
    let tpl_h = template.height();

    if tpl_w > img_w || tpl_h > img_h {
        return Vec::new();
    }

    let stride = stride.max(1);

    // Precompute template statistics
    let tpl_stats = compute_template_stats(template, mask);

    if tpl_stats.std < f64::EPSILON || tpl_stats.pixel_count == 0 {
        return Vec::new();
    }

    let mut matches = Vec::new();

    let search_w = img_w - tpl_w + 1;
    let search_h = img_h - tpl_h + 1;

    // Determine whether to use SIMD path
    #[cfg(feature = "find_image_simd")]
    let use_simd = mask.is_none() && tpl_w >= 16;
    #[cfg(not(feature = "find_image_simd"))]
    let use_simd = false;

    // Iterate over search positions with stride
    let mut y = 0u32;
    while y < search_h {
        let mut x = 0u32;
        while x < search_w {
            let score = if use_simd {
                #[cfg(feature = "find_image_simd")]
                {
                    compute_ncc_at_simd(image, template, x, y, &tpl_stats)
                }
                #[cfg(not(feature = "find_image_simd"))]
                {
                    compute_ncc_at(image, template, mask, x, y, &tpl_stats)
                }
            } else {
                compute_ncc_at(image, template, mask, x, y, &tpl_stats)
            };

            if score >= threshold {
                matches.push((x, y, score));
            }

            x += stride;
        }
        y += stride;
    }

    matches
}

/// Precomputed template statistics for NCC matching.
/// Public for benchmarking purposes.
pub struct TemplateStats {
    /// Template mean pixel value.
    pub mean: f64,
    /// Template standard deviation.
    pub std: f64,
    /// Number of active pixels (respecting mask).
    pub pixel_count: usize,
}

/// Compute template mean, std deviation, and pixel count.
/// Public for benchmarking purposes.
pub fn compute_template_stats(template: &GrayImage, mask: Option<&GrayImage>) -> TemplateStats {
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    let mut count = 0usize;

    for (x, y, pixel) in template.enumerate_pixels() {
        let use_pixel = mask
            .map(|m| m.get_pixel(x.min(m.width() - 1), y.min(m.height() - 1)).0[0] > 128)
            .unwrap_or(true);

        if use_pixel {
            let val = pixel.0[0] as f64;
            sum += val;
            sum_sq += val * val;
            count += 1;
        }
    }

    if count == 0 {
        return TemplateStats {
            mean: 0.0,
            std: 0.0,
            pixel_count: 0,
        };
    }

    let mean = sum / count as f64;
    let variance = (sum_sq / count as f64) - (mean * mean);
    let std = variance.max(0.0).sqrt();

    TemplateStats {
        mean,
        std,
        pixel_count: count,
    }
}

/// Compute NCC score at a specific position (scalar version).
#[allow(clippy::too_many_arguments)]
fn compute_ncc_at(
    image: &GrayImage,
    template: &GrayImage,
    mask: Option<&GrayImage>,
    offset_x: u32,
    offset_y: u32,
    tpl_stats: &TemplateStats,
) -> f64 {
    let tpl_w = template.width();
    let tpl_h = template.height();

    // Compute image region statistics
    let mut img_sum = 0.0;
    let mut img_sum_sq = 0.0;
    let mut cross_sum = 0.0;

    for ty in 0..tpl_h {
        for tx in 0..tpl_w {
            let use_pixel = mask
                .map(|m| m.get_pixel(tx.min(m.width() - 1), ty.min(m.height() - 1)).0[0] > 128)
                .unwrap_or(true);

            if use_pixel {
                let img_val = image.get_pixel(offset_x + tx, offset_y + ty).0[0] as f64;
                let tpl_val = template.get_pixel(tx, ty).0[0] as f64;

                img_sum += img_val;
                img_sum_sq += img_val * img_val;
                cross_sum += img_val * tpl_val;
            }
        }
    }

    let count = tpl_stats.pixel_count as f64;
    let img_mean = img_sum / count;
    let img_variance = (img_sum_sq / count) - (img_mean * img_mean);
    let img_std = img_variance.max(0.0).sqrt();

    if img_std < f64::EPSILON {
        return 0.0;
    }

    // NCC = sum((I - mean_I) * (T - mean_T)) / (n * std_I * std_T)
    // Expanded: (sum(I*T) - n*mean_I*mean_T) / (n * std_I * std_T)
    let numerator = cross_sum - count * img_mean * tpl_stats.mean;
    let denominator = count * img_std * tpl_stats.std;

    if denominator < f64::EPSILON {
        return 0.0;
    }

    (numerator / denominator).clamp(-1.0, 1.0)
}

/// SIMD-accelerated NCC computation using the `wide` crate.
///
/// This version processes 8 pixels at a time using f32x8 SIMD vectors.
/// Only used when:
/// - The `find_image_simd` feature is enabled
/// - No mask is present (masks require per-pixel conditional logic)
/// - Template width >= 16 (to amortize SIMD overhead)
#[cfg(feature = "find_image_simd")]
#[allow(clippy::too_many_arguments)]
fn compute_ncc_at_simd(
    image: &GrayImage,
    template: &GrayImage,
    offset_x: u32,
    offset_y: u32,
    tpl_stats: &TemplateStats,
) -> f64 {
    use wide::f32x8;

    let tpl_w = template.width() as usize;
    let tpl_h = template.height() as usize;
    let img_stride = image.width() as usize;

    let mut img_sum_acc = f32x8::ZERO;
    let mut img_sum_sq_acc = f32x8::ZERO;
    let mut cross_sum_acc = f32x8::ZERO;

    // Scalar accumulators for remainder
    let mut img_sum_scalar = 0.0f32;
    let mut img_sum_sq_scalar = 0.0f32;
    let mut cross_sum_scalar = 0.0f32;

    let image_raw = image.as_raw();
    let template_raw = template.as_raw();

    for ty in 0..tpl_h {
        let img_row_start = (offset_y as usize + ty) * img_stride + offset_x as usize;
        let tpl_row_start = ty * tpl_w;

        let mut tx = 0usize;

        // Process 8 pixels at a time
        while tx + 8 <= tpl_w {
            // Load 8 image pixels
            let img_slice = &image_raw[img_row_start + tx..img_row_start + tx + 8];
            let img_vals = f32x8::new([
                img_slice[0] as f32,
                img_slice[1] as f32,
                img_slice[2] as f32,
                img_slice[3] as f32,
                img_slice[4] as f32,
                img_slice[5] as f32,
                img_slice[6] as f32,
                img_slice[7] as f32,
            ]);

            // Load 8 template pixels
            let tpl_slice = &template_raw[tpl_row_start + tx..tpl_row_start + tx + 8];
            let tpl_vals = f32x8::new([
                tpl_slice[0] as f32,
                tpl_slice[1] as f32,
                tpl_slice[2] as f32,
                tpl_slice[3] as f32,
                tpl_slice[4] as f32,
                tpl_slice[5] as f32,
                tpl_slice[6] as f32,
                tpl_slice[7] as f32,
            ]);

            img_sum_acc += img_vals;
            img_sum_sq_acc += img_vals * img_vals;
            cross_sum_acc += img_vals * tpl_vals;

            tx += 8;
        }

        // Handle remaining pixels (scalar)
        while tx < tpl_w {
            let img_val = image_raw[img_row_start + tx] as f32;
            let tpl_val = template_raw[tpl_row_start + tx] as f32;

            img_sum_scalar += img_val;
            img_sum_sq_scalar += img_val * img_val;
            cross_sum_scalar += img_val * tpl_val;

            tx += 1;
        }
    }

    // Reduce SIMD accumulators
    let img_sum_arr: [f32; 8] = img_sum_acc.into();
    let img_sum_sq_arr: [f32; 8] = img_sum_sq_acc.into();
    let cross_sum_arr: [f32; 8] = cross_sum_acc.into();

    let img_sum: f64 = img_sum_arr.iter().map(|&x| x as f64).sum::<f64>() + img_sum_scalar as f64;
    let img_sum_sq: f64 =
        img_sum_sq_arr.iter().map(|&x| x as f64).sum::<f64>() + img_sum_sq_scalar as f64;
    let cross_sum: f64 =
        cross_sum_arr.iter().map(|&x| x as f64).sum::<f64>() + cross_sum_scalar as f64;

    // Compute NCC
    let count = tpl_stats.pixel_count as f64;
    let img_mean = img_sum / count;
    let img_variance = (img_sum_sq / count) - (img_mean * img_mean);
    let img_std = img_variance.max(0.0).sqrt();

    if img_std < f64::EPSILON {
        return 0.0;
    }

    let numerator = cross_sum - count * img_mean * tpl_stats.mean;
    let denominator = count * img_std * tpl_stats.std;

    if denominator < f64::EPSILON {
        return 0.0;
    }

    (numerator / denominator).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
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
}

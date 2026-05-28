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
#[cfg(feature = "find_image_parallel")]
use crate::tools::find_image::parallel::get_thread_pool;
use crate::tools::find_image::params::{BoundingBox, MatchResult, Point, ScaleRange, SearchRegion};
#[cfg(feature = "find_image_simd")]
use crate::tools::find_image::simd::compute_ncc_at_simd;
use crate::tools::find_image::transform::{extract_region, resize_image, rotate_image};
use crate::tools::screenshot_cache::ScreenshotMetadata;
use image::GrayImage;
use std::borrow::Cow;

#[cfg(feature = "find_image_parallel")]
use rayon::prelude::*;

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

#[cfg(test)]
#[path = "algorithm_tests.rs"]
mod tests;

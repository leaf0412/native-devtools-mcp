//! SIMD-accelerated NCC kernel for `find_image` (feature `find_image_simd`).
//!
//! Lives next to the scalar `compute_ncc_at` in `algorithm.rs`; the
//! caller picks between them in `match_template_ncc` based on mask
//! presence and template width.

use crate::tools::find_image::algorithm::TemplateStats;
use image::GrayImage;
use wide::f32x8;

/// SIMD-accelerated NCC computation using the `wide` crate.
///
/// Processes 8 pixels at a time using f32x8 SIMD vectors. Only invoked when:
/// - The `find_image_simd` feature is enabled
/// - No mask is present (masks require per-pixel conditional logic)
/// - Template width >= 16 (to amortize SIMD overhead)
#[allow(clippy::too_many_arguments)]
pub(super) fn compute_ncc_at_simd(
    image: &GrayImage,
    template: &GrayImage,
    offset_x: u32,
    offset_y: u32,
    tpl_stats: &TemplateStats,
) -> f64 {
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

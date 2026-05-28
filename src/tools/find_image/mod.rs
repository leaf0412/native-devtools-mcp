//! Template matching tool for locating images within screenshots.
//!
//! This module implements the `find_image` MCP tool which uses normalized
//! cross-correlation (NCC) to find template images within screenshots.
//!
//! ## Performance Optimizations
//!
//! This module supports several optional performance optimizations:
//!
//! - **Algorithmic**: Dynamic downscaling in fast mode, early exit on high-confidence
//!   matches, and efficient scale loop termination.
//! - **Parallelism** (`find_image_parallel` feature): Uses Rayon to process
//!   scale/rotation combinations in parallel.
//! - **SIMD** (`find_image_simd` feature): Uses the `wide` crate for vectorized
//!   NCC computation on x86_64 and aarch64.
//!
//! ## File layout (post-T3 split)
//!
//! - `params`    — request/response/scale/region types and pure validators
//! - `source`    — image-source resolution seam (cache lookups + decode boundary)
//! - `transform` — pure pixel decode/extract/resize/rotate helpers
//! - `algorithm` — NCC matching, work items, parallel/SIMD entry points
//! - `nms`       — non-maximum suppression for overlapping detections
//! - `handler`   — MCP `ToolHandler` impl that wires schema → algorithm

mod algorithm;
mod handler;
mod nms;
mod params;
mod source;
mod transform;

// Public surface preserved by the T3 refactor.
// `algorithm` re-exports feed `benches/find_image_bench.rs`; the bin target
// does not consume them, so silence its unused_imports warning.
#[allow(unused_imports)]
pub use algorithm::{compute_template_stats, match_template_ncc, TemplateStats};
pub use handler::FindImage;

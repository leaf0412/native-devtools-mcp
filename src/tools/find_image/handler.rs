//! `find_image` MCP tool handler.
//!
//! The orchestration flow lives here:
//!   `execute()` → validate params → resolve slots through the source seam →
//!   `spawn_blocking(decode_and_match)` → `finalize` → `CallToolResult`.
//!
//! The matching CPU pipeline lives in `algorithm::run_matching`; the cache
//! lookups and decode rules live in `source` and `transform`. This file is
//! the only piece that knows about MCP-shaped types (`Tool`, `ToolHandler`,
//! `CallToolResult`).

use crate::tools::find_image::algorithm::{run_matching, MatchOutcome, MatchRequest};
use crate::tools::find_image::params::{
    normalize_rotation, validate_scale_range, FindImageParams, FindImageResponse, ModeDefaults,
};
use crate::tools::find_image::source::{
    resolve_slot, Caches, ImageSource, Miss, RawImageBytes, SlotInput, SlotKind,
};
use crate::tools::registry::{json_to_object, ToolContext, ToolHandler};
use crate::tools::screenshot_cache::ScreenshotMetadata;
use rmcp::model::{CallToolResult, Content, Tool};
use rmcp::Error as McpError;
use std::sync::Arc;

#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;

/// `find_image` MCP tool handler.
pub struct FindImage;

#[async_trait::async_trait]
impl ToolHandler for FindImage {
    fn name(&self) -> &'static str {
        "find_image"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "find_image",
            "Find a template image within a screenshot using template matching. Returns precise click coordinates for non-text UI elements like icons and shapes. Use screenshot_id from take_screenshot or provide screenshot_image_base64. Use template_id from load_image or provide template_image_base64.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "screenshot_id": {
                        "type": "string",
                        "description": "Screenshot ID from a previous take_screenshot call (preferred)"
                    },
                    "screenshot_image_base64": {
                        "type": "string",
                        "description": "Base64-encoded screenshot image (used if no screenshot_id)"
                    },
                    "template_id": {
                        "type": "string",
                        "description": "Image ID from a previous load_image call (preferred over template_image_base64)"
                    },
                    "template_image_base64": {
                        "type": "string",
                        "description": "Base64-encoded template image to find (used if no template_id)"
                    },
                    "mask_id": {
                        "type": "string",
                        "description": "Image ID from a previous load_image call for the mask"
                    },
                    "mask_image_base64": {
                        "type": "string",
                        "description": "Base64-encoded mask image (optional; white=match, black=ignore)"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["fast", "accurate"],
                        "description": "Matching mode: 'fast' (default) for quick searches, 'accurate' for thorough matching",
                        "default": "fast"
                    },
                    "threshold": {
                        "type": "number",
                        "description": "Minimum match score 0.0-1.0 (default: 0.75)"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum matches to return (default: 3 fast, 5 accurate)"
                    },
                    "scales": {
                        "type": "object",
                        "description": "Scale search range {min, max, step}",
                        "properties": {
                            "min": { "type": "number", "default": 0.8 },
                            "max": { "type": "number", "default": 1.2 },
                            "step": { "type": "number", "default": 0.1 }
                        }
                    },
                    "search_region": {
                        "type": "object",
                        "description": "Limit search to region {x, y, w, h} in screenshot pixels",
                        "properties": {
                            "x": { "type": "integer" },
                            "y": { "type": "integer" },
                            "w": { "type": "integer" },
                            "h": { "type": "integer" }
                        }
                    },
                    "stride": {
                        "type": "integer",
                        "description": "Search step size (default: 2 fast, 1 accurate)"
                    },
                    "rotations": {
                        "type": "array",
                        "items": { "type": "number" },
                        "description": "Rotations to try in degrees (only 0, 90, 180, 270 supported)"
                    },
                    "return_screen_coords": {
                        "type": "boolean",
                        "description": "Include screen coordinates for clicking (default: true)",
                        "default": true
                    }
                }
            }))),
        )
    }

    async fn call(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<CallToolResult, McpError> {
        let params: FindImageParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        let caches = Caches {
            screenshot: ctx.screenshot_cache.clone(),
            image: ctx.image_cache.clone(),
        };
        Ok(execute(params, caches).await)
    }
}

/// Inputs passed to the blocking decode + match phase. Carries the
/// already-resolved bytes for each slot plus the matching parameters.
struct BlockingInput {
    screenshot_bytes: Option<RawImageBytes>,
    template_bytes: Option<RawImageBytes>,
    mask_bytes: Option<RawImageBytes>,
    screenshot_meta: Option<ScreenshotMetadata>,
    params: FindImageParams,
    threshold: f64,
    max_results: usize,
    scales: crate::tools::find_image::params::ScaleRange,
    stride: u32,
    rotations: Vec<f64>,
    is_fast_mode: bool,
}

/// Resolve normalized rotations and a possible warning about unsupported angles.
fn normalize_rotations(raw: Vec<f64>) -> (Vec<f64>, Option<String>) {
    let mut normalized = Vec::new();
    let mut invalid = Vec::new();
    for r in raw {
        match normalize_rotation(r) {
            Some(exact) => {
                if !normalized.contains(&exact) {
                    normalized.push(exact);
                }
            }
            None => invalid.push(r),
        }
    }
    if normalized.is_empty() {
        normalized.push(0.0);
    }
    let warning = if invalid.is_empty() {
        None
    } else {
        Some(format!(
            "Unsupported rotation angles ignored (only 0, 90, 180, 270 supported): {:?}",
            invalid
        ))
    };
    (normalized, warning)
}

/// Merge a new warning into a rolling warning string, joining with ";".
fn merge_warning(current: Option<String>, addition: String) -> Option<String> {
    Some(current.map_or(addition.clone(), |w| format!("{}; {}", w, addition)))
}

/// Async resolve of screenshot bytes through the seam. An id miss is
/// converted into a warning + None so the base64 fallback (if any) and
/// the post-decode hard error still fire as before.
async fn resolve_screenshot(
    params: &FindImageParams,
    caches: &Caches,
    warning: &mut Option<String>,
) -> (Option<RawImageBytes>, Option<ScreenshotMetadata>) {
    let (id_bytes, meta) = match params.screenshot_id.as_ref() {
        Some(id) => match ImageSource::Screenshot(id.clone()).fetch(caches).await {
            Ok(resolved) => (Some(resolved.bytes), resolved.meta.screenshot),
            Err(Miss::Error(warn_text)) => {
                *warning = Some(warn_text);
                (None, None)
            }
            Err(Miss::FallbackWithWarning(..)) => unreachable!(
                "screenshot fetch never produces FallbackWithWarning at this call site"
            ),
        },
        None => (None, None),
    };
    let bytes = id_bytes.or_else(|| {
        params
            .screenshot_image_base64
            .clone()
            .map(RawImageBytes::Base64)
    });
    (bytes, meta)
}

/// Async resolve of a template-or-mask slot. Returns `Err` for a hard
/// error the caller must surface as `CallToolResult::error`.
async fn resolve_template_or_mask(
    primary_id: Option<String>,
    fallback_b64: Option<String>,
    kind: SlotKind,
    caches: &Caches,
    warning: &mut Option<String>,
) -> Result<Option<RawImageBytes>, String> {
    let slot = SlotInput {
        primary: primary_id.map(ImageSource::Image),
        fallback_b64: fallback_b64.map(ImageSource::Base64),
    };
    let (resolved, warn_msg) = resolve_slot(slot, kind, caches).await?;
    if let Some(msg) = warn_msg {
        *warning = merge_warning(warning.take(), msg);
    }
    Ok(resolved.map(|r| r.bytes))
}

/// Public orchestrator: validate, resolve via the seam, decode + match
/// on a blocking thread, finalize the response.
pub(super) async fn execute(params: FindImageParams, caches: Caches) -> CallToolResult {
    let defaults = ModeDefaults::for_mode(&params.mode);
    let mut warning: Option<String> = None;

    let threshold = params.threshold.unwrap_or(defaults.threshold);
    let max_results = params.max_results.unwrap_or(defaults.max_results);
    let scales = params.scales.clone().unwrap_or(defaults.scales);
    let stride = params.stride.unwrap_or(defaults.stride);
    let raw_rotations = params.rotations.clone().unwrap_or_else(|| vec![0.0]);

    if let Err(e) = validate_scale_range(&scales) {
        return CallToolResult::error(vec![Content::text(e)]);
    }

    let (rotations, rotation_warning) = normalize_rotations(raw_rotations);
    if let Some(rw) = rotation_warning {
        warning = merge_warning(warning, rw);
    }

    if let Some(region) = &params.search_region {
        if region.w == 0 || region.h == 0 {
            return CallToolResult::error(vec![Content::text(
                "search_region width and height must be positive",
            )]);
        }
    }

    // Resolve through the source seam. Lock order is preserved:
    // screenshot → template → mask.
    let (screenshot_bytes, screenshot_meta) =
        resolve_screenshot(&params, &caches, &mut warning).await;

    let template_bytes = match resolve_template_or_mask(
        params.template_id.clone(),
        params.template_image_base64.clone(),
        SlotKind::Template,
        &caches,
        &mut warning,
    )
    .await
    {
        Ok(b) => b,
        Err(e) => return CallToolResult::error(vec![Content::text(e)]),
    };

    let mask_bytes = match resolve_template_or_mask(
        params.mask_id.clone(),
        params.mask_image_base64.clone(),
        SlotKind::Mask,
        &caches,
        &mut warning,
    )
    .await
    {
        Ok(b) => b,
        Err(e) => return CallToolResult::error(vec![Content::text(e)]),
    };

    // Template-source guard (defence in depth; resolve_slot also signals).
    if params.template_id.is_none() && params.template_image_base64.is_none() {
        return CallToolResult::error(vec![Content::text(
            "Either template_id or template_image_base64 must be provided",
        )]);
    }

    let is_fast_mode = params.mode != "accurate";
    let blocking_input = BlockingInput {
        screenshot_bytes,
        template_bytes,
        mask_bytes,
        screenshot_meta,
        params,
        threshold,
        max_results,
        scales,
        stride,
        rotations,
        is_fast_mode,
    };

    let outcome = tokio::task::spawn_blocking(move || decode_and_match(blocking_input))
        .await
        .unwrap_or_else(|e| MatchOutcome::Error(format!("Task panicked: {}", e)));

    finalize(outcome, warning)
}

/// Decode each resolved RawImageBytes into a GrayImage and hand off to
/// `run_matching`. The "Either ... must be provided" / "Failed to decode
/// ..." literals are byte-identical with the pre-split code so external
/// observers see no message drift.
fn decode_and_match(input: BlockingInput) -> MatchOutcome {
    let BlockingInput {
        screenshot_bytes,
        template_bytes,
        mask_bytes,
        screenshot_meta,
        params,
        threshold,
        max_results,
        scales,
        stride,
        rotations,
        is_fast_mode,
    } = input;

    let (screenshot, screenshot_meta) = match screenshot_bytes {
        Some(RawImageBytes::Png(bytes)) => match RawImageBytes::Png(bytes).decode() {
            Ok(gray) => (gray, screenshot_meta),
            Err(e) => {
                return MatchOutcome::Error(format!("Failed to decode cached screenshot: {}", e))
            }
        },
        Some(RawImageBytes::Base64(b64)) => match RawImageBytes::Base64(b64).decode() {
            Ok(gray) => (gray, None),
            Err(e) => return MatchOutcome::Error(format!("Failed to decode screenshot: {}", e)),
        },
        None => {
            return MatchOutcome::Error(
                "Either screenshot_id or screenshot_image_base64 must be provided".to_string(),
            );
        }
    };

    let template = match template_bytes {
        Some(RawImageBytes::Png(bytes)) => match RawImageBytes::Png(bytes).decode() {
            Ok(img) => img,
            Err(e) => return MatchOutcome::Error(format!("Failed to decode cached template: {}", e)),
        },
        Some(RawImageBytes::Base64(b64)) => match RawImageBytes::Base64(b64).decode() {
            Ok(img) => img,
            Err(e) => return MatchOutcome::Error(format!("Failed to decode template image: {}", e)),
        },
        None => {
            return MatchOutcome::Error(
                "Either template_id or template_image_base64 must be provided".to_string(),
            );
        }
    };

    let mask = match mask_bytes {
        Some(RawImageBytes::Png(bytes)) => match RawImageBytes::Png(bytes).decode() {
            Ok(img) => Some(img),
            Err(e) => return MatchOutcome::Error(format!("Failed to decode cached mask: {}", e)),
        },
        Some(RawImageBytes::Base64(b64)) => match RawImageBytes::Base64(b64).decode() {
            Ok(img) => Some(img),
            Err(e) => return MatchOutcome::Error(format!("Failed to decode mask image: {}", e)),
        },
        None => None,
    };

    run_matching(MatchRequest {
        screenshot,
        template,
        mask,
        screenshot_meta,
        search_region: params.search_region.clone(),
        threshold,
        max_results,
        scales,
        stride,
        rotations,
        return_screen_coords: params.return_screen_coords,
        is_fast_mode,
    })
}

/// Wrap a `MatchOutcome` into a serialized `CallToolResult` carrying
/// any accumulated warnings.
fn finalize(outcome: MatchOutcome, warning: Option<String>) -> CallToolResult {
    match outcome {
        MatchOutcome::Success(matches) => {
            let response = FindImageResponse { matches, warning };
            match serde_json::to_string_pretty(&response) {
                Ok(json) => CallToolResult::success(vec![Content::text(json)]),
                Err(e) => CallToolResult::error(vec![Content::text(format!(
                    "Failed to serialize response: {}",
                    e
                ))]),
            }
        }
        MatchOutcome::Error(e) => CallToolResult::error(vec![Content::text(e)]),
    }
}

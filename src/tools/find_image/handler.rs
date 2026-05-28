//! `find_image` MCP tool handler — schema + dispatch into the algorithm.
//!
//! The schema JSON, tool description, and parameter parsing live here; the
//! actual matching pipeline is in `algorithm::find_image`.

use crate::tools::find_image::algorithm::find_image;
use crate::tools::find_image::params::FindImageParams;
use crate::tools::find_image::source::Caches;
use crate::tools::registry::{json_to_object, ToolContext, ToolHandler};
use rmcp::model::{CallToolResult, Tool};
use rmcp::Error as McpError;
use std::sync::Arc;

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
        Ok(find_image(params, caches).await)
    }
}

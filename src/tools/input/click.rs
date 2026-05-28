//! `click` tool: resolve one coordinate variant and click at screen coords.

use super::click_variant::{select_click_variant, ClickVariant};
use super::{check_permission, run_input};
use crate::platform::{display, input};
use crate::tools::registry::{json_to_object, ToolContext, ToolHandler};
use rmcp::model::{CallToolResult, Content};
use rmcp::{model::Tool, Error as McpError};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct ClickParams {
    /// Screen X coordinate (required unless using window-relative)
    pub x: Option<f64>,
    /// Screen Y coordinate (required unless using window-relative)
    pub y: Option<f64>,

    /// Window-relative X coordinate
    pub window_x: Option<f64>,
    /// Window-relative Y coordinate
    pub window_y: Option<f64>,
    /// Window ID for window-relative coordinates
    pub window_id: Option<u32>,

    /// Screenshot pixel X coordinate
    pub screenshot_x: Option<f64>,
    /// Screenshot pixel Y coordinate
    pub screenshot_y: Option<f64>,
    /// Screenshot origin X coordinate in screen space
    pub screenshot_origin_x: Option<f64>,
    /// Screenshot origin Y coordinate in screen space
    pub screenshot_origin_y: Option<f64>,
    /// Backing scale factor used for the screenshot
    pub screenshot_scale: Option<f64>,
    /// Window ID that the screenshot was taken from (for scaling)
    pub screenshot_window_id: Option<u32>,

    /// Mouse button: "left" (default), "right", or "center"
    #[serde(default)]
    pub button: Option<String>,

    /// Number of clicks (1 for single, 2 for double)
    #[serde(default = "default_click_count")]
    pub click_count: u32,
}

fn default_click_count() -> u32 {
    1
}

pub async fn click(params: ClickParams) -> CallToolResult {
    if let Some(err) = check_permission() {
        return err;
    }

    // Parse button
    let button = match params.button.as_deref() {
        Some("right") => input::MouseButton::Right,
        Some("center") | Some("middle") => input::MouseButton::Center,
        _ => input::MouseButton::Left,
    };

    // Enforce the oneOf contract: exactly one variant's fields may be
    // present. Clients that don't validate schemas used to silently pick
    // the first complete branch on a mixed payload — now we reject it.
    let variant = match select_click_variant(&params) {
        Ok(v) => v,
        Err(msg) => return CallToolResult::error(vec![Content::text(msg)]),
    };

    // Resolve coordinates based on the validated variant.
    let (x, y) = match variant {
        ClickVariant::Screen => (
            params
                .x
                .expect("select_click_variant guarantees x is Some for Screen"),
            params
                .y
                .expect("select_click_variant guarantees y is Some for Screen"),
        ),
        ClickVariant::WindowRelative => {
            let wx = params.window_x.expect("window_x guaranteed present");
            let wy = params.window_y.expect("window_y guaranteed present");
            let window_id = params.window_id.expect("window_id guaranteed present");
            let window = match crate::platform::find_window_by_id(window_id) {
                Ok(Some(w)) => w,
                Ok(None) => {
                    return CallToolResult::error(vec![Content::text(format!(
                        "Window {} not found",
                        window_id
                    ))])
                }
                Err(e) => return CallToolResult::error(vec![Content::text(e)]),
            };
            let bounds = display::WindowBounds {
                x: window.bounds.x,
                y: window.bounds.y,
            };
            display::window_to_screen(&bounds, wx, wy)
        }
        ClickVariant::ScreenshotPixels => {
            let px = params
                .screenshot_x
                .expect("screenshot_x guaranteed present");
            let py = params
                .screenshot_y
                .expect("screenshot_y guaranteed present");
            let origin_x = params
                .screenshot_origin_x
                .expect("screenshot_origin_x guaranteed present");
            let origin_y = params
                .screenshot_origin_y
                .expect("screenshot_origin_y guaranteed present");
            let scale = params
                .screenshot_scale
                .expect("screenshot_scale guaranteed present");
            let bounds = display::WindowBounds {
                x: origin_x,
                y: origin_y,
            };
            display::screenshot_to_screen(&bounds, scale, px, py)
        }
        ClickVariant::ScreenshotPixelsLegacy => {
            let px = params
                .screenshot_x
                .expect("screenshot_x guaranteed present");
            let py = params
                .screenshot_y
                .expect("screenshot_y guaranteed present");
            let window_id = params
                .screenshot_window_id
                .expect("screenshot_window_id guaranteed present");
            let window = match crate::platform::find_window_by_id(window_id) {
                Ok(Some(w)) => w,
                Ok(None) => {
                    return CallToolResult::error(vec![Content::text(format!(
                        "Window {} not found",
                        window_id
                    ))])
                }
                Err(e) => return CallToolResult::error(vec![Content::text(e)]),
            };
            let bounds = display::WindowBounds {
                x: window.bounds.x,
                y: window.bounds.y,
            };
            // macOS: screencapture captures in physical (Retina) pixels, need scale factor.
            // Windows: BitBlt captures in logical coordinates, scale is always 1.0.
            #[cfg(target_os = "macos")]
            let scale = display::backing_scale_for_point(window.bounds.x, window.bounds.y);
            #[cfg(target_os = "windows")]
            let scale = 1.0;
            display::screenshot_to_screen(&bounds, scale, px, py)
        }
    };

    let click_count = params.click_count;
    run_input(
        move || input::click(x, y, button, click_count),
        format!("Clicked at ({:.0}, {:.0})", x, y),
        "Click failed",
    )
    .await
}

/// `click` MCP tool handler.
pub struct Click;

#[async_trait::async_trait]
impl ToolHandler for Click {
    fn name(&self) -> &'static str {
        "click"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "click",
            "Click at screen coordinates. Pass exactly one coordinate variant — the runtime \
             rejects mixes. Variants: \
             (1) 'screenshot-pixels' (PREFERRED after take_screenshot) — screenshot_x, \
             screenshot_y, screenshot_origin_x, screenshot_origin_y, screenshot_scale from \
             take_screenshot metadata; \
             (2) 'screen' — absolute screen x, y (use with find_text results); \
             (3) 'window-relative' — window_x, window_y, window_id from list_windows; \
             (4) 'screenshot-pixels-legacy' (DEPRECATED) — screenshot_x, screenshot_y, \
             screenshot_window_id. \
             Works with any app (egui, Electron, etc.). Requires Accessibility permission on macOS.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "x": {
                        "type": "number",
                        "description": "[screen variant] Absolute screen X coordinate. Use with find_text results."
                    },
                    "y": {
                        "type": "number",
                        "description": "[screen variant] Absolute screen Y coordinate. Use with find_text results."
                    },
                    "window_x": {
                        "type": "number",
                        "description": "[window-relative variant] X relative to window top-left. Pair with window_y and window_id."
                    },
                    "window_y": {
                        "type": "number",
                        "description": "[window-relative variant] Y relative to window top-left. Pair with window_x and window_id."
                    },
                    "window_id": {
                        "type": "integer",
                        "description": "[window-relative variant] Target window ID (from list_windows)."
                    },
                    "screenshot_x": {
                        "type": "number",
                        "description": "[screenshot-pixels / screenshot-pixels-legacy] X pixel inside the screenshot image."
                    },
                    "screenshot_y": {
                        "type": "number",
                        "description": "[screenshot-pixels / screenshot-pixels-legacy] Y pixel inside the screenshot image."
                    },
                    "screenshot_origin_x": {
                        "type": "number",
                        "description": "[screenshot-pixels, PREFERRED] screenshot_origin_x from take_screenshot metadata."
                    },
                    "screenshot_origin_y": {
                        "type": "number",
                        "description": "[screenshot-pixels, PREFERRED] screenshot_origin_y from take_screenshot metadata."
                    },
                    "screenshot_scale": {
                        "type": "number",
                        "description": "[screenshot-pixels, PREFERRED] screenshot_scale from take_screenshot metadata."
                    },
                    "screenshot_window_id": {
                        "type": "integer",
                        "description": "[screenshot-pixels-legacy, DEPRECATED] Window ID the screenshot was taken from. Prefer screenshot_origin_x/y + screenshot_scale."
                    },
                    "button": {
                        "type": "string",
                        "enum": ["left", "right", "center"],
                        "description": "Mouse button (default: left)"
                    },
                    "click_count": {
                        "type": "integer",
                        "description": "Number of clicks (1=single, 2=double)",
                        "default": 1
                    }
                }
            }))),
        )
    }

    async fn call(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<CallToolResult, McpError> {
        let params: ClickParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(click(params).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_click_params_accepts_valid_screenshot_pixels_variant() {
        let params: ClickParams = serde_json::from_value(serde_json::json!({
            "screenshot_x": 10.0,
            "screenshot_y": 20.0,
            "screenshot_origin_x": 100.0,
            "screenshot_origin_y": 200.0,
            "screenshot_scale": 2.0,
        }))
        .expect("valid screenshot-pixels payload should deserialize");
        assert_eq!(params.screenshot_x, Some(10.0));
        assert_eq!(params.screenshot_scale, Some(2.0));
    }

    #[test]
    fn test_click_params_accepts_valid_screen_variant() {
        let params: ClickParams = serde_json::from_value(serde_json::json!({
            "x": 500.0,
            "y": 400.0,
        }))
        .expect("valid screen payload should deserialize");
        assert_eq!(params.x, Some(500.0));
        assert_eq!(params.y, Some(400.0));
    }
}

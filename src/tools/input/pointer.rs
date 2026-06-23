//! Pointer and keyboard tools: move_mouse, drag, scroll, type_text, press_key.

use super::{check_permission, run_input};
use crate::platform::input;
use crate::tools::registry::{json_to_object, ToolContext, ToolHandler};
use rmcp::model::{CallToolResult, Content};
use rmcp::{model::Tool, Error as McpError};
use serde::Deserialize;
use std::sync::Arc;

// ============================================================================
// Move Mouse
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct MoveMouseParams {
    /// Screen X coordinate
    pub x: f64,
    /// Screen Y coordinate
    pub y: f64,
}

pub async fn move_mouse(params: MoveMouseParams) -> CallToolResult {
    if let Some(err) = check_permission() {
        return err;
    }

    let (x, y) = (params.x, params.y);
    run_input(
        move || input::move_mouse(x, y),
        format!("Moved mouse to ({:.0}, {:.0})", x, y),
        "Move failed",
    )
    .await
}

// ============================================================================
// Drag
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct DragParams {
    /// Start X coordinate
    pub start_x: f64,
    /// Start Y coordinate
    pub start_y: f64,
    /// End X coordinate
    pub end_x: f64,
    /// End Y coordinate
    pub end_y: f64,
    /// Mouse button: "left" (default), "right", or "center"
    #[serde(default)]
    pub button: Option<String>,
}

pub async fn drag(params: DragParams) -> CallToolResult {
    if let Some(err) = check_permission() {
        return err;
    }

    let button = match params.button.as_deref() {
        Some("right") => input::MouseButton::Right,
        Some("center") | Some("middle") => input::MouseButton::Center,
        _ => input::MouseButton::Left,
    };

    let (start_x, start_y, end_x, end_y) =
        (params.start_x, params.start_y, params.end_x, params.end_y);
    run_input(
        move || input::drag(start_x, start_y, end_x, end_y, button),
        format!(
            "Dragged from ({:.0}, {:.0}) to ({:.0}, {:.0})",
            start_x, start_y, end_x, end_y
        ),
        "Drag failed",
    )
    .await
}

// ============================================================================
// Scroll
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct ScrollParams {
    /// X coordinate to scroll at
    pub x: f64,
    /// Y coordinate to scroll at
    pub y: f64,
    /// Horizontal scroll delta (positive = right)
    #[serde(default)]
    pub delta_x: i32,
    /// Vertical scroll delta (positive = down, negative = up)
    pub delta_y: i32,
}

pub async fn scroll(params: ScrollParams) -> CallToolResult {
    if let Some(err) = check_permission() {
        return err;
    }

    let (x, y, delta_x, delta_y) = (params.x, params.y, params.delta_x, params.delta_y);
    run_input(
        move || input::scroll(x, y, delta_x, delta_y),
        format!(
            "Scrolled at ({:.0}, {:.0}) by ({}, {})",
            x, y, delta_x, delta_y
        ),
        "Scroll failed",
    )
    .await
}

// ============================================================================
// Type Text
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct TypeTextParams {
    /// Text to type
    pub text: String,

    /// When true, deliver keystrokes via CGEventPostToPid without moving
    /// cursor or stealing focus. Requires `app_name`. macOS only.
    #[serde(default)]
    pub background: bool,
    /// Target app name for background typing.
    #[serde(default)]
    pub app_name: Option<String>,
}

pub async fn type_text(params: TypeTextParams) -> CallToolResult {
    if let Some(err) = check_permission() {
        return err;
    }

    let char_count = params.text.chars().count();
    let text = params.text;
    let background = params.background;

    #[cfg(target_os = "macos")]
    if background {
        let app = match &params.app_name {
            Some(name) => name.clone(),
            None => {
                return CallToolResult::error(vec![Content::text(
                    "background type_text requires app_name to resolve the target process",
                )])
            }
        };
        let windows = match crate::macos::window::find_windows_by_app(&app) {
            Ok(w) => w,
            Err(e) => {
                return CallToolResult::error(vec![Content::text(format!(
                    "Failed to find windows for '{}': {}", app, e
                ))])
            }
        };
        let pid = match windows.first() {
            Some(w) => w.owner_pid as i32,
            None => {
                return CallToolResult::error(vec![Content::text(format!(
                    "No window found for app '{}'", app
                ))])
            }
        };

        let mut errors = Vec::new();
        for ch in text.chars() {
            if let Err(e) = crate::macos::bg_click::post_key_event_unicode(pid, ch) {
                errors.push(e);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !errors.is_empty() {
            return CallToolResult::error(vec![Content::text(format!(
                "Background type errors: {}", errors.join(", ")
            ))]);
        }
        return CallToolResult::success(vec![Content::text(format!(
            "Background typed {} characters on '{}'",
            char_count, app
        ))]);
    }

    run_input(
        move || input::type_text(&text),
        format!("Typed {} characters", char_count),
        "Type failed",
    )
    .await
}

// ============================================================================
// Press Key
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct PressKeyParams {
    /// Key to press (e.g., "return", "tab", "a", "f1")
    pub key: String,
    /// Modifier keys: "shift", "control", "option", "command"
    #[serde(default)]
    pub modifiers: Vec<String>,
    /// When true, deliver via CGEventPostToPid in background. Requires app_name. macOS only.
    #[serde(default)]
    pub background: bool,
    /// Target app name for background key press.
    #[serde(default)]
    pub app_name: Option<String>,
}

pub async fn press_key(params: PressKeyParams) -> CallToolResult {
    if let Some(err) = check_permission() {
        return err;
    }

    let key_desc = if params.modifiers.is_empty() {
        params.key.clone()
    } else {
        format!("{}+{}", params.modifiers.join("+"), params.key)
    };

    let background = params.background;

    #[cfg(target_os = "macos")]
    if background {
        let app = match &params.app_name {
            Some(name) => name.clone(),
            None => {
                return CallToolResult::error(vec![Content::text(
                    "background press_key requires app_name to resolve the target process",
                )])
            }
        };
        let windows = match crate::macos::window::find_windows_by_app(&app) {
            Ok(w) => w,
            Err(e) => {
                return CallToolResult::error(vec![Content::text(format!(
                    "Failed to find windows for '{}': {}", app, e
                ))])
            }
        };
        let pid = match windows.first() {
            Some(w) => w.owner_pid as i32,
            None => {
                return CallToolResult::error(vec![Content::text(format!(
                    "No window found for app '{}'", app
                ))])
            }
        };

        let keycode = match crate::macos::input::key_name_to_code(&params.key) {
            Some(k) => k,
            None => {
                return CallToolResult::error(vec![Content::text(format!("Unknown key: {}", params.key))])
            }
        };

        // Build modifier flags
        let mut flags: u64 = 0;
        for m in &params.modifiers {
            match m.to_lowercase().as_str() {
                "shift" => flags |= 0x0002,   // kCGEventFlagMaskShift
                "control" | "ctrl" => flags |= 0x0004,  // kCGEventFlagMaskControl
                "option" | "alt" => flags |= 0x0008,    // kCGEventFlagMaskAlternate
                "command" | "cmd" => flags |= 0x0010,   // kCGEventFlagMaskCommand
                _ => {}
            }
        }

        match crate::macos::bg_click::post_key_event_bg(pid, keycode, flags) {
            Ok(()) => {
                return CallToolResult::success(vec![Content::text(format!(
                    "Background pressed {} on '{}'", key_desc, app
                ))]);
            }
            Err(e) => {
                return CallToolResult::error(vec![Content::text(format!(
                    "Background key press failed: {}", e
                ))]);
            }
        }
    }

    let key = params.key;
    let modifiers = params.modifiers;
    run_input(
        move || input::press_key(&key, &modifiers),
        format!("Pressed {}", key_desc),
        "Key press failed",
    )
    .await
}

/// `move_mouse` MCP tool handler.
pub struct MoveMouse;

#[async_trait::async_trait]
impl ToolHandler for MoveMouse {
    fn name(&self) -> &'static str {
        "move_mouse"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "move_mouse",
            "Move mouse cursor to screen coordinates. Requires Accessibility permission on macOS.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["x", "y"],
                "properties": {
                    "x": {
                        "type": "number",
                        "description": "Screen X coordinate"
                    },
                    "y": {
                        "type": "number",
                        "description": "Screen Y coordinate"
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
        let params: MoveMouseParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(move_mouse(params).await)
    }
}

/// `drag` MCP tool handler.
pub struct Drag;

#[async_trait::async_trait]
impl ToolHandler for Drag {
    fn name(&self) -> &'static str {
        "drag"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "drag",
            "Drag from one point to another. Requires Accessibility permission on macOS.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["start_x", "start_y", "end_x", "end_y"],
                "properties": {
                    "start_x": {
                        "type": "number",
                        "description": "Start X coordinate"
                    },
                    "start_y": {
                        "type": "number",
                        "description": "Start Y coordinate"
                    },
                    "end_x": {
                        "type": "number",
                        "description": "End X coordinate"
                    },
                    "end_y": {
                        "type": "number",
                        "description": "End Y coordinate"
                    },
                    "button": {
                        "type": "string",
                        "enum": ["left", "right", "center"],
                        "description": "Mouse button (default: left)"
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
        let params: DragParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(drag(params).await)
    }
}

/// `scroll` MCP tool handler.
pub struct Scroll;

#[async_trait::async_trait]
impl ToolHandler for Scroll {
    fn name(&self) -> &'static str {
        "scroll"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "scroll",
            "Scroll at a position. Requires Accessibility permission on macOS.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["x", "y", "delta_y"],
                "properties": {
                    "x": {
                        "type": "number",
                        "description": "Screen X coordinate to scroll at"
                    },
                    "y": {
                        "type": "number",
                        "description": "Screen Y coordinate to scroll at"
                    },
                    "delta_x": {
                        "type": "integer",
                        "description": "Horizontal scroll amount (positive=right)",
                        "default": 0
                    },
                    "delta_y": {
                        "type": "integer",
                        "description": "Vertical scroll amount (negative=up, positive=down)"
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
        let params: ScrollParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(scroll(params).await)
    }
}

/// `type_text` MCP tool handler.
pub struct TypeText;

#[async_trait::async_trait]
impl ToolHandler for TypeText {
    fn name(&self) -> &'static str {
        "type_text"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "type_text",
            "Type text at the current cursor position. Works with any app. Requires Accessibility permission on macOS. LAST RESORT without background=true: prefer ax_set_value for native macOS apps or cdp_fill for browsers first. Set background=true with app_name for cursor-free, focus-preserving typing via CGEventPostToPid — use this instead of a plain call when escalating from ax_set_value's not_dispatchable.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Text to type"
                    },
                    "background": {
                        "type": "boolean",
                        "description": "When true, deliver keystrokes via CGEventPostToPid without moving cursor or stealing focus. Requires app_name. macOS only.",
                        "default": false
                    },
                    "app_name": {
                        "type": "string",
                        "description": "Target app name for background typing."
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
        let params: TypeTextParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(type_text(params).await)
    }
}

/// `press_key` MCP tool handler.
pub struct PressKey;

#[async_trait::async_trait]
impl ToolHandler for PressKey {
    fn name(&self) -> &'static str {
        "press_key"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "press_key",
            "Press a key combination. Works with any app. Requires Accessibility permission on macOS. LAST RESORT without background=true: prefer ax_click/ax_set_value/ax_select or cdp_press_key first. Set background=true with app_name for cursor-free, focus-preserving key delivery via CGEventPostToPid — use this instead of a plain call when escalating from an AX not_dispatchable result.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["key"],
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Key to press (e.g., 'return', 'tab', 'escape', 'a', 'f1', 'left', 'up')"
                    },
                    "modifiers": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Modifier keys: 'shift', 'control', 'option', 'command'",
                        "default": []
                    },
                    "background": {
                        "type": "boolean",
                        "description": "When true, deliver key via CGEventPostToPid without moving cursor or stealing focus. Requires app_name. macOS only.",
                        "default": false
                    },
                    "app_name": {
                        "type": "string",
                        "description": "Target app name for background key press."
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
        let params: PressKeyParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(press_key(params).await)
    }
}

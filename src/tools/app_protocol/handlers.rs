// ============================================================================
// MCP tool handlers. Each wraps the free function above with its name, schema,
// and availability. `app_connect` is always visible (so the user can connect);
// the others are gated `WhenAppConnected`, mirroring `get_app_connect_tool` /
// `get_app_tools` + the connection gate in `get_tools`. Schema JSON moved
// verbatim from the deleted getters; call bodies copied verbatim from the
// deleted `call_tool` arms, with `ctx.app_client` / `ctx.peer` replacing
// `self.app_client` / `context.peer`.
// ============================================================================

use super::ops::*;
use crate::tools::registry::{json_to_object, Availability, ToolContext, ToolHandler};
use rmcp::model::{CallToolResult, Tool};
use rmcp::Error as McpError;
use std::sync::Arc;

/// `app_connect` — always visible so a disconnected client can initiate a
/// connection. Fires `notify_tool_list_changed` (inside the free function) so
/// the now-connected app's tools become visible.
pub struct AppConnect;

#[async_trait::async_trait]
impl ToolHandler for AppConnect {
    fn name(&self) -> &'static str {
        "app_connect"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "app_connect",
            "Connect to an app's debug server via WebSocket. The app must have AppDebugKit embedded.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "WebSocket URL (e.g., ws://127.0.0.1:9222)"
                    },
                    "expected_bundle_id": {
                        "type": "string",
                        "description": "Expected bundle ID (e.g., com.example.MyApp). Connection fails if mismatch."
                    },
                    "expected_app_name": {
                        "type": "string",
                        "description": "Expected app name (case-insensitive). Connection fails if mismatch."
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
        let params: AppConnectParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        let result = app_connect(params, ctx.app_client.clone(), ctx.peer.clone()).await;
        Ok(result)
    }
}

/// `app_disconnect` — visible only while connected. Fires
/// `notify_tool_list_changed` (inside the free function) so the app tools
/// disappear once the connection closes.
pub struct AppDisconnect;

#[async_trait::async_trait]
impl ToolHandler for AppDisconnect {
    fn name(&self) -> &'static str {
        "app_disconnect"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAppConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "app_disconnect",
            "Disconnect from the app's debug server.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    async fn call(
        &self,
        _args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<CallToolResult, McpError> {
        let result = app_disconnect(ctx.app_client.clone(), ctx.peer.clone()).await;
        Ok(result)
    }
}

/// `app_get_info` — visible only while connected.
pub struct AppGetInfo;

#[async_trait::async_trait]
impl ToolHandler for AppGetInfo {
    fn name(&self) -> &'static str {
        "app_get_info"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAppConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "app_get_info",
            "Get runtime info from the connected app (name, bundle ID, version, etc.).",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    async fn call(
        &self,
        _args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<CallToolResult, McpError> {
        Ok(app_get_info(ctx.app_client.clone()).await)
    }
}

/// `app_get_tree` — visible only while connected.
pub struct AppGetTree;

#[async_trait::async_trait]
impl ToolHandler for AppGetTree {
    fn name(&self) -> &'static str {
        "app_get_tree"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAppConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "app_get_tree",
            "Get the view hierarchy from the connected app.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "depth": {
                        "type": "integer",
                        "description": "Max depth to traverse (-1 for unlimited)",
                        "default": 5
                    },
                    "root_id": {
                        "type": "string",
                        "description": "Element ID to start from (optional, defaults to key window)"
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
        let params: AppGetTreeParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(app_get_tree(params, ctx.app_client.clone()).await)
    }
}

/// `app_get_element` — visible only while connected.
pub struct AppGetElement;

#[async_trait::async_trait]
impl ToolHandler for AppGetElement {
    fn name(&self) -> &'static str {
        "app_get_element"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAppConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "app_get_element",
            "Get detailed information about an element by ID.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["element_id"],
                "properties": {
                    "element_id": {
                        "type": "string",
                        "description": "Element ID to get details for"
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
        let params: AppGetElementParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(app_get_element(params, ctx.app_client.clone()).await)
    }
}

/// `app_list_windows` — visible only while connected.
pub struct AppListWindows;

#[async_trait::async_trait]
impl ToolHandler for AppListWindows {
    fn name(&self) -> &'static str {
        "app_list_windows"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAppConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "app_list_windows",
            "List all windows in the connected app.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    async fn call(
        &self,
        _args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<CallToolResult, McpError> {
        Ok(app_list_windows(ctx.app_client.clone()).await)
    }
}

/// `app_focus_window` — visible only while connected.
pub struct AppFocusWindow;

#[async_trait::async_trait]
impl ToolHandler for AppFocusWindow {
    fn name(&self) -> &'static str {
        "app_focus_window"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAppConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "app_focus_window",
            "Focus a window in the connected app (make it key and main window).",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["window_id"],
                "properties": {
                    "window_id": {
                        "type": "string",
                        "description": "Window ID to focus (e.g., 'window-1')"
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
        let params: AppFocusWindowParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(app_focus_window(params, ctx.app_client.clone()).await)
    }
}

/// `app_query` — visible only while connected.
pub struct AppQuery;

#[async_trait::async_trait]
impl ToolHandler for AppQuery {
    fn name(&self) -> &'static str {
        "app_query"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAppConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "app_query",
            "Find elements matching a CSS-like selector in the connected app.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["selector"],
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "CSS-like selector (#id, .ClassName, [prop=value])"
                    },
                    "all": {
                        "type": "boolean",
                        "description": "Return all matches (default: first only)",
                        "default": false
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
        let params: AppQueryParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(app_query(params, ctx.app_client.clone()).await)
    }
}

/// `app_click` — visible only while connected.
pub struct AppClick;

#[async_trait::async_trait]
impl ToolHandler for AppClick {
    fn name(&self) -> &'static str {
        "app_click"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAppConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "app_click",
            "Click an element in the connected app by ID.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["element_id"],
                "properties": {
                    "element_id": {
                        "type": "string",
                        "description": "Element ID to click"
                    },
                    "click_count": {
                        "type": "integer",
                        "description": "Number of clicks (1 for single, 2 for double)",
                        "default": 1
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
        let params: AppClickParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(app_click(params, ctx.app_client.clone()).await)
    }
}

/// `app_type` — visible only while connected.
pub struct AppType;

#[async_trait::async_trait]
impl ToolHandler for AppType {
    fn name(&self) -> &'static str {
        "app_type"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAppConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "app_type",
            "Type text into an element in the connected app.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Text to type"
                    },
                    "element_id": {
                        "type": "string",
                        "description": "Element ID to type into (uses focused element if omitted)"
                    },
                    "clear_first": {
                        "type": "boolean",
                        "description": "Clear existing text first",
                        "default": false
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
        let params: AppTypeParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(app_type(params, ctx.app_client.clone()).await)
    }
}

/// `app_press_key` — visible only while connected.
pub struct AppPressKey;

#[async_trait::async_trait]
impl ToolHandler for AppPressKey {
    fn name(&self) -> &'static str {
        "app_press_key"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAppConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "app_press_key",
            "Press a key or key combination in the connected app.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["key"],
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Key to press (e.g., 'Return', 'Tab', 'Escape')"
                    },
                    "modifiers": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Modifier keys: 'shift', 'control', 'option', 'command'",
                        "default": []
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
        let params: AppPressKeyParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(app_press_key(params, ctx.app_client.clone()).await)
    }
}

/// `app_focus` — visible only while connected.
pub struct AppFocus;

#[async_trait::async_trait]
impl ToolHandler for AppFocus {
    fn name(&self) -> &'static str {
        "app_focus"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAppConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "app_focus",
            "Focus an element in the connected app (make it first responder).",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["element_id"],
                "properties": {
                    "element_id": {
                        "type": "string",
                        "description": "Element ID to focus"
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
        let params: AppFocusParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(app_focus(params, ctx.app_client.clone()).await)
    }
}

/// `app_screenshot` — visible only while connected.
pub struct AppScreenshot;

#[async_trait::async_trait]
impl ToolHandler for AppScreenshot {
    fn name(&self) -> &'static str {
        "app_screenshot"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAppConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "app_screenshot",
            "Take a screenshot of an element or window in the connected app.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "element_id": {
                        "type": "string",
                        "description": "Element ID to capture (whole window if omitted)"
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
        let params: AppScreenshotParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(app_screenshot(params, ctx.app_client.clone()).await)
    }
}

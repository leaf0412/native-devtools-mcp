//! `ToolHandler` implementations for the CDP page/connection tools.
//!
//! Each handler wraps one CDP tool with its name, schema, and call body, moved
//! verbatim from the `server.rs` getters (`get_cdp_connect_tool` /
//! `get_cdp_tools`) and `call_tool` match arms. `self.cdp_client` becomes
//! `ctx.cdp_client`. CDP tools are always listed (the trait default
//! `Availability::Always`), independent of connection state — connect/disconnect
//! deliberately do NOT fire `notify_tool_list_changed`, keeping prompt caches
//! stable. The whole module is gated `#[cfg(feature = "cdp")]` at its `mod`
//! declaration, so no per-item `cfg` is needed here.

use crate::tools::registry::{
    json_to_object, parse_string_field, parse_xy, ToolContext, ToolHandler,
};
use rmcp::model::{CallToolResult, Content, Tool};
use rmcp::Error as McpError;
use serde_json::Value;
use std::sync::Arc;

/// `cdp_connect` — always visible so a disconnected client can connect. Does
/// not fire `notify_tool_list_changed`: CDP tools are always listed.
pub struct CdpConnect;

#[async_trait::async_trait]
impl ToolHandler for CdpConnect {
    fn name(&self) -> &'static str {
        "cdp_connect"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_connect",
            "Connect to a Chrome or Electron app via its remote debugging port. The app must be launched with --remote-debugging-port=PORT and --user-data-dir=PATH (Chrome 136+ requires a non-default profile for the debug port to open). After connecting, use cdp_summarize_page for page inventory, cdp_find_elements for targeted discovery, and cdp_get_element_context to expand an ambiguous match.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["port"],
                "properties": {
                    "port": {
                        "type": "integer",
                        "description": "The remote debugging port number"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let port_num = args
            .get("port")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| McpError::invalid_params("missing required param: port", None))?;
        if port_num > 65535 {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Invalid port: {}. Port must be 0-65535.",
                port_num
            ))]));
        }
        let port = port_num as u16;
        match crate::cdp::CdpClient::connect(port).await {
            Ok(client) => {
                let page_info = if let Some(page) = client.selected_page.as_ref() {
                    let url = crate::cdp::page_url(page).await;
                    format!("Selected page: {}", url)
                } else {
                    "No pages found".to_string()
                };
                *ctx.cdp_client.write().await = Some(client);
                // Tool list does not change on CDP connect/disconnect — CDP
                // tools are always listed so prompt caches remain stable.
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Connected to Chrome/Electron on port {}. CDP tool calls will now succeed.\n{}",
                    port, page_info
                ))]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }
}

/// `cdp_disconnect` — always visible; CDP tools remain listed afterward and
/// return a "not connected" error until `cdp_connect` succeeds again.
pub struct CdpDisconnect;

#[async_trait::async_trait]
impl ToolHandler for CdpDisconnect {
    fn name(&self) -> &'static str {
        "cdp_disconnect"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_disconnect",
            "Disconnect from the Chrome/Electron app. CDP tools remain listed but will return a 'not connected' error until cdp_connect is called again.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        if let Some(client) = ctx.cdp_client.write().await.take() {
            client.disconnect();
            // Tool list is unchanged on disconnect — CDP tools remain
            // listed and will return "not connected" errors until
            // cdp_connect succeeds again.
            Ok(CallToolResult::success(vec![Content::text(
                "Disconnected from Chrome/Electron. CDP tool calls will return a 'not connected' error until cdp_connect is called again.",
            )]))
        } else {
            // Use the canonical "not connected" message shared by every
            // CDP tool handler so clients see one stable error shape.
            Ok(CallToolResult::error(vec![Content::text(
                "No CDP connection. Use cdp_connect first.",
            )]))
        }
    }
}

/// `cdp_list_pages` — list open pages (tabs) in the connected browser.
pub struct CdpListPages;

#[async_trait::async_trait]
impl ToolHandler for CdpListPages {
    fn name(&self) -> &'static str {
        "cdp_list_pages"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_list_pages",
            "List all open pages (tabs) in the connected browser. Returns page indices and URLs. The currently selected page is marked with *. Use cdp_select_page to switch between pages.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        Ok(crate::cdp::tools::cdp_list_pages(ctx.cdp_client.clone()).await)
    }
}

/// `cdp_select_page` — select a page (tab) by index as the active context.
pub struct CdpSelectPage;

#[async_trait::async_trait]
impl ToolHandler for CdpSelectPage {
    fn name(&self) -> &'static str {
        "cdp_select_page"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_select_page",
            "Select a browser page (tab) by index as context for subsequent CDP operations. Call cdp_list_pages first to see available pages and their indices.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["page_idx"],
                "properties": {
                    "page_idx": {
                        "type": "integer",
                        "description": "Page index from cdp_list_pages"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let page_idx = args
            .get("page_idx")
            .and_then(|v| v.as_u64())
            .map(|p| p as usize)
            .ok_or_else(|| McpError::invalid_params("missing required param: page_idx", None))?;
        Ok(crate::cdp::tools::cdp_select_page(page_idx, ctx.cdp_client.clone()).await)
    }
}

/// `cdp_navigate` — navigate the selected page, or go back/forward/reload.
pub struct CdpNavigate;

#[async_trait::async_trait]
impl ToolHandler for CdpNavigate {
    fn name(&self) -> &'static str {
        "cdp_navigate"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_navigate",
            "Navigate the currently selected page to a URL, or go back, forward, or reload.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Target URL (required when type is 'url')"
                    },
                    "type": {
                        "type": "string",
                        "enum": ["url", "back", "forward", "reload"],
                        "description": "Navigation type. Default: 'url'"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Maximum wait time in milliseconds for page load (default: 10000). If the page takes longer, navigation is assumed successful."
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let url = args.get("url").and_then(|v| v.as_str()).map(String::from);
        let nav_type = args.get("type").and_then(|v| v.as_str()).map(String::from);
        let timeout = args.get("timeout").and_then(|v| v.as_u64());
        Ok(crate::cdp::tools::cdp_navigate(url, nav_type, timeout, ctx.cdp_client.clone()).await)
    }
}

/// `cdp_new_page` — create a new page (tab) navigated to the given URL.
pub struct CdpNewPage;

#[async_trait::async_trait]
impl ToolHandler for CdpNewPage {
    fn name(&self) -> &'static str {
        "cdp_new_page"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_new_page",
            "Create a new page (tab) and navigate it to the given URL. The new page becomes the selected page.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to load in the new page"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let url = parse_string_field(&args, "url")?;
        Ok(crate::cdp::tools::cdp_new_page(url, ctx.cdp_client.clone()).await)
    }
}

/// `cdp_close_page` — close a page (tab) by its index. Last page cannot close.
pub struct CdpClosePage;

#[async_trait::async_trait]
impl ToolHandler for CdpClosePage {
    fn name(&self) -> &'static str {
        "cdp_close_page"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_close_page",
            "Close a page (tab) by its index. The last open page cannot be closed.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["page_idx"],
                "properties": {
                    "page_idx": {
                        "type": "integer",
                        "description": "The index of the page to close. Call cdp_list_pages to list pages."
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let page_idx = args
            .get("page_idx")
            .and_then(|v| v.as_u64())
            .map(|p| p as usize)
            .ok_or_else(|| McpError::invalid_params("missing required param: page_idx", None))?;
        Ok(crate::cdp::tools::cdp_close_page(page_idx, ctx.cdp_client.clone()).await)
    }
}

/// `cdp_click` — click a DOM element by its UID.
pub struct CdpClick;

#[async_trait::async_trait]
impl ToolHandler for CdpClick {
    fn name(&self) -> &'static str {
        "cdp_click"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_click",
            "Click a DOM element by its UID from a cdp_take_dom_snapshot or cdp_find_elements result. Scrolls the element into view automatically and clicks its center. More reliable than coordinate-based clicking for web content.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["uid"],
                "properties": {
                    "uid": {
                        "type": "string",
                        "description": "Element UID from cdp_take_dom_snapshot or cdp_find_elements (d-prefixed)"
                    },
                    "dbl_click": {
                        "type": "boolean",
                        "description": "Double-click instead of single click (default: false)"
                    },
                    "include_snapshot": {
                        "type": "boolean",
                        "description": "Appends a DOM snapshot (d-prefixed UIDs) to the response (default: false)"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let uid = parse_string_field(&args, "uid")?;
        let dbl_click = args
            .get("dbl_click")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let include_snapshot = args
            .get("include_snapshot")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(
            crate::cdp::tools::cdp_click(uid, dbl_click, include_snapshot, ctx.cdp_client.clone())
                .await,
        )
    }
}

/// `cdp_hover` — hover over the provided element.
pub struct CdpHover;

#[async_trait::async_trait]
impl ToolHandler for CdpHover {
    fn name(&self) -> &'static str {
        "cdp_hover"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_hover",
            "Hover over the provided element.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["uid"],
                "properties": {
                    "uid": {
                        "type": "string",
                        "description": "Element UID from cdp_take_dom_snapshot or cdp_find_elements (d-prefixed)"
                    },
                    "include_snapshot": {
                        "type": "boolean",
                        "description": "Appends a DOM snapshot (d-prefixed UIDs) to the response (default: false)"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let uid = parse_string_field(&args, "uid")?;
        let include_snapshot = args
            .get("include_snapshot")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(crate::cdp::tools::cdp_hover(uid, include_snapshot, ctx.cdp_client.clone()).await)
    }
}

/// `cdp_fill` — type into an input/textarea/contenteditable, or pick a select option.
pub struct CdpFill;

#[async_trait::async_trait]
impl ToolHandler for CdpFill {
    fn name(&self) -> &'static str {
        "cdp_fill"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_fill",
            "Type text into an input, text area, rich contenteditable editor, or select an option from a <select> element. Rich editors are focused inside the page and receive CDP keyboard insertion so framework state updates.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["uid", "value"],
                "properties": {
                    "uid": {
                        "type": "string",
                        "description": "Element UID from cdp_take_dom_snapshot or cdp_find_elements (d-prefixed)"
                    },
                    "value": {
                        "type": "string",
                        "description": "The value to fill in"
                    },
                    "include_snapshot": {
                        "type": "boolean",
                        "description": "Appends a DOM snapshot (d-prefixed UIDs) to the response (default: false)"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let uid = parse_string_field(&args, "uid")?;
        let value = parse_string_field(&args, "value")?;
        let include_snapshot = args
            .get("include_snapshot")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(crate::cdp::tools::cdp_fill(uid, value, include_snapshot, ctx.cdp_client.clone()).await)
    }
}

/// `cdp_press_key` — press a key or key combination.
pub struct CdpPressKey;

#[async_trait::async_trait]
impl ToolHandler for CdpPressKey {
    fn name(&self) -> &'static str {
        "cdp_press_key"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_press_key",
            "Press a key or key combination. Use this when other input methods like cdp_fill cannot be used (e.g., keyboard shortcuts, navigation keys, or special key combinations).",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["key"],
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "A key or combination (e.g., 'Enter', 'Control+A', 'Control++', 'Control+Shift+R'). Modifiers: Control, Shift, Alt, Meta"
                    },
                    "include_snapshot": {
                        "type": "boolean",
                        "description": "Appends a DOM snapshot (d-prefixed UIDs) to the response (default: false)"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let key = parse_string_field(&args, "key")?;
        let include_snapshot = args
            .get("include_snapshot")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(crate::cdp::tools::cdp_press_key(key, include_snapshot, ctx.cdp_client.clone()).await)
    }
}

/// `cdp_handle_dialog` — accept or dismiss a browser dialog.
pub struct CdpHandleDialog;

#[async_trait::async_trait]
impl ToolHandler for CdpHandleDialog {
    fn name(&self) -> &'static str {
        "cdp_handle_dialog"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_handle_dialog",
            "If a browser dialog was opened, use this command to handle it.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["action"],
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["accept", "dismiss"],
                        "description": "Whether to accept or dismiss the dialog"
                    },
                    "prompt_text": {
                        "type": "string",
                        "description": "Optional text to enter into a prompt dialog before accepting"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let action = parse_string_field(&args, "action")?;
        let prompt_text = args
            .get("prompt_text")
            .and_then(|v| v.as_str())
            .map(String::from);
        Ok(crate::cdp::tools::cdp_handle_dialog(action, prompt_text, ctx.cdp_client.clone()).await)
    }
}

/// `cdp_type_text` — type text via keyboard into a previously focused input.
pub struct CdpTypeText;

#[async_trait::async_trait]
impl ToolHandler for CdpTypeText {
    fn name(&self) -> &'static str {
        "cdp_type_text"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_type_text",
            "Type text using keyboard into a previously focused input. Use cdp_fill for form fields; use this for character-by-character keyboard input.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The text to type"
                    },
                    "submit_key": {
                        "type": "string",
                        "description": "Optional key to press after typing (e.g., 'Enter', 'Tab', 'Escape')"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let text = parse_string_field(&args, "text")?;
        let submit_key = args
            .get("submit_key")
            .and_then(|v| v.as_str())
            .map(String::from);
        Ok(crate::cdp::tools::cdp_type_text(text, submit_key, ctx.cdp_client.clone()).await)
    }
}

/// `cdp_element_at_point` — hit-test the DOM element at screen coordinates.
pub struct CdpElementAtPoint;

#[async_trait::async_trait]
impl ToolHandler for CdpElementAtPoint {
    fn name(&self) -> &'static str {
        "cdp_element_at_point"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_element_at_point",
            "Given screen coordinates (x, y) in points, hit-test the DOM element at that \
             position. Always returns the element's backend_node_id. If a current DOM \
             snapshot already contains that node, the response also includes its d-prefixed \
             UID, role, and name; otherwise uid is null and a note points at \
             cdp_take_dom_snapshot / cdp_find_elements. Requires an active CDP connection. \
             Coordinates use the same screen-point system as element_at_point and click.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["x", "y"],
                "properties": {
                    "x": {
                        "type": "number",
                        "description": "Screen X coordinate in points"
                    },
                    "y": {
                        "type": "number",
                        "description": "Screen Y coordinate in points"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let (x, y) = parse_xy(&args)?;
        Ok(crate::cdp::tools::cdp_element_at_point(x, y, ctx.cdp_client.clone()).await)
    }
}

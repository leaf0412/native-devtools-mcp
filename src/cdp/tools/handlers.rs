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

use crate::tools::registry::{json_to_object, parse_string_field, ToolContext, ToolHandler};
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

use crate::app_protocol::AppProtocolClient;
use rmcp::model::{CallToolResult, Content};
use rmcp::service::{Peer, RoleServer};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;

pub type SharedClient = Arc<RwLock<Option<AppProtocolClient>>>;

/// Identity validation result
#[derive(Debug, PartialEq)]
pub enum IdentityValidationResult {
    /// Validation passed
    Ok,
    /// Bundle ID mismatch
    BundleIdMismatch {
        expected: String,
        actual: String,
        actual_app_name: String,
    },
    /// App name mismatch
    AppNameMismatch {
        expected: String,
        actual: String,
        actual_bundle_id: String,
    },
}

/// Validates bundle ID (exact, case-sensitive match)
pub fn validate_bundle_id(expected: &str, actual: &str) -> bool {
    expected == actual
}

/// Validates app name (case-insensitive, whitespace-trimmed)
pub fn validate_app_name(expected: &str, actual: &str) -> bool {
    expected.trim().eq_ignore_ascii_case(actual.trim())
}

/// Validates app identity against expected values from runtime info
pub fn validate_identity(
    expected_bundle_id: Option<&str>,
    expected_app_name: Option<&str>,
    info: &serde_json::Value,
) -> IdentityValidationResult {
    let actual_bundle_id = info.get("bundleId").and_then(|v| v.as_str()).unwrap_or("");
    let actual_app_name = info
        .get("appName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    // Validate bundle ID if expected
    if let Some(expected) = expected_bundle_id {
        if !validate_bundle_id(expected, actual_bundle_id) {
            return IdentityValidationResult::BundleIdMismatch {
                expected: expected.to_string(),
                actual: actual_bundle_id.to_string(),
                actual_app_name: actual_app_name.to_string(),
            };
        }
    }

    // Validate app name if expected
    if let Some(expected) = expected_app_name {
        if !validate_app_name(expected, actual_app_name) {
            return IdentityValidationResult::AppNameMismatch {
                expected: expected.to_string(),
                actual: actual_app_name.to_string(),
                actual_bundle_id: actual_bundle_id.to_string(),
            };
        }
    }

    IdentityValidationResult::Ok
}

#[derive(Debug, Deserialize)]
pub struct AppConnectParams {
    pub url: String,
    #[serde(default)]
    pub expected_bundle_id: Option<String>,
    #[serde(default)]
    pub expected_app_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AppGetTreeParams {
    #[serde(default)]
    pub depth: Option<i32>,
    #[serde(default)]
    pub root_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AppQueryParams {
    pub selector: String,
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Deserialize)]
pub struct AppClickParams {
    pub element_id: String,
    #[serde(default)]
    pub click_count: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct AppTypeParams {
    pub text: String,
    #[serde(default)]
    pub element_id: Option<String>,
    #[serde(default)]
    pub clear_first: bool,
}

#[derive(Debug, Deserialize)]
pub struct AppPressKeyParams {
    pub key: String,
    #[serde(default)]
    pub modifiers: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct AppFocusParams {
    pub element_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AppScreenshotParams {
    #[serde(default)]
    pub element_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AppGetElementParams {
    pub element_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AppFocusWindowParams {
    pub window_id: String,
}

const RELIST_HINT: &str = "Re-list tools to see app_* tools if your client doesn't auto-refresh.";

pub async fn app_connect(
    params: AppConnectParams,
    client: SharedClient,
    peer: Peer<RoleServer>,
) -> CallToolResult {
    // Check if there's an existing connection (for error messages)
    let has_existing = client.read().await.is_some();

    let new_client = match AppProtocolClient::connect(&params.url).await {
        Ok(c) => c,
        Err(e) => {
            return CallToolResult::error(vec![Content::text(format!("Failed to connect: {}", e))])
        }
    };

    // Get runtime info to validate identity
    let info = match new_client.get_runtime_info().await {
        Ok(info) => info,
        Err(e) => {
            // If we can't get info but have expectations, fail (preserve existing connection)
            if params.expected_bundle_id.is_some() || params.expected_app_name.is_some() {
                new_client.close(); // Clean up the new connection
                let existing_note = if has_existing {
                    " Existing connection preserved."
                } else {
                    ""
                };
                return CallToolResult::error(vec![Content::text(format!(
                    "Failed to get app info for validation: {}.{}",
                    e, existing_note
                ))]);
            }
            // No expectations, proceed without info
            *client.write().await = Some(new_client);
            let _ = peer.notify_tool_list_changed().await;
            return CallToolResult::success(vec![Content::text(format!(
                "Connected to {}. App debug tools (app_*) are now available. {}",
                params.url, RELIST_HINT
            ))]);
        }
    };

    // Validate identity using the extracted helper
    let validation_result = validate_identity(
        params.expected_bundle_id.as_deref(),
        params.expected_app_name.as_deref(),
        &info,
    );

    match validation_result {
        IdentityValidationResult::Ok => {}
        IdentityValidationResult::BundleIdMismatch {
            expected,
            actual,
            actual_app_name,
        } => {
            new_client.close(); // Clean up the new connection
            let existing_note = if has_existing {
                " Existing connection preserved."
            } else {
                ""
            };
            return CallToolResult::error(vec![Content::text(format!(
                "Identity mismatch: connected to \"{}\" (bundleId \"{}\"), but expected bundleId \"{}\".{}",
                actual_app_name, actual, expected, existing_note
            ))]);
        }
        IdentityValidationResult::AppNameMismatch {
            expected,
            actual,
            actual_bundle_id,
        } => {
            new_client.close(); // Clean up the new connection
            let existing_note = if has_existing {
                " Existing connection preserved."
            } else {
                ""
            };
            return CallToolResult::error(vec![Content::text(format!(
                "Identity mismatch: connected to \"{}\" (bundleId \"{}\"), but expected app name \"{}\".{}",
                actual, actual_bundle_id, expected, existing_note
            ))]);
        }
    }

    // Validation passed (or no expectations), store client
    *client.write().await = Some(new_client);
    let _ = peer.notify_tool_list_changed().await;

    let msg = format!(
        "Connected. App debug tools (app_*) are now available. {}\n\n{}",
        RELIST_HINT,
        serde_json::to_string_pretty(&info).unwrap_or_default()
    );
    CallToolResult::success(vec![Content::text(msg)])
}

pub async fn app_disconnect(client: SharedClient, peer: Peer<RoleServer>) -> CallToolResult {
    if client.write().await.take().is_some() {
        let _ = peer.notify_tool_list_changed().await;
        CallToolResult::success(vec![Content::text(
            "Disconnected. App debug tools (app_*) are no longer available.",
        )])
    } else {
        CallToolResult::error(vec![Content::text("Not connected to any app")])
    }
}

/// Helper to get a cloned client, releasing the lock before async operations
async fn get_client(shared: &SharedClient) -> Option<AppProtocolClient> {
    shared.read().await.clone()
}

pub async fn app_get_info(client: SharedClient) -> CallToolResult {
    let Some(client) = get_client(&client).await else {
        return CallToolResult::error(vec![Content::text("Not connected. Use app_connect first.")]);
    };

    match client.get_runtime_info().await {
        Ok(info) => CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap_or_else(|_| "{}".to_string()),
        )]),
        Err(e) => CallToolResult::error(vec![Content::text(format!("Failed: {}", e))]),
    }
}

pub async fn app_get_tree(params: AppGetTreeParams, client: SharedClient) -> CallToolResult {
    let Some(client) = get_client(&client).await else {
        return CallToolResult::error(vec![Content::text("Not connected. Use app_connect first.")]);
    };

    match client
        .get_tree(params.depth, params.root_id.as_deref())
        .await
    {
        Ok(tree) => CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&tree).unwrap_or_else(|_| "{}".to_string()),
        )]),
        Err(e) => CallToolResult::error(vec![Content::text(format!("Failed: {}", e))]),
    }
}

pub async fn app_query(params: AppQueryParams, client: SharedClient) -> CallToolResult {
    let Some(client) = get_client(&client).await else {
        return CallToolResult::error(vec![Content::text("Not connected. Use app_connect first.")]);
    };

    let result = if params.all {
        client.query_selector_all(&params.selector).await
    } else {
        client.query_selector(&params.selector).await
    };

    match result {
        Ok(elements) => CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&elements).unwrap_or_else(|_| "{}".to_string()),
        )]),
        Err(e) => CallToolResult::error(vec![Content::text(format!("Failed: {}", e))]),
    }
}

pub async fn app_get_element(params: AppGetElementParams, client: SharedClient) -> CallToolResult {
    let Some(client) = get_client(&client).await else {
        return CallToolResult::error(vec![Content::text("Not connected. Use app_connect first.")]);
    };

    match client.get_element(&params.element_id).await {
        Ok(element) => CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&element).unwrap_or_else(|_| "{}".to_string()),
        )]),
        Err(e) => CallToolResult::error(vec![Content::text(format!("Failed: {}", e))]),
    }
}

pub async fn app_click(params: AppClickParams, client: SharedClient) -> CallToolResult {
    let Some(client) = get_client(&client).await else {
        return CallToolResult::error(vec![Content::text("Not connected. Use app_connect first.")]);
    };

    match client.click(&params.element_id, params.click_count).await {
        Ok(_) => CallToolResult::success(vec![Content::text(format!(
            "Clicked element: {}",
            params.element_id
        ))]),
        Err(e) => CallToolResult::error(vec![Content::text(format!("Failed: {}", e))]),
    }
}

pub async fn app_type(params: AppTypeParams, client: SharedClient) -> CallToolResult {
    let Some(client) = get_client(&client).await else {
        return CallToolResult::error(vec![Content::text("Not connected. Use app_connect first.")]);
    };

    match client
        .type_text(
            &params.text,
            params.element_id.as_deref(),
            params.clear_first,
        )
        .await
    {
        Ok(_) => CallToolResult::success(vec![Content::text(format!("Typed: {}", params.text))]),
        Err(e) => CallToolResult::error(vec![Content::text(format!("Failed: {}", e))]),
    }
}

pub async fn app_press_key(params: AppPressKeyParams, client: SharedClient) -> CallToolResult {
    let Some(client) = get_client(&client).await else {
        return CallToolResult::error(vec![Content::text("Not connected. Use app_connect first.")]);
    };

    match client.press_key(&params.key, params.modifiers).await {
        Ok(_) => CallToolResult::success(vec![Content::text(format!("Pressed: {}", params.key))]),
        Err(e) => CallToolResult::error(vec![Content::text(format!("Failed: {}", e))]),
    }
}

pub async fn app_focus(params: AppFocusParams, client: SharedClient) -> CallToolResult {
    let Some(client) = get_client(&client).await else {
        return CallToolResult::error(vec![Content::text("Not connected. Use app_connect first.")]);
    };

    match client.focus(&params.element_id).await {
        Ok(_) => CallToolResult::success(vec![Content::text(format!(
            "Focused element: {}",
            params.element_id
        ))]),
        Err(e) => CallToolResult::error(vec![Content::text(format!("Failed: {}", e))]),
    }
}

pub async fn app_screenshot(params: AppScreenshotParams, client: SharedClient) -> CallToolResult {
    let Some(client) = get_client(&client).await else {
        return CallToolResult::error(vec![Content::text("Not connected. Use app_connect first.")]);
    };

    match client.get_screenshot(params.element_id.as_deref()).await {
        Ok(result) => {
            // Extract base64 data from result
            if let Some(data) = result.get("data").and_then(|v| v.as_str()) {
                let width = result.get("width").and_then(|v| v.as_i64()).unwrap_or(0);
                let height = result.get("height").and_then(|v| v.as_i64()).unwrap_or(0);

                CallToolResult::success(vec![
                    Content::text(format!("Screenshot: {}x{}", width, height)),
                    Content::image(data, "image/png"),
                ])
            } else {
                CallToolResult::error(vec![Content::text("Invalid screenshot response")])
            }
        }
        Err(e) => CallToolResult::error(vec![Content::text(format!("Failed: {}", e))]),
    }
}

pub async fn app_list_windows(client: SharedClient) -> CallToolResult {
    let Some(client) = get_client(&client).await else {
        return CallToolResult::error(vec![Content::text("Not connected. Use app_connect first.")]);
    };

    match client.list_windows().await {
        Ok(windows) => CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&windows).unwrap_or_else(|_| "{}".to_string()),
        )]),
        Err(e) => CallToolResult::error(vec![Content::text(format!("Failed: {}", e))]),
    }
}

pub async fn app_focus_window(
    params: AppFocusWindowParams,
    client: SharedClient,
) -> CallToolResult {
    let Some(client) = get_client(&client).await else {
        return CallToolResult::error(vec![Content::text("Not connected. Use app_connect first.")]);
    };

    match client.focus_window(&params.window_id).await {
        Ok(_) => CallToolResult::success(vec![Content::text(format!(
            "Focused window: {}",
            params.window_id
        ))]),
        Err(e) => CallToolResult::error(vec![Content::text(format!("Failed: {}", e))]),
    }
}

// ============================================================================
// MCP tool handlers. Each wraps the free function above with its name, schema,
// and availability. `app_connect` is always visible (so the user can connect);
// the others are gated `WhenAppConnected`, mirroring `get_app_connect_tool` /
// `get_app_tools` + the connection gate in `get_tools`. Schema JSON moved
// verbatim from the deleted getters; call bodies copied verbatim from the
// deleted `call_tool` arms, with `ctx.app_client` / `ctx.peer` replacing
// `self.app_client` / `context.peer`.
// ============================================================================

use crate::tools::registry::{
    json_to_object, Availability, ToolContext, ToolHandler,
};
use rmcp::{model::Tool, Error as McpError};

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

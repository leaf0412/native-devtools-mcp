use crate::platform;
use crate::tools::probe_app::{classify_running_app, AppKind};
use rmcp::model::{CallToolResult, Content};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct ListWindowsParams {
    /// Filter by application name (optional)
    pub app_name: Option<String>,
}

pub fn list_windows(params: ListWindowsParams) -> CallToolResult {
    let windows = if let Some(app_name) = params.app_name {
        match platform::find_windows_by_app(&app_name) {
            Ok(w) => w,
            Err(e) => return CallToolResult::error(vec![Content::text(e)]),
        }
    } else {
        match platform::list_windows() {
            Ok(w) => w,
            Err(e) => return CallToolResult::error(vec![Content::text(e)]),
        }
    };

    match serde_json::to_string_pretty(&windows) {
        Ok(json) => CallToolResult::success(vec![Content::text(json)]),
        Err(e) => CallToolResult::error(vec![Content::text(format!(
            "Failed to serialize windows: {}",
            e
        ))]),
    }
}

#[derive(Debug, Deserialize)]
pub struct ListAppsParams {
    /// Filter by application name (case-insensitive substring match)
    pub app_name: Option<String>,
    /// Only return user-facing apps (excludes system agents, helpers, and daemons)
    pub user_apps_only: Option<bool>,
}

pub fn list_apps(params: ListAppsParams) -> CallToolResult {
    let mut apps = platform::list_apps();

    if let Some(ref name) = params.app_name {
        let needle = name.to_lowercase();
        apps.retain(|app| app.name.to_lowercase().contains(&needle));
    }

    if params.user_apps_only.unwrap_or(false) {
        apps.retain(|app| app.is_user_app);
    }

    match serde_json::to_string_pretty(&apps) {
        Ok(json) => CallToolResult::success(vec![Content::text(json)]),
        Err(e) => CallToolResult::error(vec![Content::text(format!(
            "Failed to serialize apps: {}",
            e
        ))]),
    }
}

#[derive(Debug, Deserialize)]
pub struct LaunchAppParams {
    /// Application name to launch (e.g., "Calculator", "Safari")
    pub app_name: String,
    /// Optional CLI arguments to pass to the app (e.g., ["--remote-debugging-port=9222"])
    pub args: Option<Vec<String>>,
    /// If true, launch without bringing the app to the foreground (uses `open -g` on macOS).
    /// Recommended when the next action will use CDP or AX dispatch, which are focus-preserving.
    pub background: Option<bool>,
}

pub fn launch_app(params: LaunchAppParams) -> CallToolResult {
    let args = params.args.as_deref().unwrap_or(&[]);
    let background = params.background.unwrap_or(false);

    // If args are provided, check if the app is already running — args only apply on fresh launch
    if !args.is_empty() && platform::is_app_running(&params.app_name) {
        return CallToolResult::error(vec![Content::text(format!(
            "'{}' is already running. CLI args only apply on fresh launch. Use quit_app to quit it first, then retry.",
            params.app_name
        ))]);
    }

    match platform::launch_app(&params.app_name, args, background) {
        Ok(()) => CallToolResult::success(vec![Content::text(format!(
            "Launched '{}'",
            params.app_name
        ))]),
        Err(e) => CallToolResult::error(vec![Content::text(e)]),
    }
}

#[derive(Debug, Deserialize)]
pub struct QuitAppParams {
    /// Application name to quit
    pub app_name: String,
    /// Force kill instead of graceful termination (default: false)
    pub force: Option<bool>,
}

pub fn quit_app(params: QuitAppParams) -> CallToolResult {
    let force = params.force.unwrap_or(false);
    match platform::quit_app(&params.app_name, force) {
        Ok(count) => {
            let method = if force { "Force-killed" } else { "Quit" };
            CallToolResult::success(vec![Content::text(format!(
                "{} '{}' ({} instance{})",
                method,
                params.app_name,
                count,
                if count == 1 { "" } else { "s" }
            ))])
        }
        Err(e) => CallToolResult::error(vec![Content::text(e)]),
    }
}

#[derive(Debug, Deserialize)]
pub struct FocusWindowParams {
    /// Window ID to focus (optional, use with window_id)
    pub window_id: Option<u32>,

    /// Application name to focus (optional, use with app_name)
    pub app_name: Option<String>,

    /// Process ID to focus (optional, use with pid)
    pub pid: Option<i32>,
}

/// Structured result returned by `focus_window`. Includes the resolved
/// identity of the focused app regardless of which variant the caller
/// used (app_name / pid / window_id), so downstream tools (CDP
/// auto-connect in particular) don't have to re-resolve it with a
/// follow-up `list_apps` or `list_windows` call.
#[derive(Debug, Serialize)]
pub struct FocusWindowResult {
    /// Human-readable app name. Always present on success.
    pub app_name: String,
    /// Process ID of the focused app. Always present on success.
    pub pid: i32,
    /// Bundle identifier (macOS only; `None` on Windows or when unavailable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    /// Classification used by CDP auto-connect policy.
    pub kind: AppKind,
}

fn focused(result: FocusWindowResult) -> CallToolResult {
    match serde_json::to_string(&result) {
        Ok(json) => CallToolResult::success(vec![Content::text(json)]),
        Err(e) => CallToolResult::error(vec![Content::text(format!(
            "Failed to serialize focus_window result: {}",
            e
        ))]),
    }
}

fn error(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.into())])
}

/// Build the structured result for a successfully focused PID by
/// consulting the live app list. Falls back to a minimal result when
/// the PID is not in the app list (possible for short-lived apps
/// between activation and enumeration) — `app_name` becomes the empty
/// string and `kind` defaults to `Native`.
fn build_result_for_pid(pid: i32, fallback_name: Option<&str>) -> FocusWindowResult {
    if let Some(app) = platform::list_apps().into_iter().find(|a| a.pid == pid) {
        let kind = classify_running_app(app.pid, app.bundle_id.as_deref(), &app.name);
        FocusWindowResult {
            app_name: app.name,
            pid: app.pid,
            bundle_id: app.bundle_id,
            kind,
        }
    } else {
        let name = fallback_name.unwrap_or("").to_string();
        let kind = if name.is_empty() {
            AppKind::Native
        } else {
            classify_running_app(pid, None, &name)
        };
        FocusWindowResult {
            app_name: name,
            pid,
            bundle_id: None,
            kind,
        }
    }
}

pub fn focus_window(params: FocusWindowParams) -> CallToolResult {
    if let Some(app_name) = params.app_name {
        focus_by_app_name(&app_name)
    } else if let Some(pid) = params.pid {
        focus_by_pid(pid)
    } else if let Some(window_id) = params.window_id {
        focus_by_window_id(window_id)
    } else {
        error("Provide one of: window_id, app_name, or pid")
    }
}

fn focus_by_app_name(app_name: &str) -> CallToolResult {
    if platform::activate_app(app_name) {
        // AXRaise ensures the window is physically raised even for apps without
        // a proper macOS bundle (e.g. Tauri dev builds) where NSRunningApplication.activate
        // reports success but doesn't bring the window to front.
        let pid = platform::find_windows_by_app(app_name)
            .ok()
            .and_then(|w| w.first().map(|win| win.owner_pid as i32));
        if let Some(pid) = pid {
            platform::raise_windows(pid);
            return focused(build_result_for_pid(pid, Some(app_name)));
        }
        // Activation succeeded but we couldn't find a PID to enrich with.
        // Return a minimal result keyed off the caller-supplied name.
        return focused(FocusWindowResult {
            app_name: app_name.to_string(),
            pid: 0,
            bundle_id: None,
            kind: AppKind::Native,
        });
    }

    // Fallback: the primary activate_app path may not find some apps (e.g. Catalyst/SwiftUI
    // on macOS). Try finding the app via the window list and activating by PID instead.
    let pid = platform::find_windows_by_app(app_name)
        .ok()
        .and_then(|w| w.first().map(|win| win.owner_pid as i32));

    if let Some(pid) = pid {
        if platform::activate_app_by_pid(pid) {
            platform::raise_windows(pid);
            return focused(build_result_for_pid(pid, Some(app_name)));
        }
    }

    error(format!(
        "No app found matching '{}'. Use list_apps to find the correct app name.",
        app_name
    ))
}

fn focus_by_pid(pid: i32) -> CallToolResult {
    if platform::activate_app_by_pid(pid) {
        platform::raise_windows(pid);
        focused(build_result_for_pid(pid, None))
    } else {
        error(format!(
            "No app found with PID {}. Use list_apps to find running apps.",
            pid
        ))
    }
}

fn focus_by_window_id(window_id: u32) -> CallToolResult {
    match platform::find_window_by_id(window_id) {
        Ok(Some(window)) => {
            let pid = window.owner_pid as i32;
            let owner_name = window.owner_name.clone();
            if platform::activate_app_by_pid(pid) {
                platform::raise_windows(pid);
                // `owner_name` gives us a reliable fallback if `list_apps`
                // doesn't surface this pid (e.g. helper window owned by
                // a process that list_apps doesn't consider user-facing).
                focused(build_result_for_pid(pid, Some(&owner_name)))
            } else {
                error(format!(
                    "Found window {} but failed to activate its owning app (PID {}).",
                    window_id, window.owner_pid
                ))
            }
        }
        Ok(None) => error(format!(
            "Window {} not found. Use list_windows to find available windows.",
            window_id
        )),
        Err(e) => error(e),
    }
}

use crate::tools::registry::{json_to_object, ToolContext, ToolHandler};
use rmcp::{model::Tool, Error as McpError};
use std::sync::Arc;

/// `list_windows` MCP tool handler.
pub struct ListWindows;

#[async_trait::async_trait]
impl ToolHandler for ListWindows {
    fn name(&self) -> &'static str {
        "list_windows"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "list_windows",
            "List all visible windows on screen with their IDs, titles, app names, and bounds.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "app_name": {
                        "type": "string",
                        "description": "Filter windows by application name (optional)"
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
        let params: ListWindowsParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(list_windows(params))
    }
}

/// `list_apps` MCP tool handler.
pub struct ListApps;

#[async_trait::async_trait]
impl ToolHandler for ListApps {
    fn name(&self) -> &'static str {
        "list_apps"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "list_apps",
            "List all running applications with their names, bundle IDs, and PIDs.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "app_name": {
                        "type": "string",
                        "description": "Filter by application name (case-insensitive substring match)"
                    },
                    "user_apps_only": {
                        "type": "boolean",
                        "description": "Only return user-facing apps (excludes system agents, helpers, and daemons)"
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
        let params: ListAppsParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(list_apps(params))
    }
}

/// `focus_window` MCP tool handler.
pub struct FocusWindow;

#[async_trait::async_trait]
impl ToolHandler for FocusWindow {
    fn name(&self) -> &'static str {
        "focus_window"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "focus_window",
            "Bring a window or application to the front. Specify window_id, app_name, or pid.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "window_id": {
                        "type": "integer",
                        "description": "Window ID to focus"
                    },
                    "app_name": {
                        "type": "string",
                        "description": "Application name to focus"
                    },
                    "pid": {
                        "type": "integer",
                        "description": "Process ID to focus"
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
        let params: FocusWindowParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(focus_window(params))
    }
}

/// `launch_app` MCP tool handler.
pub struct LaunchApp;

#[async_trait::async_trait]
impl ToolHandler for LaunchApp {
    fn name(&self) -> &'static str {
        "launch_app"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "launch_app",
            "Launch an application by name. On macOS, finds apps in /Applications and other standard locations. If the app is already running and no args are provided, brings it to the front. If args are provided and the app is already running, returns an error — use quit_app first.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["app_name"],
                "properties": {
                    "app_name": {
                        "type": "string",
                        "description": "Application name to launch (e.g., 'Calculator', 'Safari')"
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "CLI arguments to pass to the app (e.g., ['--remote-debugging-port=9222']). Only applied on fresh launch — if the app is already running, returns an error."
                    },
                    "background": {
                        "type": "boolean",
                        "description": "If true, launch without bringing the app to the foreground (uses `open -g` on macOS). Recommended when the next action will use CDP or AX dispatch, which are focus-preserving."
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
        let params: LaunchAppParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(launch_app(params))
    }
}

/// `quit_app` MCP tool handler.
pub struct QuitApp;

#[async_trait::async_trait]
impl ToolHandler for QuitApp {
    fn name(&self) -> &'static str {
        "quit_app"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "quit_app",
            "Quit a running application by name. Graceful by default (app can save state). Use force=true to kill immediately.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["app_name"],
                "properties": {
                    "app_name": {
                        "type": "string",
                        "description": "Application name to quit (e.g., 'Calculator', 'Safari')"
                    },
                    "force": {
                        "type": "boolean",
                        "description": "Force kill instead of graceful quit (default: false)"
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
        let params: QuitAppParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(quit_app(params))
    }
}

#[cfg(test)]
mod focus_window_tests {
    use super::*;

    #[test]
    fn result_serializes_structured_fields() {
        let result = FocusWindowResult {
            app_name: "Signal".to_string(),
            pid: 16024,
            bundle_id: Some("org.whispersystems.signal-desktop".to_string()),
            kind: AppKind::ElectronApp,
        };
        let json: serde_json::Value = serde_json::to_value(&result).unwrap();
        assert_eq!(json["app_name"], "Signal");
        assert_eq!(json["pid"], 16024);
        assert_eq!(json["bundle_id"], "org.whispersystems.signal-desktop");
        assert_eq!(json["kind"], "ElectronApp");
    }

    #[test]
    fn result_omits_missing_bundle_id() {
        let result = FocusWindowResult {
            app_name: "Notepad".to_string(),
            pid: 42,
            bundle_id: None,
            kind: AppKind::Native,
        };
        let json: serde_json::Value = serde_json::to_value(&result).unwrap();
        assert!(!json.as_object().unwrap().contains_key("bundle_id"));
        assert_eq!(json["kind"], "Native");
    }
}

//! Tool trait + registry that replaces the hand-written `call_tool` match and
//! `get_*_tools` schema getters in `server.rs`.
//!
//! Each MCP tool is one [`ToolHandler`] implementor living in its own module,
//! owning its name, schema, availability, and call body. The [`ToolRegistry`]
//! collects them into a `Vec<Box<dyn ToolHandler>>`; `list_tools` filters by
//! [`Availability`] against the runtime [`ConnectionState`], and `call_tool`
//! dispatches by name.

use crate::app_protocol::AppProtocolClient;
use crate::tools::{image_cache::ImageCache, screenshot_cache::ScreenshotCache};
use rmcp::{
    model::{CallToolResult, Tool},
    service::{Peer, RoleServer},
    Error as McpError,
};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::android::AndroidDevice;

/// Shared server state handed to every tool's `call`. A tool clones only the
/// one `Arc` it needs (segregation by convention + review, not per-capability
/// sub-traits — YAGNI for a closed set of Arcs).
#[derive(Clone)]
pub struct ToolContext {
    pub app_client: Arc<RwLock<Option<AppProtocolClient>>>,
    pub screenshot_cache: Arc<RwLock<ScreenshotCache>>,
    pub image_cache: Arc<RwLock<ImageCache>>,
    pub android_device: Arc<RwLock<Option<AndroidDevice>>>,
    pub hover_tracker: Arc<RwLock<Option<crate::tools::hover_tracker::HoverTracker>>>,
    pub screen_recorder: Arc<RwLock<Option<crate::tools::screen_recorder::ScreenRecorder>>>,
    #[cfg(feature = "cdp")]
    pub cdp_client: Arc<RwLock<Option<crate::cdp::CdpClient>>>,
    #[cfg(target_os = "macos")]
    pub ax_session: Arc<crate::tools::ax_session::AxSession>,
    pub peer: Peer<RoleServer>,
}

/// Runtime connection axes that gate which tools are visible. Compile-time
/// platform/feature gating stays as `#[cfg]` on registration, not here.
pub struct ConnectionState {
    pub app_connected: bool,
    pub android_connected: bool,
    pub hover_tracking: bool,
    pub recording: bool,
}

/// When a tool is visible in `list_tools`, as a function of [`ConnectionState`].
pub enum Availability {
    Always,
    WhenAppConnected,
    WhenAndroidConnected,
    WhenHoverTracking,
    WhenRecording,
}

impl Availability {
    pub fn is_visible(&self, state: &ConnectionState) -> bool {
        match self {
            Availability::Always => true,
            Availability::WhenAppConnected => state.app_connected,
            Availability::WhenAndroidConnected => state.android_connected,
            Availability::WhenHoverTracking => state.hover_tracking,
            Availability::WhenRecording => state.recording,
        }
    }
}

/// A single MCP tool: one source of truth for its name, schema, gating, and
/// behavior. `name()` is both the dispatch key and the schema name.
#[async_trait::async_trait]
pub trait ToolHandler: Send + Sync {
    /// Dispatch key and schema name — single source of truth.
    fn name(&self) -> &'static str;

    /// Verbatim-moved schema JSON; the only place a tool's schema lives.
    fn schema(&self) -> Tool;

    /// Runtime visibility axis. Compile-time gating is `#[cfg]` on registration.
    fn availability(&self) -> Availability {
        Availability::Always
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError>;
}

/// Collection of every migrated tool handler.
pub struct ToolRegistry {
    tools: Vec<Box<dyn ToolHandler>>,
}

impl ToolRegistry {
    /// Build the registry. `#[cfg]` guards mirror today's getters exactly.
    pub fn build() -> Self {
        #[allow(unused_mut)]
        let mut tools: Vec<Box<dyn ToolHandler>> = Vec::new();
        tools.push(Box::new(crate::tools::screenshot::TakeScreenshot));
        tools.push(Box::new(crate::tools::navigation::ListWindows));
        tools.push(Box::new(crate::tools::navigation::ListApps));
        tools.push(Box::new(crate::tools::navigation::FocusWindow));
        tools.push(Box::new(crate::tools::navigation::LaunchApp));
        tools.push(Box::new(crate::tools::navigation::QuitApp));
        tools.push(Box::new(crate::tools::probe_app::ProbeApp));
        tools.push(Box::new(crate::tools::input::Click));
        tools.push(Box::new(crate::tools::input::MoveMouse));
        tools.push(Box::new(crate::tools::input::Drag));
        tools.push(Box::new(crate::tools::input::Scroll));
        tools.push(Box::new(crate::tools::input::TypeText));
        tools.push(Box::new(crate::tools::input::PressKey));
        tools.push(Box::new(crate::tools::input::GetDisplays));
        tools.push(Box::new(crate::tools::input::FindText));
        tools.push(Box::new(crate::tools::input::ElementAtPoint));
        tools.push(Box::new(crate::tools::find_image::FindImage));
        tools.push(Box::new(crate::tools::load_image::LoadImage));
        tools.push(Box::new(crate::tools::ax_snapshot::TakeAxSnapshot));
        #[cfg(target_os = "macos")]
        {
            tools.push(Box::new(crate::tools::ax_click::AxClick));
            tools.push(Box::new(crate::tools::ax_set_value::AxSetValue));
            tools.push(Box::new(crate::tools::ax_select::AxSelect));
        }
        tools.push(Box::new(crate::tools::app_protocol::AppConnect));
        tools.push(Box::new(crate::tools::app_protocol::AppDisconnect));
        tools.push(Box::new(crate::tools::app_protocol::AppGetInfo));
        tools.push(Box::new(crate::tools::app_protocol::AppGetTree));
        tools.push(Box::new(crate::tools::app_protocol::AppGetElement));
        tools.push(Box::new(crate::tools::app_protocol::AppListWindows));
        tools.push(Box::new(crate::tools::app_protocol::AppFocusWindow));
        tools.push(Box::new(crate::tools::app_protocol::AppQuery));
        tools.push(Box::new(crate::tools::app_protocol::AppClick));
        tools.push(Box::new(crate::tools::app_protocol::AppType));
        tools.push(Box::new(crate::tools::app_protocol::AppPressKey));
        tools.push(Box::new(crate::tools::app_protocol::AppFocus));
        tools.push(Box::new(crate::tools::app_protocol::AppScreenshot));
        #[cfg(feature = "cdp")]
        {
            tools.push(Box::new(crate::cdp::tools::handlers::CdpConnect));
            tools.push(Box::new(crate::cdp::tools::handlers::CdpDisconnect));
            tools.push(Box::new(crate::cdp::tools::handlers::CdpNavigate));
            tools.push(Box::new(crate::cdp::tools::handlers::CdpNewPage));
            tools.push(Box::new(crate::cdp::tools::handlers::CdpListPages));
            tools.push(Box::new(crate::cdp::tools::handlers::CdpSelectPage));
            tools.push(Box::new(crate::cdp::tools::handlers::CdpClosePage));
            tools.push(Box::new(crate::cdp::tools::handlers::CdpClick));
            tools.push(Box::new(crate::cdp::tools::handlers::CdpFill));
            tools.push(Box::new(crate::cdp::tools::handlers::CdpTypeText));
            tools.push(Box::new(crate::cdp::tools::handlers::CdpPressKey));
            tools.push(Box::new(crate::cdp::tools::handlers::CdpHover));
            tools.push(Box::new(crate::cdp::tools::handlers::CdpHandleDialog));
            tools.push(Box::new(crate::cdp::tools::handlers::CdpElementAtPoint));
        }
        tools.push(Box::new(crate::android::tools::AndroidListDevices));
        tools.push(Box::new(crate::android::tools::AndroidConnect));
        tools.push(Box::new(crate::android::tools::AndroidDisconnect));
        tools.push(Box::new(crate::android::tools::AndroidScreenshot));
        tools.push(Box::new(crate::android::tools::AndroidClick));
        tools.push(Box::new(crate::android::tools::AndroidTypeText));
        tools.push(Box::new(crate::android::tools::AndroidPressKey));
        tools.push(Box::new(crate::android::tools::AndroidSwipe));
        tools.push(Box::new(crate::android::tools::AndroidFindText));
        tools.push(Box::new(crate::android::tools::AndroidListApps));
        tools.push(Box::new(crate::android::tools::AndroidLaunchApp));
        tools.push(Box::new(crate::android::tools::AndroidGetDisplayInfo));
        tools.push(Box::new(crate::android::tools::AndroidGetCurrentActivity));
        tools.push(Box::new(crate::tools::hover_tracker::StartHoverTracking));
        tools.push(Box::new(crate::tools::hover_tracker::GetHoverEvents));
        tools.push(Box::new(crate::tools::hover_tracker::StopHoverTracking));
        tools.push(Box::new(crate::tools::screen_recorder::StartRecording));
        tools.push(Box::new(crate::tools::screen_recorder::StopRecording));
        Self { tools }
    }

    /// Look up a handler by name.
    pub fn get(&self, name: &str) -> Option<&dyn ToolHandler> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    /// Names of every registered tool — used by the legacy-getter union to
    /// drop already-migrated tools and avoid double emission.
    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    /// Schemas of tools visible for the given connection state.
    pub fn schemas(&self, state: &ConnectionState) -> Vec<Tool> {
        self.tools
            .iter()
            .filter(|t| t.availability().is_visible(state))
            .map(|t| t.schema())
            .collect()
    }
}

// ============================================================================
// Shared argument-parsing helpers, moved verbatim from `server.rs` so tool
// handlers can reuse the exact same `Err(McpError::invalid_params)` shapes.
// ============================================================================

/// Serialize a value to pretty-printed JSON, returning a formatted error on failure.
pub fn to_json_pretty(value: &impl Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("Failed to serialize: {}", e))
}

/// Extract a required string field from a JSON value.
pub fn parse_string_field(args: &Value, field: &str) -> Result<String, McpError> {
    args.get(field)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| McpError::invalid_params(format!("missing required param: {}", field), None))
}

/// Extract required `x` and `y` number fields from a JSON value.
pub fn parse_xy(args: &Value) -> Result<(f64, f64), McpError> {
    let x = args
        .get("x")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| McpError::invalid_params("missing required param: x", None))?;
    let y = args
        .get("y")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| McpError::invalid_params("missing required param: y", None))?;
    Ok((x, y))
}

pub fn json_to_object(value: Value) -> rmcp::model::JsonObject {
    match value {
        Value::Object(map) => map,
        _ => Default::default(),
    }
}

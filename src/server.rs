use crate::app_protocol::AppProtocolClient;
use crate::tools::registry::{ConnectionState, ToolContext, ToolRegistry};
use crate::tools::{image_cache::ImageCache, screenshot_cache::ScreenshotCache};
use rmcp::{
    handler::server::ServerHandler,
    model::{
        CallToolRequestParam, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParam, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
        ToolAnnotations,
    },
    service::{RequestContext, RoleServer},
    Error as McpError,
};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::android::AndroidDevice;

// ============================================================================
// Tool safety-hint annotations
//
// Each tool is tagged with the MCP `ToolAnnotations` hints
// (readOnlyHint, destructiveHint, idempotentHint, openWorldHint) so clients
// can reason about safety before invoking. These are *hints* per the MCP spec.
// ============================================================================

/// Read-only, idempotent, closed-world (queries: screenshots, snapshots, finds).
fn annotate_read_only() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(true)
        .idempotent(true)
        .destructive(false)
        .open_world(false)
}

/// Non-destructive state change on a closed world (clicks, typing, scrolling,
/// focusing, launching a local app, connecting to a local debug server).
fn annotate_state_change() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(false)
        .idempotent(false)
        .destructive(false)
        .open_world(false)
}

/// Destructive tool on a closed world (quit app, close tab).
fn annotate_destructive() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(false)
        .idempotent(false)
        .destructive(true)
        .open_world(false)
}

/// Non-destructive state change that reaches an open world (e.g. web
/// navigation, arbitrary URL loads).
fn annotate_open_world_state_change() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(false)
        .idempotent(false)
        .destructive(false)
        .open_world(true)
}

/// Arbitrary code evaluation in an open world (JS eval in a browser page).
fn annotate_open_world_destructive() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(false)
        .idempotent(false)
        .destructive(true)
        .open_world(true)
}

/// Apply the given annotation to every tool in `tools` whose name appears in
/// `names`. Missing names are silently ignored because the conditional tool
/// groups (app_*, android_*, cdp_*, hover, recording) aren't always present.
/// The `test_every_tool_has_annotations` test catches any tool left without
/// an annotation.
fn annotate_tools(tools: &mut [Tool], names: &[&str], annotation: ToolAnnotations) {
    for name in names {
        if let Some(tool) = tools.iter_mut().find(|t| t.name.as_ref() == *name) {
            tool.annotations = Some(annotation.clone());
        }
    }
}

#[derive(Clone)]
pub struct MacOSDevToolsServer {
    app_client: Arc<RwLock<Option<AppProtocolClient>>>,
    screenshot_cache: Arc<RwLock<ScreenshotCache>>,
    image_cache: Arc<RwLock<ImageCache>>,
    android_device: Arc<RwLock<Option<AndroidDevice>>>,
    hover_tracker: Arc<RwLock<Option<crate::tools::hover_tracker::HoverTracker>>>,
    screen_recorder: Arc<RwLock<Option<crate::tools::screen_recorder::ScreenRecorder>>>,
    #[cfg(feature = "cdp")]
    cdp_client: Arc<RwLock<Option<crate::cdp::CdpClient>>>,
    #[cfg(target_os = "macos")]
    ax_session: Arc<crate::tools::ax_session::AxSession>,
}

impl Default for MacOSDevToolsServer {
    fn default() -> Self {
        Self::new()
    }
}

impl MacOSDevToolsServer {
    pub fn new() -> Self {
        Self {
            app_client: Arc::new(RwLock::new(None)),
            screenshot_cache: Arc::new(RwLock::new(ScreenshotCache::default())),
            image_cache: Arc::new(RwLock::new(ImageCache::default())),
            android_device: Arc::new(RwLock::new(None)),
            hover_tracker: Arc::new(RwLock::new(None)),
            screen_recorder: Arc::new(RwLock::new(None)),
            #[cfg(feature = "cdp")]
            cdp_client: Arc::new(RwLock::new(None)),
            #[cfg(target_os = "macos")]
            ax_session: Arc::new(crate::tools::ax_session::AxSession::new()),
        }
    }

    /// Build a [`ToolContext`] for the duration of one `call_tool`, capturing
    /// the request's `peer` so handlers can emit `notify_tool_list_changed`.
    fn tool_context(&self, peer: rmcp::service::Peer<RoleServer>) -> ToolContext {
        ToolContext {
            app_client: self.app_client.clone(),
            screenshot_cache: self.screenshot_cache.clone(),
            image_cache: self.image_cache.clone(),
            android_device: self.android_device.clone(),
            hover_tracker: self.hover_tracker.clone(),
            screen_recorder: self.screen_recorder.clone(),
            #[cfg(feature = "cdp")]
            cdp_client: self.cdp_client.clone(),
            #[cfg(target_os = "macos")]
            ax_session: self.ax_session.clone(),
            peer,
        }
    }

    async fn is_connected(&self) -> bool {
        self.app_client.read().await.is_some()
    }

    async fn is_android_connected(&self) -> bool {
        self.android_device.read().await.is_some()
    }

    async fn is_hover_tracking(&self) -> bool {
        self.hover_tracker.read().await.is_some()
    }

    async fn is_recording(&self) -> bool {
        self.screen_recorder.read().await.is_some()
    }

    #[cfg(feature = "cdp")]
    async fn is_cdp_connected(&self) -> bool {
        self.cdp_client.read().await.is_some()
    }

    /// Get tools available based on connection state.
    /// Base tools and app_connect are always available.
    /// Other app_* tools are only available when connected.
    ///
    /// CDP tools are always listed (independent of `cdp_connected`) so the
    /// tool surface does not mutate mid-session — clients that prompt-cache
    /// the tool list stay warm. Each CDP tool handler returns a clean
    /// "No CDP connection" error when called without an active connection.
    /// The `cdp_connected` parameter is accepted for API stability but is
    /// no longer used to gate visibility.
    pub fn get_tools(
        app_connected: bool,
        android_connected: bool,
        cdp_connected: bool,
        hover_tracking: bool,
        recording: bool,
    ) -> Vec<Tool> {
        let _ = cdp_connected;
        let state = ConnectionState {
            app_connected,
            android_connected,
            hover_tracking,
            recording,
        };
        let mut tools = ToolRegistry::build().schemas(&state);
        Self::apply_tool_annotations(&mut tools);
        tools
    }

    /// Attach MCP safety-hint annotations (readOnlyHint, destructiveHint,
    /// idempotentHint, openWorldHint) to every tool in the list.
    ///
    /// Classification keys off tool *name* (not description or schema) so
    /// it's stable across schema edits. Tool names absent from `tools`
    /// (conditional groups gated by connection state) are ignored —
    /// `test_every_tool_has_annotations` catches any unclassified tool.
    fn apply_tool_annotations(tools: &mut [Tool]) {
        // Read-only queries: screenshots, snapshots, finds, metadata.
        annotate_tools(
            tools,
            &[
                "take_screenshot",
                "list_windows",
                "list_apps",
                "get_displays",
                "find_text",
                "element_at_point",
                "find_image",
                "probe_app",
                "android_list_devices",
                "app_get_info",
                "app_get_tree",
                "app_query",
                "app_get_element",
                "app_list_windows",
                "app_screenshot",
                "android_screenshot",
                "android_find_text",
                "android_list_apps",
                "android_get_display_info",
                "android_get_current_activity",
            ],
            annotate_read_only(),
        );

        // Non-destructive state changes: clicks, typing, launches, sessions.
        annotate_tools(
            tools,
            &[
                "focus_window",
                "launch_app",
                "click",
                "move_mouse",
                "drag",
                "scroll",
                "type_text",
                "press_key",
                "load_image",
                "app_connect",
                "android_connect",
                "start_hover_tracking",
                "start_recording",
                "app_disconnect",
                "app_click",
                "app_type",
                "app_press_key",
                "app_focus",
                "app_focus_window",
                "android_disconnect",
                "android_click",
                "android_swipe",
                "android_type_text",
                "android_press_key",
                "android_launch_app",
                "get_hover_events",
                "stop_hover_tracking",
                "stop_recording",
            ],
            annotate_state_change(),
        );

        // take_ax_snapshot is state-changing on both platforms: macOS bumps
        // the session generation on every call (invalidating prior uids);
        // Windows advertises the same posture for a uniform client-safety
        // contract even though the underlying UIA read is still pure.
        annotate_tools(tools, &["take_ax_snapshot"], annotate_state_change());

        #[cfg(target_os = "macos")]
        {
            annotate_tools(
                tools,
                &["ax_click", "ax_set_value", "ax_select"],
                annotate_state_change(),
            );
        }

        annotate_tools(tools, &["quit_app"], annotate_destructive());

        #[cfg(feature = "cdp")]
        {
            annotate_tools(
                tools,
                &[
                    "cdp_take_dom_snapshot",
                    "cdp_summarize_page",
                    "cdp_find_elements",
                    "cdp_get_element_context",
                    "cdp_list_pages",
                    "cdp_element_at_point",
                    "cdp_wait_for",
                    "cdp_wait_for_page_change",
                ],
                annotate_read_only(),
            );
            annotate_tools(
                tools,
                &[
                    "cdp_connect",
                    "cdp_disconnect",
                    "cdp_click",
                    "cdp_hover",
                    "cdp_fill",
                    "cdp_press_key",
                    "cdp_handle_dialog",
                    "cdp_type_text",
                    "cdp_select_page",
                ],
                annotate_state_change(),
            );
            annotate_tools(
                tools,
                &["cdp_launch", "cdp_navigate", "cdp_new_page"],
                annotate_open_world_state_change(),
            );
            annotate_tools(
                tools,
                &["cdp_evaluate_script"],
                annotate_open_world_destructive(),
            );
            annotate_tools(tools, &["cdp_close_page"], annotate_destructive());
        }
    }
}

impl ServerHandler for MacOSDevToolsServer {
    fn get_info(&self) -> ServerInfo {
        let mut instructions = String::from(
            "Native DevTools MCP server for automating desktop apps (macOS/Windows) and Android devices.\n\n\
             WHICH TOOLS TO USE:\n\
             - Desktop apps (coordinate-based, cross-platform): no prefix (click, find_text, take_screenshot, type_text, etc.). Moves the cursor; steals focus.\n",
        );

        #[cfg(target_os = "macos")]
        {
            instructions.push_str(
                "- Desktop apps (element-precise, macOS only): ax_* (ax_click, ax_set_value, ax_select) — focus-preserving dispatch against uids from take_ax_snapshot.\n",
            );
        }

        instructions.push_str(
            "- Android devices: android_* (android_click, android_find_text, etc.)\n\
             - App debug protocol: app_* — only when given a WebSocket URL to connect to.\n\
             NEVER mix these — desktop tools do not work on Android and vice versa.\n\n\
             == DESKTOP (macOS/Windows) ==\n\n\
             CLICKING BY TEXT (PREFERRED): Use find_text to locate UI elements by name, \
             then click at the returned coordinates.\n\
             Example: find_text(text='Submit') → click(x=..., y=...).\n\n\
             CLICKING BY VISUAL POSITION: Use take_screenshot with include_ocr=true. \
             The OCR results include screen coordinates you can click directly. \
             For positions not covered by OCR, use the screenshot metadata \
             (origin_x, origin_y, scale) to convert pixel positions.\n\n\
             Always call focus_window before clicking to ensure the target window receives input.\n\n\
             Screenshot best practice: Use take_screenshot with app_name (e.g., app_name='Code') \
             to capture a specific window. Avoid mode='screen' unless you need to see multiple windows.\n\n",
        );

        #[cfg(target_os = "macos")]
        {
            instructions.push_str(
                "ELEMENT-PRECISE AUTOMATION (macOS, PREFERRED for native apps): \
                 Call take_ax_snapshot(app_name='...') to get a tree of elements tagged \
                 with generation-stamped uids like 'a42g3'. Then pick the dispatch \
                 primitive that matches the target: ax_click(uid) dispatches AXPress for \
                 buttons, menu items, and anything pressable; ax_set_value(uid, text) \
                 writes kAXValueAttribute on text fields; ax_select(uid) writes \
                 AXSelectedRows for NSOutlineView / NSTableView row selection (sidebars, \
                 rule lists, file browsers) — rows typically refuse AXPress so ax_click \
                 returns not_dispatchable or AX error -25205 against them. IMPORTANT: any \
                 fresh take_ax_snapshot invalidates all prior uids — snapshot immediately \
                 before each ax_click / ax_set_value / ax_select call. ax_set_value is \
                 value assignment, not keystrokes: no IME, no undo-stack entry. If a call \
                 fails with not_dispatchable and returns a fallback {x, y}, retry via \
                 click(x, y) (plus type_text(text) for ax_set_value).\n\n",
            );
        }

        instructions.push_str(
            "App debug protocol (app_* tools): For element-level precision in apps with an embedded \
             debug server. Use app_connect with a WebSocket URL first, then app_click, app_type, etc.\n\n\
             == ANDROID ==\n\n\
             All Android tools require connecting to a device first:\n\
             1. android_list_devices — find available devices and their serial numbers\n\
             2. android_connect(serial='...') — connect (this unlocks all other android_* tools)\n\
             To switch devices, call android_disconnect first, then android_connect to the new device.\n\n\
             CLICKING BY TEXT (PREFERRED): Use android_find_text to search the accessibility tree, \
             then android_click at the returned coordinates.\n\
             Example: android_find_text(text='Settings') → android_click(x=..., y=...).\n\n\
             CLICKING BY VISUAL POSITION: Use android_screenshot to see the screen, \
             then android_click at the desired coordinates.\n\
             Note: android_screenshot has no OCR — always prefer android_find_text for text elements.\n\n\
             Android coordinates are absolute screen pixels — no scale conversion needed.\n\
             Use android_press_key with Android keycodes (e.g., 'KEYCODE_BACK', 'KEYCODE_HOME').",
        );

        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
            server_info: Implementation {
                name: "native-devtools-mcp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            instructions: Some(instructions),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let connected = self.is_connected().await;
        #[cfg(feature = "cdp")]
        let cdp_connected = self.is_cdp_connected().await;
        #[cfg(not(feature = "cdp"))]
        let cdp_connected = false;
        Ok(ListToolsResult {
            tools: Self::get_tools(
                connected,
                self.is_android_connected().await,
                cdp_connected,
                self.is_hover_tracking().await,
                self.is_recording().await,
            ),
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = request
            .arguments
            .map(Value::Object)
            .unwrap_or(Value::Object(Default::default()));

        let registry = ToolRegistry::build();
        match registry.get(request.name.as_ref()) {
            Some(handler) => {
                let ctx = self.tool_context(context.peer);
                handler.call(args, &ctx).await
            }
            None => Err(McpError::invalid_params(
                format!("Unknown tool: {}", request.name),
                None,
            )),
        }
    }
}

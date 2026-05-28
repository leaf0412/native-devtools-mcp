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

/// Serialize a value to pretty-printed JSON, returning a formatted error on failure.
fn to_json_pretty(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("Failed to serialize: {}", e))
}

/// Extract a required string field from a JSON value.
fn parse_string_field(args: &Value, field: &str) -> Result<String, McpError> {
    args.get(field)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| McpError::invalid_params(format!("missing required param: {}", field), None))
}

fn json_to_object(value: Value) -> rmcp::model::JsonObject {
    match value {
        Value::Object(map) => map,
        _ => Default::default(),
    }
}

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

/// Tool names that have been migrated to the [`ToolRegistry`]. The legacy
/// schema getters and `call_tool` match still contain entries that are being
/// moved batch by batch; this list is the coexistence seam — schemas and
/// dispatch for these names come from the registry, and the legacy paths are
/// filtered/short-circuited so the net tool set is invariant.
const MIGRATED: &[&str] = &[
    "take_screenshot",
    "list_windows",
    "list_apps",
    "focus_window",
    "launch_app",
    "quit_app",
    "probe_app",
    "click",
    "move_mouse",
    "drag",
    "scroll",
    "type_text",
    "press_key",
    "get_displays",
    "find_text",
    "element_at_point",
    "find_image",
    "load_image",
    "take_ax_snapshot",
    #[cfg(target_os = "macos")]
    "ax_click",
    #[cfg(target_os = "macos")]
    "ax_set_value",
    #[cfg(target_os = "macos")]
    "ax_select",
    "app_connect",
    "app_disconnect",
    "app_get_info",
    "app_get_tree",
    "app_get_element",
    "app_list_windows",
    "app_focus_window",
    "app_query",
    "app_click",
    "app_type",
    "app_press_key",
    "app_focus",
    "app_screenshot",
    #[cfg(feature = "cdp")]
    "cdp_connect",
    #[cfg(feature = "cdp")]
    "cdp_disconnect",
    #[cfg(feature = "cdp")]
    "cdp_navigate",
    #[cfg(feature = "cdp")]
    "cdp_new_page",
    #[cfg(feature = "cdp")]
    "cdp_list_pages",
    #[cfg(feature = "cdp")]
    "cdp_select_page",
    #[cfg(feature = "cdp")]
    "cdp_close_page",
    #[cfg(feature = "cdp")]
    "cdp_click",
    #[cfg(feature = "cdp")]
    "cdp_fill",
    #[cfg(feature = "cdp")]
    "cdp_type_text",
    #[cfg(feature = "cdp")]
    "cdp_press_key",
    #[cfg(feature = "cdp")]
    "cdp_hover",
    #[cfg(feature = "cdp")]
    "cdp_handle_dialog",
    #[cfg(feature = "cdp")]
    "cdp_element_at_point",
    "android_list_devices",
    "android_connect",
    "android_disconnect",
    "android_screenshot",
    "android_click",
    "android_type_text",
    "android_press_key",
    "android_swipe",
    "android_find_text",
    "android_list_apps",
    "android_launch_app",
    "android_get_display_info",
    "android_get_current_activity",
    "start_hover_tracking",
    "get_hover_events",
    "stop_hover_tracking",
    "start_recording",
    "stop_recording",
];

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
        let registry = ToolRegistry::build();

        // Registry-owned tools, filtered by availability.
        let mut tools = registry.schemas(&state);

        // Legacy getters minus already-migrated names — the coexistence seam.
        // As tools move to the registry their name lands in MIGRATED and is
        // dropped here, keeping the net set invariant.
        let mut legacy = Self::get_legacy_tools(app_connected, android_connected, hover_tracking, recording);
        legacy.retain(|t| !MIGRATED.contains(&t.name.as_ref()));
        tools.append(&mut legacy);

        Self::apply_tool_annotations(&mut tools);
        tools
    }

    /// Hand-written schema getters that have not yet been migrated to the
    /// registry. Shrinks one batch at a time until step 8 deletes it.
    fn get_legacy_tools(
        app_connected: bool,
        android_connected: bool,
        hover_tracking: bool,
        recording: bool,
    ) -> Vec<Tool> {
        let mut tools = Self::get_base_tools();
        if app_connected {
            tools.extend(Self::get_app_tools());
        }
        tools.extend(Self::get_android_base_tools());
        if android_connected {
            tools.extend(Self::get_android_tools());
        }
        #[cfg(feature = "cdp")]
        {
            tools.extend(Self::get_cdp_tools());
        }
        tools.extend(Self::get_hover_tracking_tools(hover_tracking));
        tools.extend(Self::get_recording_tools(recording));
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
                &["cdp_navigate", "cdp_new_page"],
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

    /// Tools that are always available (system tools, CGEvent tools, etc.).
    ///
    /// `take_ax_snapshot` and the macOS `ax_*` tools now live on the
    /// [`ToolRegistry`]; this getter holds the remaining legacy base tools
    /// (currently none) and stays as the seam the legacy union appends to.
    fn get_base_tools() -> Vec<Tool> {
        Vec::new()
    }

    /// App debug tools — all now live on the [`ToolRegistry`] (gated
    /// `WhenAppConnected`); this getter stays as the seam the legacy union
    /// appends to and currently emits nothing.
    fn get_app_tools() -> Vec<Tool> {
        Vec::new()
    }

    /// Android tools that are always available (device discovery and connection)
    fn get_android_base_tools() -> Vec<Tool> {
        // Migrated to the registry (android_list_devices, android_connect);
        // empty seam like get_base_tools / get_app_tools until step 8.
        Vec::new()
    }

    /// Android tools available only when a device is connected — all now live
    /// on the [`ToolRegistry`] (gated `WhenAndroidConnected`); this getter stays
    /// as the seam the legacy union appends to and currently emits nothing.
    fn get_android_tools() -> Vec<Tool> {
        Vec::new()
    }

    /// Hover tracking tools — all now live on the [`ToolRegistry`]
    /// (`start_hover_tracking` is `Always`; `get_hover_events` /
    /// `stop_hover_tracking` are gated `WhenHoverTracking`). This getter stays
    /// as the seam the legacy union appends to and currently emits nothing.
    fn get_hover_tracking_tools(_tracking_active: bool) -> Vec<Tool> {
        Vec::new()
    }

    /// Screen recording tools — all now live on the [`ToolRegistry`]
    /// (`start_recording` is `Always`; `stop_recording` is gated
    /// `WhenRecording`). This getter stays as the seam the legacy union appends
    /// to and currently emits nothing.
    fn get_recording_tools(_recording_active: bool) -> Vec<Tool> {
        Vec::new()
    }

    #[cfg(feature = "cdp")]
    const UID_DESC: &'static str =
        "Element UID from cdp_take_dom_snapshot or cdp_find_elements (d-prefixed)";

    fn get_cdp_tools() -> Vec<Tool> {
        vec![
            Tool::new(
                "cdp_take_dom_snapshot",
                "Take a full DOM snapshot of the selected browser page. Returns all interactive elements with UIDs prefixed 'd' (e.g., d1, d2). Use when you need the complete page structure — captures contenteditable editors, placeholder inputs, and custom widgets. For targeted lookups, prefer cdp_find_elements instead. UIDs are valid for cdp_click, cdp_fill, and other action tools.",
                Arc::new(json_to_object(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "max_nodes": {
                            "type": "integer",
                            "description": "Maximum number of nodes to return (default: 500)"
                        }
                    }
                }))),
            ),
            Tool::new(
                "cdp_summarize_page",
                "Return a compact page summary: URL, title, current page generation, and an inventory of interactive elements grouped by role with a few sample labels. Does not return element UIDs and does not overwrite the current DOM UID snapshot. Use this for orientation before issuing targeted cdp_find_elements queries.",
                Arc::new(json_to_object(serde_json::json!({
                    "type": "object",
                    "properties": {}
                }))),
            ),
            Tool::new(
                "cdp_find_elements",
                "PREFERRED discovery tool. Search the live DOM for interactive elements matching a text query across labels, visible text, values, placeholders, titles, alt text, and test ids. Returns UIDs prefixed 'd' (e.g., d1, d2), match provenance, visible/accessibility text evidence, parent context for disambiguation, viewport geometry, warnings, and a page-level inventory grouped by role. Always try this first — it gives focused results without flooding context. Use cdp_take_dom_snapshot only if you need the full page structure. UIDs are valid for cdp_click, cdp_fill, and other action tools.",
                Arc::new(json_to_object(serde_json::json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Text to search for across element labels, visible text, values, placeholders, titles, alt text, and test ids"
                        },
                        "role": {
                            "type": "string",
                            "description": "Optional role filter (e.g., 'textbox', 'button')"
                        },
                        "max_results": {
                            "type": "integer",
                            "description": "Maximum matches to return (default: 10)"
                        }
                    }
                }))),
            ),
            Tool::new(
                "cdp_get_element_context",
                "Expand a UID returned by the most recent cdp_find_elements or cdp_take_dom_snapshot call. Returns the stored match evidence, nearby snapshot matches, and bounded live DOM context around the element (ancestors, siblings, and children). Use this when targeted search returns multiple plausible matches or the match needs local context before acting.",
                Arc::new(json_to_object(serde_json::json!({
                    "type": "object",
                    "required": ["uid"],
                    "properties": {
                        "uid": {
                            "type": "string",
                            "description": (Self::UID_DESC)
                        },
                        "ancestor_depth": {
                            "type": "integer",
                            "description": "Maximum ancestor levels to include (default: 3, max: 8)"
                        },
                        "sibling_limit": {
                            "type": "integer",
                            "description": "Number of preceding/following siblings to include around the element (default: 2, max: 10)"
                        },
                        "child_limit": {
                            "type": "integer",
                            "description": "Maximum direct children to summarize (default: 8, max: 50)"
                        },
                        "max_chars": {
                            "type": "integer",
                            "description": "Maximum text characters per summarized node (default: 240, range: 40-1000)"
                        }
                    }
                }))),
            ),
            Tool::new(
                "cdp_evaluate_script",
                "Evaluate a JavaScript function in the selected browser page. Arbitrary script execution may mutate page or external state and is approval-gated by clients; prefer cdp_find_elements/cdp_take_dom_snapshot for discovery and typed cdp_* action tools for UI changes. Returns the response as JSON. Example without arguments: '() => document.title'. Example with element arguments: pass UIDs from cdp_take_dom_snapshot or cdp_find_elements via args to reference DOM elements, e.g., '(el) => el.innerText' with args=[{uid: 'd5'}].",
                Arc::new(json_to_object(serde_json::json!({
                    "type": "object",
                    "required": ["function"],
                    "properties": {
                        "function": {
                            "type": "string",
                            "description": "JavaScript function to evaluate (e.g., '() => document.title' or '(el) => el.innerText')"
                        },
                        "args": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "uid": { "type": "string", "description": (Self::UID_DESC) }
                                }
                            },
                            "description": "Optional element arguments from snapshot UIDs"
                        }
                    }
                }))),
            ),
            Tool::new(
                "cdp_wait_for",
                "Wait for the specified text to appear on the selected page. Resolves when any value appears.",
                Arc::new(json_to_object(serde_json::json!({
                    "type": "object",
                    "required": ["text"],
                    "properties": {
                        "text": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "description": "Non-empty list of texts. Resolves when any value appears on the page."
                        },
                        "timeout": {
                            "type": "integer",
                            "description": "Maximum wait time in milliseconds (default: 10000)"
                        },
                        "include_snapshot": {
                            "type": "boolean",
                            "description": "Appends a DOM snapshot (d-prefixed UIDs) to the response after the text appears (default: false). When false, only a short 'text appeared after Xms' line is returned."
                        }
                    }
                }))),
            ),
            Tool::new(
                "cdp_wait_for_page_change",
                "Wait until the selected page or a scoped DOM element has a semantic visible-text/editor-value change. Use this for unknown incoming messages, replies, notifications, or page updates after you have chosen the best scope_uid with cdp_find_elements/cdp_get_element_context. This blocks inside one tool call and returns compact before/after deltas.",
                Arc::new(json_to_object(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "scope_uid": {
                            "type": "string",
                            "description": "Optional d-prefixed uid to watch. Prefer a stable container such as a message list, inbox list, thread, panel, or editor. Omit only when a page-level wait is intentional."
                        },
                        "condition": {
                            "type": "string",
                            "description": "Coarse condition label for the wait, e.g. semantic_delta, new_visible_text, item_count_changed. Currently evaluated as semantic_delta and echoed in the result."
                        },
                        "goal": {
                            "type": "string",
                            "description": "Short natural-language goal to echo in the result so the next model turn can judge relevance, e.g. new incoming message in Note to Self."
                        },
                        "timeout": {
                            "type": "integer",
                            "description": "Maximum wait time in milliseconds (default: 55000, capped at 55000 to stay within the Clickweave MCP client timeout)"
                        },
                        "poll_interval_ms": {
                            "type": "integer",
                            "description": "Backup polling interval in milliseconds for changes MutationObserver cannot see (default: 500, clamped to 100-5000)"
                        },
                        "stable_ms": {
                            "type": "integer",
                            "description": "Debounce/stability window after a wake-up before comparing semantic text (default: 500, clamped to 100-2000)"
                        },
                        "include_snapshot": {
                            "type": "boolean",
                            "description": "Appends a compact DOM snapshot after the wait returns (default: false). The normal response already includes before/after text tails and deltas."
                        }
                    }
                }))),
            ),
        ]
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

        // Try the registry first; fall through to the legacy match for tools
        // not yet migrated. Migrated arms are deleted as they move.
        let registry = ToolRegistry::build();
        if let Some(handler) = registry.get(request.name.as_ref()) {
            let ctx = self.tool_context(context.peer);
            return handler.call(args, &ctx).await;
        }

        match request.name.as_ref() {
            #[cfg(feature = "cdp")]
            "cdp_take_dom_snapshot" => {
                let max_nodes = args
                    .get("max_nodes")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);
                Ok(
                    crate::cdp::tools::cdp_take_dom_snapshot(max_nodes, self.cdp_client.clone())
                        .await,
                )
            }
            #[cfg(feature = "cdp")]
            "cdp_summarize_page" => {
                Ok(crate::cdp::tools::cdp_summarize_page(self.cdp_client.clone()).await)
            }
            #[cfg(feature = "cdp")]
            "cdp_find_elements" => {
                let query = parse_string_field(&args, "query")?;
                let role = args.get("role").and_then(|v| v.as_str()).map(String::from);
                let max_results = args
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);
                Ok(crate::cdp::tools::cdp_find_elements(
                    query,
                    role,
                    max_results,
                    self.cdp_client.clone(),
                )
                .await)
            }
            #[cfg(feature = "cdp")]
            "cdp_get_element_context" => {
                let uid = parse_string_field(&args, "uid")?;
                let ancestor_depth = args
                    .get("ancestor_depth")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);
                let sibling_limit = args
                    .get("sibling_limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);
                let child_limit = args
                    .get("child_limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);
                let max_chars = args
                    .get("max_chars")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);
                Ok(crate::cdp::tools::cdp_get_element_context(
                    uid,
                    ancestor_depth,
                    sibling_limit,
                    child_limit,
                    max_chars,
                    self.cdp_client.clone(),
                )
                .await)
            }
            #[cfg(feature = "cdp")]
            "cdp_evaluate_script" => {
                let function = parse_string_field(&args, "function")?;
                let script_args = args.get("args").and_then(|v| v.as_array()).cloned();
                Ok(crate::cdp::tools::cdp_evaluate_script(
                    function,
                    script_args,
                    self.cdp_client.clone(),
                )
                .await)
            }
            #[cfg(feature = "cdp")]
            "cdp_wait_for" => {
                let texts: Vec<String> = args
                    .get("text")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if texts.is_empty() {
                    return Err(McpError::invalid_params(
                        "missing required param: text (array of strings)",
                        None,
                    ));
                }
                let timeout = args.get("timeout").and_then(|v| v.as_u64());
                let include_snapshot = args
                    .get("include_snapshot")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Ok(crate::cdp::tools::cdp_wait_for(
                    texts,
                    timeout,
                    include_snapshot,
                    self.cdp_client.clone(),
                )
                .await)
            }
            #[cfg(feature = "cdp")]
            "cdp_wait_for_page_change" => {
                let scope_uid = args
                    .get("scope_uid")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let condition = args
                    .get("condition")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let goal = args.get("goal").and_then(|v| v.as_str()).map(String::from);
                let timeout = args.get("timeout").and_then(|v| v.as_u64());
                let poll_interval_ms = args.get("poll_interval_ms").and_then(|v| v.as_u64());
                let stable_ms = args.get("stable_ms").and_then(|v| v.as_u64());
                let include_snapshot = args
                    .get("include_snapshot")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Ok(crate::cdp::tools::cdp_wait_for_page_change(
                    scope_uid,
                    condition,
                    goal,
                    timeout,
                    poll_interval_ms,
                    stable_ms,
                    include_snapshot,
                    self.cdp_client.clone(),
                )
                .await)
            }
            _ => Err(McpError::invalid_params(
                format!("Unknown tool: {}", request.name),
                None,
            )),
        }
    }
}

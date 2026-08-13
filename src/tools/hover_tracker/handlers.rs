//! MCP tool handlers for hover tracking.
//!
//! Each wraps the hover-tracking state machine with its name, schema, and
//! availability. `start_hover_tracking` is always visible (so the user can
//! begin a session); `get_hover_events` and `stop_hover_tracking` are gated
//! `WhenHoverTracking`, mirroring the deleted `get_hover_tracking_tools` + the
//! tracking gate in `get_tools`. Schema JSON moved verbatim from that getter;
//! call bodies copied verbatim from the deleted `call_tool` arms, with
//! `ctx.hover_tracker` / `ctx.peer` replacing `self.hover_tracker` /
//! `context.peer`. Every `notify_tool_list_changed` is preserved so the visible
//! tool set mutates on start/stop.

use std::sync::Arc;

use rmcp::{
    model::{CallToolResult, Content, Tool},
    Error as McpError,
};

use crate::tools::registry::{
    json_to_object, to_json_pretty, Availability, ToolContext, ToolHandler,
};

/// `start_hover_tracking` — always visible so a session can be started.
/// Fires `notify_tool_list_changed` so `get_hover_events` / `stop_hover_tracking`
/// become visible once tracking is active.
pub struct StartHoverTracking;

#[async_trait::async_trait]
impl ToolHandler for StartHoverTracking {
    fn name(&self) -> &'static str {
        "start_hover_tracking"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "start_hover_tracking",
            "Start tracking hover state changes. Polls cursor position and accessibility element at configurable intervals, recording transitions. Use get_hover_events to retrieve recorded events, and stop_hover_tracking to end the session. Only one tracking session can be active at a time.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "app_name": {
                        "type": "string",
                        "description": "Scope element lookup to a specific application (e.g., 'Safari'). Faster and avoids ambiguity."
                    },
                    "poll_interval_ms": {
                        "type": "integer",
                        "description": "Polling interval in milliseconds (default: 100)",
                        "default": 100
                    },
                    "max_duration_ms": {
                        "type": "integer",
                        "description": "Auto-stop after this many milliseconds (0 = unlimited — runs until stop_hover_tracking)",
                        "default": 0
                    },
                    "min_dwell_ms": {
                        "type": "integer",
                        "description": "Minimum time (ms) cursor must stay on a new element before recording a transition. Filters out pass-through elements during fast mouse movement. 0 = record every change immediately. (default: 300)",
                        "default": 300
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
        // Auto-clean finished tracker (e.g. from max duration timeout)
        let already_active = {
            let guard = ctx.hover_tracker.read().await;
            match guard.as_ref() {
                Some(t) if t.is_finished() => false, // will clean up below
                Some(_) => true,
                None => false,
            }
        };
        if already_active {
            return Ok(CallToolResult::error(vec![Content::text(
                "Hover tracking is already active. Use stop_hover_tracking to end the current session first.",
            )]));
        }
        // Clean up any finished tracker before starting a new one
        if ctx.hover_tracker.read().await.is_some() {
            ctx.hover_tracker.write().await.take();
        }

        let app_name = args
            .get("app_name")
            .and_then(|v| v.as_str())
            .map(String::from);
        let poll_interval_ms = args
            .get("poll_interval_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(100)
            .clamp(10, 10_000) as u32;
        let max_duration_ms = args
            .get("max_duration_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let max_duration_ms = if max_duration_ms == 0 {
            u32::MAX as u64
        } else {
            max_duration_ms.clamp(100, u32::MAX as u64)
        } as u32;
        let min_dwell_ms = args
            .get("min_dwell_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(300)
            .clamp(0, 10_000) as u32;

        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let cancel = tokio_util::sync::CancellationToken::new();

        let task_handle = crate::tools::hover_tracker::start_polling(
            events.clone(),
            cancel.clone(),
            app_name.clone(),
            poll_interval_ms,
            max_duration_ms,
            min_dwell_ms,
        );

        let tracker = crate::tools::hover_tracker::HoverTracker::new(events, task_handle, cancel);
        *ctx.hover_tracker.write().await = Some(tracker);
        let _ = ctx.peer.notify_tool_list_changed().await;

        let msg = format!(
            "Hover tracking started (poll: {}ms, max: {}ms, dwell: {}ms{}). Use get_hover_events to read transitions, stop_hover_tracking to end.",
            poll_interval_ms,
            max_duration_ms,
            min_dwell_ms,
            app_name.map_or(String::new(), |a| format!(", app: {}", a)),
        );
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }
}

/// `get_hover_events` — visible only while tracking. Drains buffered events and,
/// if the session auto-stopped, clears state and fires `notify_tool_list_changed`.
pub struct GetHoverEvents;

#[async_trait::async_trait]
impl ToolHandler for GetHoverEvents {
    fn name(&self) -> &'static str {
        "get_hover_events"
    }

    fn availability(&self) -> Availability {
        Availability::WhenHoverTracking
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "get_hover_events",
            "Retrieve and drain buffered hover events since the last call. Returns a JSON array of transition events, each with cursor position, element info, timestamp, and dwell time. Events are consumed — subsequent calls return only new events.",
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
        // Single lock: check auto-stop and drain events together
        let result = {
            let guard = ctx.hover_tracker.read().await;
            guard.as_ref().map(|t| {
                let auto_stopped = t.is_finished();
                let events = t.drain_events();
                (auto_stopped, events)
            })
        };

        match result {
            Some((auto_stopped, events)) => {
                let json = to_json_pretty(&events);

                if auto_stopped {
                    ctx.hover_tracker.write().await.take();
                    let _ = ctx.peer.notify_tool_list_changed().await;
                }

                // Always return the JSON array for consistent parsing.
                // The timeout sentinel event (with timeout: true) signals
                // auto-stop within the event stream itself.
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            None => Ok(CallToolResult::error(vec![Content::text(
                "No hover tracking session is active. Use start_hover_tracking first.",
            )])),
        }
    }
}

/// `stop_hover_tracking` — visible only while tracking. Cancels the background
/// task, drains remaining events, and fires `notify_tool_list_changed`.
pub struct StopHoverTracking;

#[async_trait::async_trait]
impl ToolHandler for StopHoverTracking {
    fn name(&self) -> &'static str {
        "stop_hover_tracking"
    }

    fn availability(&self) -> Availability {
        Availability::WhenHoverTracking
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "stop_hover_tracking",
            "Stop hover tracking and return any remaining buffered events. Ends the background polling task.",
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
        let tracker = ctx.hover_tracker.write().await.take();
        match tracker {
            Some(tracker) => {
                let events = tracker.cancel_and_drain().await;
                let _ = ctx.peer.notify_tool_list_changed().await;
                // Return raw JSON array for consistent parsing with get_hover_events
                Ok(CallToolResult::success(vec![Content::text(
                    to_json_pretty(&events),
                )]))
            }
            None => Ok(CallToolResult::error(vec![Content::text(
                "No hover tracking session is active.",
            )])),
        }
    }
}

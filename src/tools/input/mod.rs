//! Input tools for system-level mouse and keyboard simulation.
//!
//! These tools wrap input operations in `spawn_blocking` to avoid
//! blocking the tokio runtime, since input operations use `thread::sleep`.

mod click;
mod click_variant;
mod diagnostics;
mod pointer;
mod query;

pub use click::*;
pub use diagnostics::*;
pub use pointer::*;
pub use query::*;

use crate::platform::input;
use rmcp::model::{CallToolResult, Content};

/// Snapshot of the host process's Accessibility trust, for inclusion in
/// structured input errors so callers can tell a permission problem apart
/// from a target/event/verification problem.
pub(crate) fn current_permission_status() -> PermissionStatus {
    PermissionStatus {
        accessibility_trusted: input::check_accessibility_permission(),
    }
}

/// Check accessibility permission and return a structured error result if not
/// granted. Returns `None` when permission is available.
///
/// Shared between the coord-based input tools (`click`, `type_text`, …) and
/// the macOS AX dispatch tools (`ax_click`, `ax_set_value`) so the
/// permission-stage failure is identical across tools.
///
/// Note: this only reports the *permission-probe* stage. A `false` here means
/// the host process is untrusted; it does NOT mean a particular target window
/// or event path failed. Downstream stages report their own codes.
pub(crate) fn check_permission() -> Option<CallToolResult> {
    if !input::check_accessibility_permission() {
        let perm = PermissionStatus {
            accessibility_trusted: false,
        };
        return Some(diagnostics::error(
            InputErrorCode::AccessibilityUntrusted,
            "Accessibility permission not granted to the app that runs this MCP server. \
             Grant it to the host app (e.g. Terminal, VS Code, Claude Desktop, Ghostty) in \
             System Settings → Privacy & Security → Accessibility, then restart that app. \
             The permission must be granted to the app that runs this server, not to the \
             server binary itself.",
            Some(&perm),
        ));
    }
    None
}

/// Run a blocking platform query that already produces a `CallToolResult`
/// (e.g. `find_text`, `element_at_point`, `get_displays`) off the async
/// executor via `spawn_blocking`, so a slow accessibility/UIA/OCR call doesn't
/// pin a tokio worker thread and stall every other tool.
pub(super) async fn run_blocking_query<F>(op: F) -> CallToolResult
where
    F: FnOnce() -> CallToolResult + Send + 'static,
{
    tokio::task::spawn_blocking(op)
        .await
        .unwrap_or_else(|e| CallToolResult::error(vec![Content::text(format!("Task failed: {}", e))]))
}

/// Run a blocking input operation and convert the result to CallToolResult.
pub(super) async fn run_input<F>(op: F, success_msg: String, error_prefix: &str) -> CallToolResult
where
    F: FnOnce() -> Result<(), String> + Send + 'static,
{
    match tokio::task::spawn_blocking(op).await {
        Ok(Ok(())) => CallToolResult::success(vec![Content::text(success_msg)]),
        Ok(Err(e)) => {
            CallToolResult::error(vec![Content::text(format!("{}: {}", error_prefix, e))])
        }
        Err(e) => CallToolResult::error(vec![Content::text(format!("Task failed: {}", e))]),
    }
}

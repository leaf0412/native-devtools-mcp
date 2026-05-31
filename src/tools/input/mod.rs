//! Input tools for system-level mouse and keyboard simulation.
//!
//! These tools wrap input operations in `spawn_blocking` to avoid
//! blocking the tokio runtime, since input operations use `thread::sleep`.

mod click;
mod click_variant;
mod pointer;
mod query;

pub use click::*;
pub use pointer::*;
pub use query::*;

use crate::platform::input;
use rmcp::model::{CallToolResult, Content};

/// Check accessibility permission and return a standardized plain-text error
/// result if not granted. Returns `None` when permission is available.
///
/// Shared between the coord-based input tools (`click`, `type_text`, …) and
/// the macOS AX dispatch tools (`ax_click`, `ax_set_value`) so all user-facing
/// permission messages are identical.
pub(crate) fn check_permission() -> Option<CallToolResult> {
    if !input::check_accessibility_permission() {
        #[cfg(target_os = "macos")]
        let msg = "Accessibility permission required.\n\n\
             Grant permission to your MCP client (e.g., Claude Desktop, VS Code, Terminal) in:\n\
             System Settings → Privacy & Security → Accessibility\n\n\
             The permission must be granted to the app that runs this MCP server, \
             not to the server binary itself.";

        #[cfg(target_os = "windows")]
        let msg = "Input injection permission denied.\n\n\
             This typically occurs when targeting elevated (admin) windows \
             from a non-elevated process, or when targeting secure desktops.";

        return Some(CallToolResult::error(vec![Content::text(msg)]));
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

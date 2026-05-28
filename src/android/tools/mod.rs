//! MCP tool handlers for Android device, screenshot, and input tools.
//!
//! Each struct wraps the old `call_tool` arm body verbatim, with `ctx.android_device`
//! / `ctx.peer` replacing `self.android_device` / `context.peer`. `with_android_device`
//! moved here from the `MacOSDevToolsServer` impl as a free async function so handlers
//! can acquire the device lock without `&self`.
//!
//! Availability mirrors the deleted `get_android_base_tools` (Always) and
//! `get_android_tools` (WhenAndroidConnected) + the connection gate in `get_tools`:
//! `android_list_devices` / `android_connect` are always visible so the user can
//! list and connect; the rest are gated behind a live connection.

mod device;
mod query;

pub use device::*;
pub use query::*;

use crate::android::AndroidDevice;
use rmcp::model::{CallToolResult, Content};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Acquire the android device lock and call `f` with a mutable reference.
/// Returns a "not connected" error result if no device is connected.
pub async fn with_android_device<F>(
    android_device: Arc<RwLock<Option<AndroidDevice>>>,
    f: F,
) -> CallToolResult
where
    F: FnOnce(&mut AndroidDevice) -> CallToolResult,
{
    let mut guard = android_device.write().await;
    match guard.as_mut() {
        Some(device) => f(device),
        None => CallToolResult::error(vec![Content::text(
            "No Android device connected. Use android_connect first.",
        )]),
    }
}

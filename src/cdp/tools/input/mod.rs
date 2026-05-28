//! CDP input tools: click, hover, fill, press_key, type_text.

mod keyboard;
mod pointer;

pub use keyboard::*;
pub use pointer::*;

use crate::cdp::CdpClient;
use rmcp::model::CallToolResult;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Cap for snapshots auto-appended after an action. Smaller than the
/// user-facing `cdp_take_dom_snapshot` default (500) because the "quick
/// look after click/hover/fill" use case doesn't need the full page, and
/// every extra element costs three CDP round trips.
pub(super) const AUTO_SNAPSHOT_MAX_NODES: u32 = 100;

/// Append a snapshot to an existing tool result if `include_snapshot` is true.
pub(super) async fn maybe_append_snapshot(
    mut result: CallToolResult,
    include_snapshot: bool,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    if !include_snapshot {
        return result;
    }
    let snapshot =
        super::script::cdp_take_dom_snapshot(Some(AUTO_SNAPSHOT_MAX_NODES), cdp_client).await;
    result.content.extend(snapshot.content);
    result
}

pub(super) async fn invalidate_snapshot_cache(cdp_client: Arc<RwLock<Option<CdpClient>>>) {
    if let Some(client) = cdp_client.write().await.as_mut() {
        client.invalidate_snapshots();
    }
}

pub(super) async fn finish_after_action(
    result: CallToolResult,
    include_snapshot: bool,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    invalidate_snapshot_cache(cdp_client.clone()).await;
    maybe_append_snapshot(result, include_snapshot, cdp_client).await
}

pub(super) fn observed_fill_status(
    strategy: &str,
    observed_text: &str,
    value: &str,
) -> &'static str {
    let matched = if strategy == "select_value" {
        observed_text
            .lines()
            .any(|part| part.trim() == value || part.contains(value))
    } else {
        observed_text.contains(value)
    };
    if matched {
        "observed_text=true"
    } else {
        "observed_text=false"
    }
}

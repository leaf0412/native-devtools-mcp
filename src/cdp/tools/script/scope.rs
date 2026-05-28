//! Shared helpers for resolving DOM scope and shaping CDP call arguments.
//!
//! These helpers exist to dedupe the guard/match/`require_page` boilerplate
//! and the two-step UID resolution (`resolve_uid_from_maps` then
//! `DOM.resolveNode`) that the wait code repeats. The legacy callers in
//! `summary.rs` and `evaluate.rs` inline the same patterns; those stay as-is
//! per the scalpel rule (TODO: migrate when those modules need touching).

use crate::cdp::{cdp_error, page_url, CdpClient};
use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, ResolveNodeParams};
use chromiumoxide::cdp::js_protocol::runtime::{CallArgument, ReleaseObjectParams, RemoteObjectId};
use chromiumoxide::page::Page;
use rmcp::model::CallToolResult;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A scope UID fully resolved to a live remote object on the page.
///
/// `uid` and `backend_node_id` are part of the resolver's public contract
/// even though today only `object_id` is consumed (wait code). Keeping them
/// lets a future scope-aware caller use them without another round-trip;
/// the `allow(dead_code)` is honest about that intent.
pub(super) struct ResolvedScope {
    #[allow(dead_code)]
    pub uid: String,
    #[allow(dead_code)]
    pub backend_node_id: i64,
    pub object_id: RemoteObjectId,
}

/// Build a `CallArgument` from a primitive JSON value.
///
/// Replaces the old `string_argument` helper: the name was misleading
/// (callers always passed numbers, never strings) and the new name reflects
/// the actual contract.
pub(super) fn json_value_arg(value: impl Into<serde_json::Value>) -> CallArgument {
    CallArgument::builder().value(value.into()).build()
}

/// Best-effort release of a remote object after a call. Errors are
/// intentionally swallowed: there is nothing useful the caller can do
/// (the object may already be gone, the page may have navigated, etc.),
/// and this is a cleanup-only side effect.
pub(super) async fn release_object(page: &Page, object_id: RemoteObjectId) {
    let _ = page.execute(ReleaseObjectParams::new(object_id)).await;
}

/// Acquire a `Page` from the shared CDP client, surfacing the
/// "No CDP connection" error verbatim if the client is absent.
pub(super) async fn borrow_page(
    cdp_client: &Arc<RwLock<Option<CdpClient>>>,
) -> Result<Page, CallToolResult> {
    let guard = cdp_client.read().await;
    let client = guard
        .as_ref()
        .ok_or_else(|| cdp_error("No CDP connection. Use cdp_connect first."))?;
    client.require_page()
}

/// Resolve a scope UID end-to-end: snapshot lookup -> `backendNodeId` ->
/// `DOM.resolveNode` -> live `objectId`. Returns one struct instead of the
/// two-step pair the old code used.
pub(super) async fn resolve_element_scope(
    uid: &str,
    page: &Page,
    cdp_client: &Arc<RwLock<Option<CdpClient>>>,
) -> Result<ResolvedScope, CallToolResult> {
    let backend_node_id = resolve_scope_backend_node_id(uid, page, cdp_client.clone()).await?;
    let object_id = resolve_scope_object_id(uid, backend_node_id, page).await?;
    Ok(ResolvedScope {
        uid: uid.to_string(),
        backend_node_id,
        object_id,
    })
}

/// Step-1 helper retained as a thin wrapper so existing wait code in
/// `mod.rs` still compiles unchanged until step 4 moves to `wait.rs`.
pub(super) async fn resolve_scope_backend_node_id(
    scope_uid: &str,
    page: &Page,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> Result<i64, CallToolResult> {
    let current_url = page_url(page).await;
    let guard = cdp_client.read().await;
    let client = guard
        .as_ref()
        .ok_or_else(|| cdp_error("No CDP connection. Use cdp_connect first."))?;
    let node = crate::cdp::resolve_uid_from_maps(
        scope_uid,
        client.last_dom_snapshot.as_ref(),
        client.generation,
        &current_url,
    )
    .map_err(cdp_error)?;

    Ok(node.backend_node_id)
}

/// Step-2 helper retained as a thin wrapper so existing wait code in
/// `mod.rs` still compiles unchanged until step 4 moves to `wait.rs`.
pub(super) async fn resolve_scope_object_id(
    scope_uid: &str,
    backend_node_id: i64,
    page: &Page,
) -> Result<RemoteObjectId, CallToolResult> {
    let resolve_params = ResolveNodeParams::builder()
        .backend_node_id(BackendNodeId::new(backend_node_id))
        .build();
    let remote_object = page.execute(resolve_params).await.map_err(|e| {
        cdp_error(format!(
            "Scope uid={} could not be resolved to a DOM node: {}",
            scope_uid, e
        ))
    })?;
    remote_object.result.object.object_id.ok_or_else(|| {
        cdp_error(format!(
            "Scope uid={} could not be resolved to a DOM node.",
            scope_uid
        ))
    })
}

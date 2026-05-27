//! `ax_select` tool — write `AXSelectedRows` on the outline/table that
//! encloses the uid-targeted element. This is the row-selection dispatch
//! primitive for `NSOutlineView` / `NSTableView` sidebars, where
//! `AXPress` is not a supported action and coordinate-based clicks would
//! steal focus.
//!
//! Accepts a uid that points at the row itself, a cell within the row, or
//! any descendant of the row. The handler walks up the `AXParent` chain
//! to find the enclosing `AXRow` and then the enclosing `AXOutline` or
//! `AXTable`, then writes `AXSelectedRows = [row]` on that container.
//!
//! The row-resolution walk and the attribute write both execute inside
//! the `AxSession::dispatch` closure so the session's read lock is held
//! across the whole operation — a concurrent `take_ax_snapshot` cannot
//! publish a fresh generation mid-select.

use crate::macos::ax::{
    ancestor_role_chain, element_bbox, select_rows_attribute, AXDispatchError, AXRef,
};
use crate::tools::ax_response;
use crate::tools::ax_row_target::{resolve_row_and_container, RowResolution};
use crate::tools::ax_session::{AxSession, LookupError};
use rmcp::model::CallToolResult;
use serde::Deserialize;
use std::sync::Arc;

const DISPATCHED_VIA: &str = "AXSelectedRows";

#[derive(Deserialize)]
pub struct AxSelectParams {
    pub uid: String,
}

pub async fn ax_select(params: AxSelectParams, session: Arc<AxSession>) -> CallToolResult {
    if let Some(err) = crate::tools::input::check_permission() {
        return err;
    }

    // `dispatch` holds the session's read lock across the closure so a
    // concurrent `take_ax_snapshot` cannot publish a fresh generation
    // mid-select. The walk to the enclosing row + the attribute write
    // both execute under the held lock.
    let outcome = session
        .dispatch(&params.uid, |ax_ref: &AXRef| {
            // Capture pre-dispatch bbox so error envelopes have a fallback
            // centre even when the walk cannot resolve a container.
            let pre_bbox = unsafe { element_bbox(ax_ref.as_raw()) };

            let chain = ancestor_role_chain(ax_ref);
            let role_strs: Vec<Option<&str>> = chain.iter().map(|(_, r)| r.as_deref()).collect();

            let (row_idx, container_idx) = match resolve_row_and_container(&role_strs) {
                RowResolution::Resolved {
                    row_idx,
                    container_idx,
                } => (row_idx, container_idx),
                RowResolution::NoRow => {
                    return ax_response::error(
                        "no_row_ancestor",
                        "uid does not resolve to an element inside an AXRow; \
                         ax_select only applies to NSOutlineView / NSTableView rows",
                        pre_bbox,
                    );
                }
                RowResolution::NoContainer { row_idx } => {
                    let row_bbox = unsafe { element_bbox(chain[row_idx].0.as_raw()) }.or(pre_bbox);
                    return ax_response::error(
                        "no_outline_container",
                        "row is not nested inside an AXOutline or AXTable; \
                         nothing to write AXSelectedRows to",
                        row_bbox,
                    );
                }
            };

            let row = &chain[row_idx].0;
            let container = &chain[container_idx].0;
            let row_bbox = unsafe { element_bbox(row.as_raw()) }.or(pre_bbox);

            match select_rows_attribute(container, &[row]) {
                Ok(()) => {
                    let post_bbox = unsafe { element_bbox(row.as_raw()) }.or(row_bbox);
                    ax_response::success(DISPATCHED_VIA, post_bbox)
                }
                Err(AXDispatchError::NotDispatchable) => ax_response::error(
                    "not_dispatchable",
                    "container does not expose a writable AXSelectedRows attribute",
                    row_bbox,
                ),
                Err(AXDispatchError::AxError(code)) => ax_response::error(
                    "ax_error",
                    &format!(
                        "AXUIElementSetAttributeValue(AXSelectedRows) failed with AX error {}",
                        code
                    ),
                    row_bbox,
                ),
            }
        })
        .await;

    match outcome {
        Ok(result) => result,
        Err(LookupError::SnapshotExpired { reason }) => {
            ax_response::error("snapshot_expired", &reason, None)
        }
        Err(LookupError::UidNotFound) => ax_response::error(
            "uid_not_found",
            &format!("uid {} is not present in the current snapshot", params.uid),
            None,
        ),
    }
}

use crate::tools::registry::{json_to_object, ToolContext, ToolHandler};
use rmcp::{model::Tool, Error as McpError};

/// `ax_select` MCP tool handler (macOS only).
pub struct AxSelect;

#[async_trait::async_trait]
impl ToolHandler for AxSelect {
    fn name(&self) -> &'static str {
        "ax_select"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "ax_select",
            "macOS only. Select a row inside an NSOutlineView / NSTableView (sidebars, \
             rule lists, file browsers) by writing AXSelectedRows on the enclosing \
             outline or table. Accepts a uid pointing at the row, a cell inside the row, \
             or any descendant — the tool walks up to the enclosing AXRow then the \
             enclosing AXOutline / AXTable. Does not move the cursor and does not steal \
             focus. Use ax_select instead of ax_click for row targets: rows typically \
             refuse AXPress (returning not_dispatchable or AX error -25205), and a \
             coordinate click steals focus. On failure the tool returns { error: { code, \
             message, fallback: { x, y } | null } } with codes snapshot_expired, \
             uid_not_found, no_row_ancestor, no_outline_container, not_dispatchable, or \
             ax_error. The fallback centre falls back from the row bbox to the \
             originally-targeted element bbox so the caller can still click(x, y) if \
             desired.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["uid"],
                "properties": {
                    "uid": {
                        "type": "string",
                        "description": "Element uid from the most recent take_ax_snapshot. Points at the row itself, a cell within the row, or any descendant."
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
        let params: AxSelectParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(ax_select(params, ctx.ax_session.clone()).await)
    }
}

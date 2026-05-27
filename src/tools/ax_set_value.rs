//! `ax_set_value` tool — write `kAXValueAttribute` on an element resolved
//! from a generation-tagged uid.
//!
//! This is value assignment, not key-event typing: no IME composition, no
//! keydown/keyup events, no app undo-stack entry. Callers that need those
//! semantics must fall back to `click(x, y)` + `type_text(text)` using the
//! `fallback` centre returned on `not_dispatchable`.

use crate::macos::ax::{element_bbox, set_value_attribute, AXDispatchError, AXRef};
use crate::tools::ax_response;
use crate::tools::ax_session::{AxSession, LookupError};
use rmcp::model::CallToolResult;
use serde::Deserialize;
use std::sync::Arc;

const DISPATCHED_VIA: &str = "AXSetAttributeValue";

#[derive(Deserialize)]
pub struct AxSetValueParams {
    pub uid: String,
    pub text: String,
}

pub async fn ax_set_value(params: AxSetValueParams, session: Arc<AxSession>) -> CallToolResult {
    if let Some(err) = crate::tools::input::check_permission() {
        return err;
    }

    // `dispatch` holds the session's read lock across the closure so a
    // concurrent `take_ax_snapshot` cannot publish a fresh generation
    // mid-write. This is the atomic half of the "fresh snapshot invalidates
    // prior uids" contract.
    let outcome = session
        .dispatch(&params.uid, |ax_ref: &AXRef| {
            // Capture pre-dispatch bbox so `not_dispatchable` / `ax_error`
            // still get a fallback centre if the element disappears
            // mid-dispatch.
            let pre_bbox = unsafe { element_bbox(ax_ref.as_raw()) };

            match set_value_attribute(ax_ref, &params.text) {
                Ok(()) => {
                    // On success, return the element's CURRENT bbox for
                    // telemetry. The snapshot's bbox remains the pre-action
                    // animation source on the client side. Fall back to
                    // pre_bbox if the element no longer exposes
                    // position+size post-dispatch.
                    let post_bbox = unsafe { element_bbox(ax_ref.as_raw()) }.or(pre_bbox);
                    ax_response::success(DISPATCHED_VIA, post_bbox)
                }
                Err(AXDispatchError::NotDispatchable) => ax_response::error(
                    "not_dispatchable",
                    "element does not expose a writable kAXValueAttribute",
                    pre_bbox,
                ),
                Err(AXDispatchError::AxError(code)) => ax_response::error(
                    "ax_error",
                    &format!("AXUIElementSetAttributeValue failed with AX error {}", code),
                    pre_bbox,
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

/// `ax_set_value` MCP tool handler (macOS only).
pub struct AxSetValue;

#[async_trait::async_trait]
impl ToolHandler for AxSetValue {
    fn name(&self) -> &'static str {
        "ax_set_value"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "ax_set_value",
            "macOS only. Write to an element's kAXValueAttribute — value assignment, \
             not key-event typing. Use for AXTextField / AXTextArea / AXSearchField and \
             similar text widgets. Does NOT fire keydown/keyup, does NOT participate in \
             IME/composition, does NOT populate the app's undo stack, and will not work \
             on rich editors that refuse AXValue writes. On not_dispatchable, the caller \
             should fall back to a two-step sequence: click(fallback.x, fallback.y) to \
             focus, then type_text(text) for key-event input.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["uid", "text"],
                "properties": {
                    "uid": {
                        "type": "string",
                        "description": "Element uid from the most recent take_ax_snapshot, e.g. \"a42g3\"."
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to assign via kAXValueAttribute."
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
        let params: AxSetValueParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(ax_set_value(params, ctx.ax_session.clone()).await)
    }
}

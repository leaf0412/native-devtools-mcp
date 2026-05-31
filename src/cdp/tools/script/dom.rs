//! DOM walker plumbing and per-node rendering helpers shared by `summary.rs`.

use crate::cdp::cdp_error;
use chromiumoxide::cdp::browser_protocol::dom::{
    BackendNodeId, DescribeNodeParams, ResolveNodeParams,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    CallArgument, CallFunctionOnParams, EvaluateParams, ReleaseObjectParams,
};
use chromiumoxide::page::Page;
use futures_util::stream::StreamExt;
use rmcp::model::CallToolResult;

/// Max number of candidate-resolution chains (3 RPCs each) kept in flight at
/// once over the single CDP WebSocket. Bounds the burst — at `max_nodes=500`
/// an unbounded fan-out would queue ~1500 messages simultaneously.
const DOM_RESOLVE_CONCURRENCY: usize = 16;

/// Shared DOM walker + single-pass resolution logic used by both
/// `cdp_find_elements` and `cdp_take_dom_snapshot`.
///
/// Runs the JS walker with `return_by_value=false` to get element references,
/// then iterates to extract metadata and resolve `backendNodeId` atomically
/// via `DOM.describeNode`. Drops candidates where resolution fails or returns
/// `backendNodeId=0`.
pub(super) async fn resolve_dom_candidates(
    page: &Page,
    walker_js: &str,
) -> Result<
    (
        Vec<crate::cdp::dom_discovery::DomCandidate>,
        serde_json::Value,
    ),
    CallToolResult,
> {
    // Step 1: Evaluate walker with return_by_value=false to get element references
    let mut eval_params = EvaluateParams::new(walker_js);
    eval_params.return_by_value = Some(false);

    let walker_result = match page.execute(eval_params).await {
        Ok(resp) => resp,
        Err(e) => return Err(cdp_error(format!("DOM walker failed: {}", e))),
    };

    let result_object_id = match walker_result.result.result.object_id {
        Some(id) => id,
        None => return Err(cdp_error("DOM walker returned no object reference")),
    };

    // Step 2: Extract inventory (by-value) from the result
    let inventory_js = "function() { return JSON.stringify(this.inventory); }";
    let inv_params = CallFunctionOnParams::builder()
        .function_declaration(inventory_js)
        .object_id(result_object_id.clone())
        .return_by_value(true)
        .build();
    let inventory: serde_json::Value = match page.execute(inv_params.unwrap()).await {
        Ok(resp) => resp
            .result
            .result
            .value
            .and_then(|v| v.as_str().and_then(|s| serde_json::from_str(s).ok()))
            .unwrap_or(serde_json::json!([])),
        Err(_) => serde_json::json!([]),
    };

    // Step 3: Extract all metadata in one bulk call (avoids per-element round-trips)
    let meta_js = "function() { return JSON.stringify(this.metadata); }";
    let meta_params = CallFunctionOnParams::builder()
        .function_declaration(meta_js)
        .object_id(result_object_id.clone())
        .return_by_value(true)
        .build();
    let all_metadata: Vec<crate::cdp::dom_discovery::DomCandidate> =
        match page.execute(meta_params.unwrap()).await {
            Ok(resp) => resp
                .result
                .result
                .value
                .and_then(|v| v.as_str().and_then(|s| serde_json::from_str(s).ok()))
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };

    // Step 4: Resolve backendNodeIds with bounded parallelism. Each element
    // requires three RPCs (get ref, DOM.describeNode, releaseObject); pipelining
    // them over the single CDP WebSocket hides round-trip latency, but the
    // fan-out is capped at DOM_RESOLVE_CONCURRENCY so a large page doesn't queue
    // ~1500 messages at once. `buffered` (not `buffer_unordered`) preserves
    // candidate order, which downstream UID assignment (d1, d2, …) depends on.
    let describe_futures = all_metadata.into_iter().enumerate().map(|(i, candidate)| {
        let result_object_id = result_object_id.clone();
        async move { resolve_candidate(page, &result_object_id, i, candidate).await }
    });
    let candidates: Vec<crate::cdp::dom_discovery::DomCandidate> =
        futures_util::stream::iter(describe_futures)
            .buffered(DOM_RESOLVE_CONCURRENCY)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .flatten()
            .collect();

    // Release the wrapper remote object to avoid memory leaks
    let _ = page
        .execute(ReleaseObjectParams::new(result_object_id))
        .await;

    Ok((candidates, inventory))
}

/// Resolve one DOM candidate's backendNodeId via `DOM.describeNode`.
///
/// Returns `None` when any of the three round trips fails or the
/// describe returns `backendNodeId=0` (no real DOM backing).
async fn resolve_candidate(
    page: &Page,
    result_object_id: &chromiumoxide::cdp::js_protocol::runtime::RemoteObjectId,
    index: usize,
    mut candidate: crate::cdp::dom_discovery::DomCandidate,
) -> Option<crate::cdp::dom_discovery::DomCandidate> {
    let get_el_js = format!("function() {{ return this.elements[{}]; }}", index);
    let el_params = CallFunctionOnParams::builder()
        .function_declaration(&get_el_js)
        .object_id(result_object_id.clone())
        .return_by_value(false)
        .build()
        .ok()?;
    let el_object_id = page
        .execute(el_params)
        .await
        .ok()?
        .result
        .result
        .object_id?;

    let el_oid_for_release = el_object_id.clone();
    let describe = DescribeNodeParams::builder()
        .object_id(el_object_id)
        .build();
    let describe_result = page.execute(describe).await;
    let _ = page
        .execute(ReleaseObjectParams::new(el_oid_for_release))
        .await;

    let id = *describe_result.ok()?.result.node.backend_node_id.inner();
    if id == 0 {
        return None;
    }
    candidate.backend_node_id = id;
    Some(candidate)
}

pub(super) fn dom_candidate_json(
    uid: &str,
    n: &crate::cdp::dom_discovery::DomCandidate,
) -> serde_json::Value {
    let viewport_rect = n.viewport_rect.as_ref().map(|r| {
        serde_json::json!({
            "x": r.x,
            "y": r.y,
            "width": r.width,
            "height": r.height,
        })
    });

    serde_json::json!({
        "uid": uid,
        "role": n.role,
        "label": n.label,
        "tag": n.tag,
        "disabled": n.disabled,
        "parent_role": n.parent_role,
        "parent_name": n.parent_name,
        "accessible_name": n.accessible_name,
        "visible_text": n.visible_text,
        "value": n.value,
        "placeholder": n.placeholder,
        "title": n.title,
        "alt_text": n.alt_text,
        "test_id": n.test_id,
        "matched_on": n.matched_on,
        "warnings": n.warnings,
        "viewport_rect": viewport_rect,
        "in_viewport": n.in_viewport,
    })
}

pub(super) fn snapshot_node_json(uid: &str, node: &crate::cdp::SnapshotNode) -> serde_json::Value {
    serde_json::json!({
        "uid": uid,
        "role": node.role,
        "label": node.name,
    })
}

pub(super) fn nearby_snapshot_candidates(
    snapshot: &crate::cdp::SnapshotMap,
    uid: &str,
    radius: usize,
) -> Vec<serde_json::Value> {
    if radius == 0 {
        return Vec::new();
    }
    let Some(index) = snapshot
        .ordered_uids
        .iter()
        .position(|candidate_uid| candidate_uid == uid)
    else {
        return Vec::new();
    };

    let start = index.saturating_sub(radius);
    let end = (index + radius + 1).min(snapshot.ordered_uids.len());
    snapshot.ordered_uids[start..end]
        .iter()
        .filter(|candidate_uid| candidate_uid.as_str() != uid)
        .filter_map(|candidate_uid| {
            snapshot
                .uid_to_candidate
                .get(candidate_uid)
                .map(|candidate| dom_candidate_json(candidate_uid, candidate))
                .or_else(|| {
                    snapshot
                        .uid_to_node
                        .get(candidate_uid)
                        .map(|node| snapshot_node_json(candidate_uid, node))
                })
        })
        .collect()
}

pub(super) async fn page_title(page: &Page) -> String {
    let mut eval_params = EvaluateParams::new("document.title || \"\"");
    eval_params.return_by_value = Some(true);
    page.execute(eval_params)
        .await
        .ok()
        .and_then(|resp| resp.result.result.value)
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_default()
}

pub(super) async fn live_element_context(
    page: &Page,
    uid: &str,
    backend_node_id: i64,
    ancestor_depth: u32,
    sibling_limit: u32,
    child_limit: u32,
    max_chars: u32,
) -> Result<serde_json::Value, CallToolResult> {
    let resolve_params = ResolveNodeParams::builder()
        .backend_node_id(BackendNodeId::new(backend_node_id))
        .build();
    let remote_object = page.execute(resolve_params).await.map_err(|e| {
        cdp_error(format!(
            "Element uid={} could not be resolved to a DOM node: {}",
            uid, e
        ))
    })?;
    let object_id = remote_object.result.object.object_id.ok_or_else(|| {
        cdp_error(format!(
            "Element uid={} could not be resolved to a DOM node.",
            uid
        ))
    })?;

    let context_fn = r#"function(ancestorDepth, siblingLimit, childLimit, maxChars) {
        const normalize = value => (value || "").replace(/\s+/g, " ").trim();
        const truncate = value => {
            const text = normalize(value);
            return text.length > maxChars ? text.substring(0, maxChars) : text;
        };
        const rectFor = el => {
            const rect = el.getBoundingClientRect();
            return {
                x: Math.round(rect.x * 10) / 10,
                y: Math.round(rect.y * 10) / 10,
                width: Math.round(rect.width * 10) / 10,
                height: Math.round(rect.height * 10) / 10,
            };
        };
        const roleFor = el => {
            const aria = el.getAttribute("role");
            if (aria) return aria;
            const tag = el.tagName;
            if (tag === "BUTTON" || (tag === "INPUT" && ["submit", "button", "reset"].includes(el.type))) return "button";
            if (tag === "A" && el.hasAttribute("href")) return "link";
            if (tag === "INPUT") {
                const type = el.type || "text";
                if (type === "checkbox") return "checkbox";
                if (type === "radio") return "radio";
                if (type === "search") return "searchbox";
                return "textbox";
            }
            if (tag === "TEXTAREA") return "textbox";
            if (tag === "SELECT") return "combobox";
            if (el.isContentEditable) return "textbox";
            return tag.toLowerCase();
        };
        const summarize = el => ({
            tag: el.tagName.toLowerCase(),
            role: roleFor(el),
            text: truncate(el.innerText || el.textContent || ""),
            aria_label: truncate(el.getAttribute("aria-label") || ""),
            title: truncate(el.getAttribute("title") || ""),
            placeholder: truncate(el.getAttribute("placeholder") || el.getAttribute("data-placeholder") || ""),
            value: truncate((el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT") ? el.value : ""),
            test_id: truncate(el.getAttribute("data-testid") || el.getAttribute("data-test") || el.getAttribute("data-cy") || ""),
            disabled: el.disabled === true || el.getAttribute("aria-disabled") === "true",
            rect: rectFor(el),
        });

        const ancestors = [];
        let parent = this.parentElement;
        while (parent && ancestors.length < ancestorDepth) {
            ancestors.push(summarize(parent));
            parent = parent.parentElement;
        }

        const siblings = [];
        if (this.parentElement) {
            const children = Array.from(this.parentElement.children);
            const index = children.indexOf(this);
            const start = Math.max(0, index - siblingLimit);
            const end = Math.min(children.length, index + siblingLimit + 1);
            for (let i = start; i < end; i++) {
                if (children[i] !== this) siblings.push(summarize(children[i]));
            }
        }

        const children = Array.from(this.children).slice(0, childLimit).map(summarize);
        return {
            element: summarize(this),
            ancestors,
            siblings,
            children,
        };
    }"#;

    let call_params = CallFunctionOnParams::builder()
        .function_declaration(context_fn)
        .object_id(object_id.clone())
        .arguments(vec![
            CallArgument::builder()
                .value(serde_json::Value::from(ancestor_depth))
                .build(),
            CallArgument::builder()
                .value(serde_json::Value::from(sibling_limit))
                .build(),
            CallArgument::builder()
                .value(serde_json::Value::from(child_limit))
                .build(),
            CallArgument::builder()
                .value(serde_json::Value::from(max_chars))
                .build(),
        ])
        .return_by_value(true)
        .await_promise(true)
        .build()
        .map_err(|e| cdp_error(format!("Failed to build context call params: {}", e)))?;

    let call_result = page.execute(call_params).await;
    let _ = page.execute(ReleaseObjectParams::new(object_id)).await;

    match call_result {
        Ok(resp) => {
            if let Some(exc) = &resp.result.exception_details {
                return Err(cdp_error(format!("JavaScript exception: {}", exc.text)));
            }
            Ok(resp
                .result
                .result
                .value
                .as_ref()
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        }
        Err(e) => Err(cdp_error(format!(
            "Failed to expand element context for uid={}: {}",
            uid, e
        ))),
    }
}

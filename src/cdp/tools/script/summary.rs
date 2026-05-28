//! CDP discovery / context / snapshot tools layered on the `dom` helpers.

use super::dom::{
    dom_candidate_json, live_element_context, nearby_snapshot_candidates, page_title,
    resolve_dom_candidates, snapshot_node_json,
};
use crate::cdp::{cdp_error, page_url, CdpClient};
use rmcp::model::{CallToolResult, Content};
use std::sync::Arc;
use tokio::sync::RwLock;

pub async fn cdp_summarize_page(cdp_client: Arc<RwLock<Option<CdpClient>>>) -> CallToolResult {
    let guard = cdp_client.read().await;
    let client = match guard.as_ref() {
        Some(c) => c,
        None => return cdp_error("No CDP connection. Use cdp_connect first."),
    };

    let page = match client.require_page() {
        Ok(p) => p,
        Err(e) => return e,
    };

    let page_url = page_url(&page).await;
    let title = page_title(&page).await;
    let generation = client.generation;
    let walker_js = crate::cdp::dom_discovery::dom_walker_js("", None, 0);
    let (_candidates, inventory) = match resolve_dom_candidates(&page, &walker_js).await {
        Ok(result) => result,
        Err(e) => return e,
    };

    let result = serde_json::json!({
        "page_url": page_url,
        "title": title,
        "source": "dom_summary",
        "snapshot_generation": generation,
        "inventory": inventory,
    });

    CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )])
}

pub async fn cdp_get_element_context(
    uid: String,
    ancestor_depth: Option<u32>,
    sibling_limit: Option<u32>,
    child_limit: Option<u32>,
    max_chars: Option<u32>,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    let guard = cdp_client.read().await;
    let client = match guard.as_ref() {
        Some(c) => c,
        None => return cdp_error("No CDP connection. Use cdp_connect first."),
    };

    let page = match client.require_page() {
        Ok(p) => p,
        Err(e) => return e,
    };

    let current_url = page_url(&page).await;
    let snapshot =
        match client.last_dom_snapshot.as_ref() {
            Some(snapshot) => snapshot,
            None => return cdp_error(
                "No DOM snapshot available. Call cdp_find_elements or cdp_take_dom_snapshot before cdp_get_element_context.",
            ),
        };
    let node = match crate::cdp::resolve_uid_from_maps(
        &uid,
        Some(snapshot),
        client.generation,
        &current_url,
    ) {
        Ok(node) => node,
        Err(msg) => return cdp_error(msg),
    };

    let generation = snapshot.generation;
    let stored_element = snapshot
        .uid_to_candidate
        .get(&uid)
        .map(|candidate| dom_candidate_json(&uid, candidate))
        .unwrap_or_else(|| snapshot_node_json(&uid, node));
    let nearby =
        nearby_snapshot_candidates(snapshot, &uid, sibling_limit.unwrap_or(2).min(10) as usize);
    let backend_node_id = node.backend_node_id;

    let live_context = match live_element_context(
        &page,
        &uid,
        backend_node_id,
        ancestor_depth.unwrap_or(3).min(8),
        sibling_limit.unwrap_or(2).min(10),
        child_limit.unwrap_or(8).min(50),
        max_chars.unwrap_or(240).clamp(40, 1000),
    )
    .await
    {
        Ok(context) => context,
        Err(e) => return e,
    };

    let result = serde_json::json!({
        "page_url": current_url,
        "source": "dom_context",
        "uid": uid,
        "snapshot_generation": generation,
        "element": stored_element,
        "nearby_snapshot_matches": nearby,
        "live_context": live_context,
    });

    CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )])
}

pub async fn cdp_find_elements(
    query: String,
    role: Option<String>,
    max_results: Option<u32>,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    let mut guard = cdp_client.write().await;
    let client = match guard.as_mut() {
        Some(c) => c,
        None => return cdp_error("No CDP connection. Use cdp_connect first."),
    };

    let page = match client.require_page() {
        Ok(p) => p,
        Err(e) => return e,
    };

    let max = max_results.unwrap_or(10);
    let page_url = page_url(&page).await;
    let generation = client.generation;

    let walker_js = crate::cdp::dom_discovery::dom_walker_js(&query, role.as_deref(), max);

    let (candidates, inventory) = match resolve_dom_candidates(&page, &walker_js).await {
        Ok(result) => result,
        Err(e) => return e,
    };

    // Build snapshot map and format response
    let snapshot_map =
        crate::cdp::dom_discovery::build_dom_snapshot(&candidates, page_url.clone(), generation);

    let matches_json: Vec<serde_json::Value> = candidates
        .iter()
        .enumerate()
        .map(|(i, n)| dom_candidate_json(&format!("d{}", i + 1), n))
        .collect();

    client.last_dom_snapshot = Some(snapshot_map);

    let result = serde_json::json!({
        "page_url": page_url,
        "source": "dom",
        "matches": matches_json,
        "inventory": inventory,
    });

    CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )])
}

pub async fn cdp_take_dom_snapshot(
    max_nodes: Option<u32>,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    let mut guard = cdp_client.write().await;
    let client = match guard.as_mut() {
        Some(c) => c,
        None => return cdp_error("No CDP connection. Use cdp_connect first."),
    };

    let page = match client.require_page() {
        Ok(p) => p,
        Err(e) => return e,
    };

    let max = max_nodes.unwrap_or(500);
    let page_url = page_url(&page).await;
    let generation = client.generation;

    // Use empty query to match all interactive elements
    let walker_js = crate::cdp::dom_discovery::dom_walker_js("", None, max);

    let (candidates, _inventory) = match resolve_dom_candidates(&page, &walker_js).await {
        Ok(result) => result,
        Err(e) => return e,
    };

    let snapshot_map =
        crate::cdp::dom_discovery::build_dom_snapshot(&candidates, page_url, generation);

    let output = crate::cdp::dom_discovery::format_dom_snapshot(&candidates);
    client.last_dom_snapshot = Some(snapshot_map);

    CallToolResult::success(vec![Content::text(output)])
}

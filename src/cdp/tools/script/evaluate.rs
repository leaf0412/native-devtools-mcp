//! `cdp_evaluate_script` and its JS-result formatter.

use crate::cdp::{cdp_error, page_url, CdpClient};
use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, ResolveNodeParams};
use chromiumoxide::cdp::js_protocol::runtime::{
    CallArgument, CallFunctionOnParams, EvaluateParams,
};
use rmcp::model::{CallToolResult, Content};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Format a JS execution response into a tool result, handling exceptions.
fn format_js_result(
    result: &chromiumoxide::cdp::js_protocol::runtime::EvaluateReturns,
) -> CallToolResult {
    if let Some(exc) = &result.exception_details {
        return cdp_error(format!("JavaScript exception: {}", exc.text));
    }
    let value = result
        .result
        .value
        .as_ref()
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "null".to_string()),
    )])
}

pub async fn cdp_evaluate_script(
    function: String,
    args: Option<Vec<serde_json::Value>>,
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

    // Determine if we need to resolve element args.
    let has_uid_args = args
        .as_ref()
        .is_some_and(|a| a.iter().any(|v| v.get("uid").is_some()));

    if !has_uid_args {
        // Simple case: evaluate the expression directly.
        // If it looks like a function declaration or arrow function, wrap as an IIFE
        // so `() => document.title` returns the title, not the function object.
        // Use `=>` presence to detect arrow functions — avoids false positives on
        // parenthesized expressions like `(1 + 2)` or `({ title: document.title })`.
        let trimmed = function.trim_start();
        let is_function = trimmed.starts_with("function")
            || trimmed.starts_with("async function")
            || function.contains("=>");
        let expression = if is_function {
            format!("({})()", function)
        } else {
            function
        };

        let mut eval_params = EvaluateParams::new(expression);
        eval_params.return_by_value = Some(true);
        eval_params.await_promise = Some(true);

        return match page.execute(eval_params).await {
            Ok(resp) => format_js_result(&resp.result),
            Err(e) => cdp_error(format!("Failed to evaluate script: {}", e)),
        };
    }

    // Args case: resolve UIDs to remote objects and call function with them.
    let arg_list = match args.as_ref() {
        Some(a) => a,
        None => return cdp_error("args required when passing element references"),
    };
    let mut call_arguments: Vec<CallArgument> = Vec::with_capacity(arg_list.len());

    // Collect (uid, backend_node_id) pairs first to avoid borrow issues.
    let mut uid_backend_pairs: Vec<(String, i64)> = Vec::with_capacity(arg_list.len());
    let current_url = page_url(&page).await;
    for arg in arg_list {
        if let Some(uid) = arg.get("uid").and_then(|v| v.as_str()) {
            let node = match crate::cdp::resolve_uid_from_maps(
                uid,
                client.last_dom_snapshot.as_ref(),
                client.generation,
                &current_url,
            ) {
                Ok(n) => n,
                Err(msg) => return cdp_error(msg),
            };
            uid_backend_pairs.push((uid.to_string(), node.backend_node_id));
        }
    }

    // Resolve each backend node ID to a remote object ID.
    // Track the first element's objectId to use as the execution context for callFunctionOn.
    let mut first_object_id = None;
    for (uid, backend_node_id) in &uid_backend_pairs {
        let resolve_params = ResolveNodeParams::builder()
            .backend_node_id(BackendNodeId::new(*backend_node_id))
            .build();

        let remote_object = match page.execute(resolve_params).await {
            Ok(resp) => resp.result.object,
            Err(_) => {
                return cdp_error(format!(
                    "Element uid={} could not be resolved to a DOM node.",
                    uid
                ));
            }
        };

        let object_id = match remote_object.object_id {
            Some(id) => id,
            None => {
                return cdp_error(format!(
                    "Element uid={} could not be resolved to a DOM node.",
                    uid
                ));
            }
        };

        if first_object_id.is_none() {
            first_object_id = Some(object_id.clone());
        }
        call_arguments.push(CallArgument::builder().object_id(object_id).build());
    }

    // CDP callFunctionOn requires objectId or executionContextId.
    // Use the first element's objectId so the function executes in that element's context.
    let target_object_id = match first_object_id {
        Some(id) => id,
        None => return cdp_error("No element arguments could be resolved."),
    };

    // Call the function with the resolved element arguments.
    let call_params = match CallFunctionOnParams::builder()
        .function_declaration(function)
        .object_id(target_object_id)
        .arguments(call_arguments)
        .return_by_value(true)
        .await_promise(true)
        .build()
    {
        Ok(p) => p,
        Err(e) => return cdp_error(format!("Failed to build call params: {}", e)),
    };

    match page.execute(call_params).await {
        Ok(resp) => {
            // CallFunctionOn returns CallFunctionOnReturns, not EvaluateReturns.
            if let Some(exc) = &resp.result.exception_details {
                return cdp_error(format!("JavaScript exception: {}", exc.text));
            }
            let value = resp
                .result
                .result
                .value
                .as_ref()
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| "null".to_string()),
            )])
        }
        Err(e) => cdp_error(format!("Failed to call function: {}", e)),
    }
}

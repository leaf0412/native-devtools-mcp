//! `ToolHandler` implementations for the CDP script/discovery tools.
//!
//! Each handler wraps one CDP tool with its name, schema, and call body, moved
//! verbatim from `get_cdp_tools()` in `server.rs` and the `call_tool` match
//! arms. `self.cdp_client` becomes `ctx.cdp_client`. The `Self::UID_DESC`
//! constant referenced by the original schemas is unreachable from this module,
//! so its literal value is inlined byte-identical. CDP tools are always listed
//! (the trait default `Availability::Always`). The whole module is gated
//! `#[cfg(feature = "cdp")]` at its `mod` declaration, so no per-item `cfg` is
//! needed here.

use crate::tools::registry::{json_to_object, parse_string_field, ToolContext, ToolHandler};
use rmcp::model::{CallToolResult, Tool};
use rmcp::Error as McpError;
use serde_json::Value;
use std::sync::Arc;

/// `cdp_take_dom_snapshot` — take a full DOM snapshot of the selected page.
pub struct CdpTakeDomSnapshot;

#[async_trait::async_trait]
impl ToolHandler for CdpTakeDomSnapshot {
    fn name(&self) -> &'static str {
        "cdp_take_dom_snapshot"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_take_dom_snapshot",
            "Take a full DOM snapshot of the selected browser page. Returns all interactive elements with UIDs prefixed 'd' (e.g., d1, d2). Use when you need the complete page structure — captures contenteditable editors, placeholder inputs, and custom widgets. For targeted lookups, prefer cdp_find_elements instead. UIDs are valid for cdp_click, cdp_fill, and other action tools.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "max_nodes": {
                        "type": "integer",
                        "description": "Maximum number of nodes to return (default: 500)"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let max_nodes = args
            .get("max_nodes")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        Ok(crate::cdp::tools::cdp_take_dom_snapshot(max_nodes, ctx.cdp_client.clone()).await)
    }
}

/// `cdp_summarize_page` — return a compact page summary without UIDs.
pub struct CdpSummarizePage;

#[async_trait::async_trait]
impl ToolHandler for CdpSummarizePage {
    fn name(&self) -> &'static str {
        "cdp_summarize_page"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_summarize_page",
            "Return a compact page summary: URL, title, current page generation, and an inventory of interactive elements grouped by role with a few sample labels. Does not return element UIDs and does not overwrite the current DOM UID snapshot. Use this for orientation before issuing targeted cdp_find_elements queries.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        Ok(crate::cdp::tools::cdp_summarize_page(ctx.cdp_client.clone()).await)
    }
}

/// `cdp_find_elements` — preferred discovery tool over the live DOM.
pub struct CdpFindElements;

#[async_trait::async_trait]
impl ToolHandler for CdpFindElements {
    fn name(&self) -> &'static str {
        "cdp_find_elements"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_find_elements",
            "PREFERRED discovery tool. Search the live DOM for interactive elements matching a text query across labels, visible text, values, placeholders, titles, alt text, and test ids. Returns UIDs prefixed 'd' (e.g., d1, d2), match provenance, visible/accessibility text evidence, parent context for disambiguation, viewport geometry, warnings, and a page-level inventory grouped by role. Always try this first — it gives focused results without flooding context. Use cdp_take_dom_snapshot only if you need the full page structure. UIDs are valid for cdp_click, cdp_fill, and other action tools.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Text to search for across element labels, visible text, values, placeholders, titles, alt text, and test ids"
                    },
                    "role": {
                        "type": "string",
                        "description": "Optional role filter (e.g., 'textbox', 'button')"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum matches to return (default: 10)"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let query = parse_string_field(&args, "query")?;
        let role = args.get("role").and_then(|v| v.as_str()).map(String::from);
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        Ok(
            crate::cdp::tools::cdp_find_elements(query, role, max_results, ctx.cdp_client.clone())
                .await,
        )
    }
}

/// `cdp_get_element_context` — expand a UID with bounded live DOM context.
pub struct CdpGetElementContext;

#[async_trait::async_trait]
impl ToolHandler for CdpGetElementContext {
    fn name(&self) -> &'static str {
        "cdp_get_element_context"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_get_element_context",
            "Expand a UID returned by the most recent cdp_find_elements or cdp_take_dom_snapshot call. Returns the stored match evidence, nearby snapshot matches, and bounded live DOM context around the element (ancestors, siblings, and children). Use this when targeted search returns multiple plausible matches or the match needs local context before acting.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["uid"],
                "properties": {
                    "uid": {
                        "type": "string",
                        "description": "Element UID from cdp_take_dom_snapshot or cdp_find_elements (d-prefixed)"
                    },
                    "ancestor_depth": {
                        "type": "integer",
                        "description": "Maximum ancestor levels to include (default: 3, max: 8)"
                    },
                    "sibling_limit": {
                        "type": "integer",
                        "description": "Number of preceding/following siblings to include around the element (default: 2, max: 10)"
                    },
                    "child_limit": {
                        "type": "integer",
                        "description": "Maximum direct children to summarize (default: 8, max: 50)"
                    },
                    "max_chars": {
                        "type": "integer",
                        "description": "Maximum text characters per summarized node (default: 240, range: 40-1000)"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let uid = parse_string_field(&args, "uid")?;
        let ancestor_depth = args
            .get("ancestor_depth")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let sibling_limit = args
            .get("sibling_limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let child_limit = args
            .get("child_limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let max_chars = args
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        Ok(crate::cdp::tools::cdp_get_element_context(
            uid,
            ancestor_depth,
            sibling_limit,
            child_limit,
            max_chars,
            ctx.cdp_client.clone(),
        )
        .await)
    }
}

/// `cdp_evaluate_script` — evaluate a JS function in the selected page.
pub struct CdpEvaluateScript;

#[async_trait::async_trait]
impl ToolHandler for CdpEvaluateScript {
    fn name(&self) -> &'static str {
        "cdp_evaluate_script"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_evaluate_script",
            "Evaluate a JavaScript function in the selected browser page. Arbitrary script execution may mutate page or external state and is approval-gated by clients; prefer cdp_find_elements/cdp_take_dom_snapshot for discovery and typed cdp_* action tools for UI changes. Returns the response as JSON. Example without arguments: '() => document.title'. Example with element arguments: pass UIDs from cdp_take_dom_snapshot or cdp_find_elements via args to reference DOM elements, e.g., '(el) => el.innerText' with args=[{uid: 'd5'}].",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["function"],
                "properties": {
                    "function": {
                        "type": "string",
                        "description": "JavaScript function to evaluate (e.g., '() => document.title' or '(el) => el.innerText')"
                    },
                    "args": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "uid": { "type": "string", "description": "Element UID from cdp_take_dom_snapshot or cdp_find_elements (d-prefixed)" }
                            }
                        },
                        "description": "Optional element arguments from snapshot UIDs"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let function = parse_string_field(&args, "function")?;
        let script_args = args.get("args").and_then(|v| v.as_array()).cloned();
        Ok(
            crate::cdp::tools::cdp_evaluate_script(function, script_args, ctx.cdp_client.clone())
                .await,
        )
    }
}

/// `cdp_wait_for` — wait for any of the specified texts to appear.
pub struct CdpWaitFor;

#[async_trait::async_trait]
impl ToolHandler for CdpWaitFor {
    fn name(&self) -> &'static str {
        "cdp_wait_for"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_wait_for",
            "Wait for the specified text to appear on the selected page. Resolves when any value appears.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "description": "Non-empty list of texts. Resolves when any value appears on the page."
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Maximum wait time in milliseconds (default: 10000)"
                    },
                    "include_snapshot": {
                        "type": "boolean",
                        "description": "Appends a DOM snapshot (d-prefixed UIDs) to the response after the text appears (default: false). When false, only a short 'text appeared after Xms' line is returned."
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let texts: Vec<String> = args
            .get("text")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if texts.is_empty() {
            return Err(McpError::invalid_params(
                "missing required param: text (array of strings)",
                None,
            ));
        }
        let timeout = args.get("timeout").and_then(|v| v.as_u64());
        let include_snapshot = args
            .get("include_snapshot")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(crate::cdp::tools::cdp_wait_for(
            texts,
            timeout,
            include_snapshot,
            ctx.cdp_client.clone(),
        )
        .await)
    }
}

/// `cdp_wait_for_page_change` — wait for a semantic page/element change.
pub struct CdpWaitForPageChange;

#[async_trait::async_trait]
impl ToolHandler for CdpWaitForPageChange {
    fn name(&self) -> &'static str {
        "cdp_wait_for_page_change"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "cdp_wait_for_page_change",
            "Wait until the selected page or a scoped DOM element has a semantic visible-text/editor-value change. Use this for unknown incoming messages, replies, notifications, or page updates after you have chosen the best scope_uid with cdp_find_elements/cdp_get_element_context. This blocks inside one tool call and returns compact before/after deltas.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "scope_uid": {
                        "type": "string",
                        "description": "Optional d-prefixed uid to watch. Prefer a stable container such as a message list, inbox list, thread, panel, or editor. Omit only when a page-level wait is intentional."
                    },
                    "condition": {
                        "type": "string",
                        "description": "Coarse condition label for the wait, e.g. semantic_delta, new_visible_text, item_count_changed. Currently evaluated as semantic_delta and echoed in the result."
                    },
                    "goal": {
                        "type": "string",
                        "description": "Short natural-language goal to echo in the result so the next model turn can judge relevance, e.g. new incoming message in Note to Self."
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Maximum wait time in milliseconds (default: 55000, capped at 55000 to stay within the Clickweave MCP client timeout)"
                    },
                    "poll_interval_ms": {
                        "type": "integer",
                        "description": "Backup polling interval in milliseconds for changes MutationObserver cannot see (default: 500, clamped to 100-5000)"
                    },
                    "stable_ms": {
                        "type": "integer",
                        "description": "Debounce/stability window after a wake-up before comparing semantic text (default: 500, clamped to 100-2000)"
                    },
                    "include_snapshot": {
                        "type": "boolean",
                        "description": "Appends a compact DOM snapshot after the wait returns (default: false). The normal response already includes before/after text tails and deltas."
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let scope_uid = args
            .get("scope_uid")
            .and_then(|v| v.as_str())
            .map(String::from);
        let condition = args
            .get("condition")
            .and_then(|v| v.as_str())
            .map(String::from);
        let goal = args.get("goal").and_then(|v| v.as_str()).map(String::from);
        let timeout = args.get("timeout").and_then(|v| v.as_u64());
        let poll_interval_ms = args.get("poll_interval_ms").and_then(|v| v.as_u64());
        let stable_ms = args.get("stable_ms").and_then(|v| v.as_u64());
        let include_snapshot = args
            .get("include_snapshot")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(crate::cdp::tools::cdp_wait_for_page_change(
            scope_uid,
            condition,
            goal,
            timeout,
            poll_interval_ms,
            stable_ms,
            include_snapshot,
            ctx.cdp_client.clone(),
        )
        .await)
    }
}

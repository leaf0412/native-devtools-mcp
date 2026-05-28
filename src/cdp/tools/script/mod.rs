//! CDP script and snapshot tools: evaluate_script, find_elements,
//! take_dom_snapshot, wait_for, wait_for_page_change.

mod dom;
mod evaluate;
mod scope;
mod summary;

pub use evaluate::cdp_evaluate_script;
pub use summary::{
    cdp_find_elements, cdp_get_element_context, cdp_summarize_page, cdp_take_dom_snapshot,
};

use crate::cdp::{cdp_error, CdpClient};
use chromiumoxide::cdp::js_protocol::runtime::{CallFunctionOnParams, EvaluateParams};
use chromiumoxide::page::Page;
use rmcp::model::{CallToolResult, Content};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

const MAX_WAIT_TIMEOUT_MS: u64 = 60_000;
const MAX_PAGE_CHANGE_WAIT_TIMEOUT_MS: u64 = 55_000;
const DEFAULT_PAGE_CHANGE_WAIT_TIMEOUT_MS: u64 = 55_000;
const DEFAULT_PAGE_CHANGE_POLL_MS: u64 = 500;
const MIN_PAGE_CHANGE_POLL_MS: u64 = 100;
const MAX_PAGE_CHANGE_POLL_MS: u64 = 5_000;
const DEFAULT_PAGE_CHANGE_STABLE_MS: u64 = 500;
const MIN_PAGE_CHANGE_STABLE_MS: u64 = 100;
const MAX_PAGE_CHANGE_STABLE_MS: u64 = 2_000;

pub async fn cdp_wait_for(
    texts: Vec<String>,
    timeout_ms: Option<u64>,
    include_snapshot: bool,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    let raw_timeout = timeout_ms.unwrap_or(10_000).min(MAX_WAIT_TIMEOUT_MS);
    let timeout = std::time::Duration::from_millis(raw_timeout);
    let poll_interval = std::time::Duration::from_millis(500);
    let start = std::time::Instant::now();

    // Build JS check: resolves true when any of the texts appear in the page body.
    // serde_json::to_string on Vec<String> is infallible.
    let texts_json = serde_json::to_string(&texts).unwrap();
    let check_js = format!(
        "document.body && {}.some(t => document.body.innerText.includes(t))",
        texts_json
    );

    loop {
        let found = {
            let guard = cdp_client.read().await;
            let client = match guard.as_ref() {
                Some(c) => c,
                None => return cdp_error("No CDP connection. Use cdp_connect first."),
            };
            let page = match client.require_page() {
                Ok(p) => p,
                Err(e) => return e,
            };

            let mut eval_params = EvaluateParams::new(&check_js);
            eval_params.return_by_value = Some(true);

            match page.execute(eval_params).await {
                Ok(resp) => resp
                    .result
                    .result
                    .value
                    .as_ref()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                Err(_) => false,
            }
        };

        if found {
            let elapsed_ms = start.elapsed().as_millis();
            let header = format!("Text appeared after {}ms: {}", elapsed_ms, texts_json);
            if !include_snapshot {
                return CallToolResult::success(vec![Content::text(header)]);
            }
            // Smaller cap than the user-facing default: cdp_wait_for is
            // typically followed by a targeted cdp_find_elements, so a
            // lightweight snapshot is enough to show what appeared.
            let mut result = cdp_take_dom_snapshot(Some(100), cdp_client.clone()).await;
            result.content.insert(0, Content::text(header));
            return result;
        }

        if start.elapsed() >= timeout {
            return cdp_error(format!(
                "Timed out after {}ms waiting for text: {}",
                timeout.as_millis(),
                texts_json
            ));
        }

        tokio::time::sleep(poll_interval).await;
    }
}

const PAGE_CHANGE_WAIT_JS: &str = r#"
async function(timeoutMs, stableMs, pollIntervalMs) {
  const root = (this && this.nodeType === Node.ELEMENT_NODE) ? this : document.body;
  const startedAt = Date.now();
  const safeRoot = root || document.body;

  const normalizeLines = (value) => String(value || '')
    .replace(/\u200e|\u200f|\u202a|\u202b|\u202c|\u202d|\u202e|\u2066|\u2067|\u2068|\u2069/g, '')
    .split(/\n+/)
    .map((line) => line.replace(/\s+/g, ' ').trim())
    .filter(Boolean)
    .join('\n')
    .trim();
  const stripDynamic = (value) => normalizeLines(value)
    .replace(/\b(?:now|today|yesterday)\b/gi, '<relative-time>')
    .replace(/\b\d+\s*(?:s|sec|secs|second|seconds|m|min|mins|minute|minutes|h|hr|hrs|hour|hours|d|day|days|w|week|weeks|mo|month|months|y|yr|yrs|year|years)\b/gi, '<relative-time>');
  const hash = (value) => {
    let h = 2166136261;
    for (let i = 0; i < value.length; i++) {
      h ^= value.charCodeAt(i);
      h = Math.imul(h, 16777619);
    }
    return `${(h >>> 0).toString(16).padStart(8, '0')}:${value.length}`;
  };
  const roleFor = (el) => {
    if (!el || !el.getAttribute) return '';
    const role = el.getAttribute('role');
    if (role) return role;
    const tag = (el.tagName || '').toLowerCase();
    if (tag === 'button') return 'button';
    if (tag === 'a' && el.hasAttribute('href')) return 'link';
    if (tag === 'input' || tag === 'textarea' || el.isContentEditable) return 'textbox';
    if (tag === 'select') return 'combobox';
    return tag;
  };
  const isVisible = (el) => {
    if (!(el instanceof Element)) return false;
    if (el === document.body || el === safeRoot) return true;
    const style = window.getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden') return false;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 || rect.height > 0;
  };
  const fieldSelector = 'input, textarea, select, [contenteditable="true"], [contenteditable="plaintext-only"], [role="textbox"]';
  const fieldValue = (el) => {
    if (!el) return '';
    if ('value' in el && el.value != null && String(el.value).trim()) return el.value;
    return el.innerText || el.textContent || el.getAttribute('aria-label') || el.getAttribute('placeholder') || '';
  };
  const summarizeElement = (el) => {
    if (!el) return null;
    return {
      tag: (el.tagName || '').toLowerCase(),
      role: roleFor(el),
      aria_label: normalizeLines(el.getAttribute ? el.getAttribute('aria-label') : '').slice(0, 240),
      placeholder: normalizeLines(el.getAttribute ? (el.getAttribute('placeholder') || el.getAttribute('data-placeholder')) : '').slice(0, 240),
      text: normalizeLines(('value' in el && el.value) ? el.value : (el.innerText || el.textContent || '')).slice(-500)
    };
  };
  const capture = () => {
    const textSource = safeRoot && 'innerText' in safeRoot ? safeRoot.innerText : (safeRoot ? safeRoot.textContent : '');
    const visibleText = normalizeLines(textSource);
    const fields = [];
    if (safeRoot && safeRoot.matches && safeRoot.matches(fieldSelector)) fields.push(safeRoot);
    if (safeRoot && safeRoot.querySelectorAll) fields.push(...safeRoot.querySelectorAll(fieldSelector));
    const fieldText = fields
      .filter((el, index) => fields.indexOf(el) === index)
      .filter(isVisible)
      .map(fieldValue)
      .map(normalizeLines)
      .filter(Boolean)
      .join('\n');
    const semanticText = stripDynamic([visibleText, fieldText].filter(Boolean).join('\n'));
    const semanticUnits = semanticText.split(/\n+/).map((v) => v.trim()).filter(Boolean);
    return {
      signature: hash(`${location.href}\n${document.title || ''}\n${semanticText}`),
      url: location.href,
      title: document.title || '',
      text_length: visibleText.length,
      semantic_text_length: semanticText.length,
      visible_text_tail: visibleText.slice(-2500),
      semantic_text_tail: semanticText.slice(-2500),
      semantic_units: semanticUnits.slice(-50),
      root: summarizeElement(safeRoot),
      active_element: summarizeElement(document.activeElement)
    };
  };
  const addedUnits = (beforeUnits, afterUnits) => {
    const counts = new Map();
    for (const unit of beforeUnits || []) counts.set(unit, (counts.get(unit) || 0) + 1);
    const added = [];
    for (const unit of afterUnits || []) {
      const count = counts.get(unit) || 0;
      if (count > 0) {
        counts.set(unit, count - 1);
      } else {
        added.push(unit);
      }
    }
    return added.slice(-20);
  };
  const suffixDelta = (beforeText, afterText) => {
    let i = 0;
    const limit = Math.min(beforeText.length, afterText.length);
    while (i < limit && beforeText.charCodeAt(i) === afterText.charCodeAt(i)) i++;
    return afterText.slice(i).trim().slice(-2500);
  };
  const buildResult = (changed, timedOut, before, after, trigger) => ({
    source: 'dom_semantic_wait',
    page_url: after.url,
    title: after.title,
    changed,
    timed_out: timedOut,
    elapsed_ms: Date.now() - startedAt,
    timeout_ms: timeoutMs,
    stable_ms: stableMs,
    poll_interval_ms: pollIntervalMs,
    trigger,
    before: {
      signature: before.signature,
      text_length: before.text_length,
      semantic_text_length: before.semantic_text_length,
      visible_text_tail: before.visible_text_tail,
      semantic_text_tail: before.semantic_text_tail,
      root: before.root,
      active_element: before.active_element
    },
    after: {
      signature: after.signature,
      text_length: after.text_length,
      semantic_text_length: after.semantic_text_length,
      visible_text_tail: after.visible_text_tail,
      semantic_text_tail: after.semantic_text_tail,
      root: after.root,
      active_element: after.active_element
    },
    deltas: changed ? [{
      kind: 'semantic_text_delta',
      text: suffixDelta(before.semantic_text_tail, after.semantic_text_tail),
      added_text: addedUnits(before.semantic_units, after.semantic_units)
    }] : []
  });
  const meaningfulMutation = (mutation) => {
    if (mutation.type === 'childList' || mutation.type === 'characterData') return true;
    if (mutation.type !== 'attributes') return false;
    return [
      'value',
      'aria-label',
      'aria-selected',
      'aria-checked',
      'aria-expanded',
      'aria-pressed',
      'role',
      'placeholder',
      'title',
      'disabled'
    ].includes(mutation.attributeName);
  };
  const waitForWake = (remainingMs) => new Promise((resolve) => {
    let settled = false;
    let stableTimer = null;
    let observer = null;
    const cleanup = () => {
      if (stableTimer) clearTimeout(stableTimer);
      clearTimeout(timeoutTimer);
      clearInterval(pollTimer);
      if (observer) observer.disconnect();
    };
    const finish = (reason) => {
      if (settled) return;
      settled = true;
      cleanup();
      resolve(reason);
    };
    const schedule = (reason) => {
      if (stableTimer) clearTimeout(stableTimer);
      stableTimer = setTimeout(() => finish(reason), stableMs);
    };
    try {
      observer = new MutationObserver((mutations) => {
        if (mutations.some(meaningfulMutation)) schedule('mutation');
      });
      observer.observe(safeRoot, {
        subtree: true,
        childList: true,
        characterData: true,
        attributes: true,
        attributeFilter: [
          'value',
          'aria-label',
          'aria-selected',
          'aria-checked',
          'aria-expanded',
          'aria-pressed',
          'role',
          'placeholder',
          'title',
          'disabled'
        ]
      });
    } catch (_) {
      schedule('observer_unavailable');
    }
    const pollTimer = setInterval(() => schedule('poll'), pollIntervalMs);
    const timeoutTimer = setTimeout(() => finish('timeout'), Math.max(0, remainingMs));
  });

  const before = capture();
  let latest = before;
  while (Date.now() - startedAt < timeoutMs) {
    const remainingMs = timeoutMs - (Date.now() - startedAt);
    const trigger = await waitForWake(remainingMs);
    latest = capture();
    if (latest.signature !== before.signature) {
      return buildResult(true, false, before, latest, trigger);
    }
    if (trigger === 'timeout') break;
  }
  return buildResult(false, true, before, latest, 'timeout');
}
"#;

use scope::{
    json_value_arg, release_object, resolve_scope_backend_node_id, resolve_scope_object_id,
};

fn semantic_wait_value_from_evaluate(
    result: chromiumoxide::cdp::js_protocol::runtime::EvaluateReturns,
) -> Result<Value, CallToolResult> {
    if let Some(exc) = &result.exception_details {
        return Err(cdp_error(format!(
            "JavaScript exception while waiting for page change: {}",
            exc.text
        )));
    }
    Ok(result.result.value.as_ref().cloned().unwrap_or(Value::Null))
}

fn semantic_wait_value_from_call(
    result: chromiumoxide::cdp::js_protocol::runtime::CallFunctionOnReturns,
) -> Result<Value, CallToolResult> {
    if let Some(exc) = &result.exception_details {
        return Err(cdp_error(format!(
            "JavaScript exception while waiting for page change: {}",
            exc.text
        )));
    }
    Ok(result.result.value.as_ref().cloned().unwrap_or(Value::Null))
}

async fn wait_for_page_semantic_change(
    page: &Page,
    timeout_ms: u64,
    stable_ms: u64,
    poll_interval_ms: u64,
) -> Result<Value, CallToolResult> {
    let expression = format!(
        "({}).call(document.body, {}, {}, {})",
        PAGE_CHANGE_WAIT_JS, timeout_ms, stable_ms, poll_interval_ms
    );
    let mut eval_params = EvaluateParams::new(expression);
    eval_params.return_by_value = Some(true);
    eval_params.await_promise = Some(true);
    let resp = page
        .execute(eval_params)
        .await
        .map_err(|e| cdp_error(format!("Failed to wait for page change: {}", e)))?;
    semantic_wait_value_from_evaluate(resp.result)
}

async fn wait_for_scoped_semantic_change(
    page: &Page,
    object_id: chromiumoxide::cdp::js_protocol::runtime::RemoteObjectId,
    timeout_ms: u64,
    stable_ms: u64,
    poll_interval_ms: u64,
) -> Result<Value, CallToolResult> {
    let call_params = CallFunctionOnParams::builder()
        .function_declaration(PAGE_CHANGE_WAIT_JS)
        .object_id(object_id.clone())
        .arguments(vec![
            json_value_arg(timeout_ms),
            json_value_arg(stable_ms),
            json_value_arg(poll_interval_ms),
        ])
        .return_by_value(true)
        .await_promise(true)
        .build()
        .map_err(|e| cdp_error(format!("Failed to build wait call params: {}", e)))?;
    let call_result = page.execute(call_params).await;
    release_object(page, object_id).await;
    let resp =
        call_result.map_err(|e| cdp_error(format!("Failed to wait for page change: {}", e)))?;
    semantic_wait_value_from_call(resp.result)
}

fn decorate_semantic_wait_result(
    mut value: Value,
    scope_uid: Option<&str>,
    condition: &str,
    goal: Option<&str>,
) -> Value {
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "scope".to_string(),
            serde_json::json!({
                "kind": if scope_uid.is_some() { "element" } else { "page" },
                "uid": scope_uid,
            }),
        );
        obj.insert(
            "condition".to_string(),
            Value::String(condition.to_string()),
        );
        if let Some(goal) = goal {
            obj.insert("goal".to_string(), Value::String(goal.to_string()));
        }
        obj.insert(
            "hint".to_string(),
            Value::String(
                "The wait tool consumed one agent step. Judge whether `deltas` satisfies the goal; if it does, act on it, otherwise call this wait tool again with the same scope rather than polling."
                    .to_string(),
            ),
        );
    }
    value
}

pub async fn cdp_wait_for_page_change(
    scope_uid: Option<String>,
    condition: Option<String>,
    goal: Option<String>,
    timeout_ms: Option<u64>,
    poll_interval_ms: Option<u64>,
    stable_ms: Option<u64>,
    include_snapshot: bool,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    let raw_timeout = timeout_ms
        .unwrap_or(DEFAULT_PAGE_CHANGE_WAIT_TIMEOUT_MS)
        .min(MAX_PAGE_CHANGE_WAIT_TIMEOUT_MS);
    let poll_interval = poll_interval_ms
        .unwrap_or(DEFAULT_PAGE_CHANGE_POLL_MS)
        .clamp(MIN_PAGE_CHANGE_POLL_MS, MAX_PAGE_CHANGE_POLL_MS);
    let stable = stable_ms
        .unwrap_or(DEFAULT_PAGE_CHANGE_STABLE_MS)
        .clamp(MIN_PAGE_CHANGE_STABLE_MS, MAX_PAGE_CHANGE_STABLE_MS);
    let condition = condition.unwrap_or_else(|| "semantic_delta".to_string());
    let scope_uid = scope_uid.filter(|uid| !uid.trim().is_empty());

    let page = {
        let guard = cdp_client.read().await;
        let client = match guard.as_ref() {
            Some(c) => c,
            None => return cdp_error("No CDP connection. Use cdp_connect first."),
        };
        match client.require_page() {
            Ok(page) => page,
            Err(e) => return e,
        }
    };

    let value = match scope_uid.as_deref() {
        Some(uid) => {
            let backend_node_id =
                match resolve_scope_backend_node_id(uid, &page, cdp_client.clone()).await {
                    Ok(backend_node_id) => backend_node_id,
                    Err(e) => return e,
                };
            let object_id = match resolve_scope_object_id(uid, backend_node_id, &page).await {
                Ok(object_id) => object_id,
                Err(e) => return e,
            };
            match wait_for_scoped_semantic_change(
                &page,
                object_id,
                raw_timeout,
                stable,
                poll_interval,
            )
            .await
            {
                Ok(value) => value,
                Err(e) => return e,
            }
        }
        None => {
            match wait_for_page_semantic_change(&page, raw_timeout, stable, poll_interval).await {
                Ok(value) => value,
                Err(e) => return e,
            }
        }
    };

    let result =
        decorate_semantic_wait_result(value, scope_uid.as_deref(), &condition, goal.as_deref());
    let result_text = serde_json::to_string_pretty(&result).unwrap_or_default();
    if !include_snapshot {
        return CallToolResult::success(vec![Content::text(result_text)]);
    }

    let mut snapshot = cdp_take_dom_snapshot(Some(100), cdp_client.clone()).await;
    snapshot.content.insert(0, Content::text(result_text));
    snapshot
}


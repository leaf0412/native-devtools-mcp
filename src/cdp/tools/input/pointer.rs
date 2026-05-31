//! CDP pointer tools: click, hover, fill.

use super::super::{resolve_element_center, resolve_node_checkout, resolve_to_object_id};
use crate::cdp::{cdp_error, CdpClient};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchMouseEventParams, DispatchMouseEventType, InsertTextParams, MouseButton,
};
use chromiumoxide::cdp::js_protocol::runtime::{CallArgument, CallFunctionOnParams};
use rmcp::model::{CallToolResult, Content};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

pub async fn cdp_click(
    uid: String,
    dbl_click: bool,
    include_snapshot: bool,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    let (page, _generation) = match crate::cdp::checkout_page(&cdp_client).await {
        Ok(v) => v,
        Err(e) => return e,
    };

    let (node_role, node_name, cx, cy) =
        match resolve_element_center(&uid, &cdp_client, &page).await {
            Ok(v) => v,
            Err(e) => return e,
        };

    let click_count = if dbl_click { 2_i64 } else { 1_i64 };

    let move_event = DispatchMouseEventParams::new(DispatchMouseEventType::MouseMoved, cx, cy);

    let mut press_event =
        DispatchMouseEventParams::new(DispatchMouseEventType::MousePressed, cx, cy);
    press_event.button = Some(MouseButton::Left);
    press_event.buttons = Some(1);
    press_event.click_count = Some(click_count);

    let mut release_event =
        DispatchMouseEventParams::new(DispatchMouseEventType::MouseReleased, cx, cy);
    release_event.button = Some(MouseButton::Left);
    release_event.click_count = Some(click_count);

    for event in [move_event, press_event, release_event] {
        if let Err(e) = page.execute(event).await {
            return cdp_error(format!("Click failed on uid={}: {}", uid, e));
        }
    }

    let dbl_note = if dbl_click { " (double-click)" } else { "" };
    let result = CallToolResult::success(vec![Content::text(format!(
        "Clicked uid={} '{}' ({}) at ({:.1}, {:.1}){}",
        uid, node_name, node_role, cx, cy, dbl_note
    ))]);
    super::finish_after_action(result, include_snapshot, cdp_client).await
}

pub async fn cdp_hover(
    uid: String,
    include_snapshot: bool,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    let (page, _generation) = match crate::cdp::checkout_page(&cdp_client).await {
        Ok(v) => v,
        Err(e) => return e,
    };

    let (node_role, node_name, cx, cy) =
        match resolve_element_center(&uid, &cdp_client, &page).await {
            Ok(v) => v,
            Err(e) => return e,
        };

    let move_event = DispatchMouseEventParams::new(DispatchMouseEventType::MouseMoved, cx, cy);
    if let Err(e) = page.execute(move_event).await {
        return cdp_error(format!("Hover failed on uid={}: {}", uid, e));
    }

    let result = CallToolResult::success(vec![Content::text(format!(
        "Hovered uid={} '{}' ({}) at ({:.1}, {:.1})",
        uid, node_name, node_role, cx, cy
    ))]);
    super::finish_after_action(result, include_snapshot, cdp_client).await
}

pub async fn cdp_fill(
    uid: String,
    value: String,
    include_snapshot: bool,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    let (page, _generation) = match crate::cdp::checkout_page(&cdp_client).await {
        Ok(v) => v,
        Err(e) => return e,
    };

    let current_url = crate::cdp::page_url(&page).await;
    let (backend_node_id, node_role, node_name) =
        match resolve_node_checkout(&uid, &cdp_client, &current_url).await {
            Ok(v) => v,
            Err(e) => return e,
        };

    let object_id = match resolve_to_object_id(&uid, backend_node_id, &page).await {
        Ok(id) => id,
        Err(e) => return e,
    };

    let fill_fn = r#"function(value) {
        function textOf(el) {
            if (!el) return "";
            if (el.tagName === "SELECT") {
                const selected = el.options && el.selectedIndex >= 0 ? el.options[el.selectedIndex] : null;
                const selectedValue = selected ? selected.value : (el.value || "");
                const selectedText = selected ? (selected.textContent || "").replace(/\s+/g, " ").trim() : "";
                return [selectedValue, selectedText].filter(Boolean).join("\n");
            }
            if ("value" in el) return el.value || "";
            return (el.innerText || el.textContent || "").replace(/\s+/g, " ").trim();
        }

        function findRichEditor(el) {
            if (el && el.isContentEditable) return el;
            if (!el || !el.querySelector) return null;
            return el.querySelector([
                "[contenteditable='true']",
                "[contenteditable='plaintext-only']",
                ".ql-editor",
                ".ProseMirror",
                "[data-lexical-editor='true']",
                "[role='textbox'][contenteditable]"
            ].join(","));
        }

        function selectEditableContents(el) {
            el.focus({ preventScroll: true });
            const doc = el.ownerDocument || document;
            const selection = doc.getSelection && doc.getSelection();
            if (!selection) return;
            const range = doc.createRange();
            range.selectNodeContents(el);
            selection.removeAllRanges();
            selection.addRange(range);
        }

        function setNativeValue(el, nextValue) {
            const proto = el.tagName === "TEXTAREA"
                ? HTMLTextAreaElement.prototype
                : HTMLInputElement.prototype;
            const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
            if (setter) setter.call(el, nextValue);
            else el.value = nextValue;
            el.dispatchEvent(new InputEvent("input", {
                bubbles: true,
                composed: true,
                inputType: "insertText",
                data: nextValue
            }));
            el.dispatchEvent(new Event("change", { bubbles: true }));
        }

        if (this.tagName === 'SELECT') {
            const option = Array.from(this.options).find(o => o.value === value || o.textContent.trim() === value);
            if (!option) throw new Error('Option not found: ' + value);
            this.value = option.value;
            this.dispatchEvent(new Event('input', { bubbles: true }));
            this.dispatchEvent(new Event('change', { bubbles: true }));
            return { strategy: "select_value", observedText: textOf(this) };
        }

        if (this.tagName === "INPUT" || this.tagName === "TEXTAREA") {
            this.focus({ preventScroll: true });
            if (this.select) this.select();
            setNativeValue(this, value);
            return { strategy: "native_value_setter", observedText: textOf(this) };
        }

        const richEditor = findRichEditor(this);
        if (richEditor) {
            selectEditableContents(richEditor);
            return {
                strategy: "rich_editor_keyboard",
                observedText: textOf(richEditor),
                targetTag: richEditor.tagName.toLowerCase(),
                targetClass: String(richEditor.className || "")
            };
        }

        this.focus();
        if (this.select) this.select();
        else document.execCommand('selectAll', false, null);
        document.execCommand('insertText', false, value);
        return { strategy: "exec_command", observedText: textOf(this) };
    }"#;

    let call_params = match CallFunctionOnParams::builder()
        .function_declaration(fill_fn)
        .object_id(object_id.clone())
        .arguments(vec![CallArgument::builder()
            .value(serde_json::Value::String(value.clone()))
            .build()])
        .await_promise(true)
        .return_by_value(true)
        .build()
    {
        Ok(p) => p,
        Err(e) => return cdp_error(format!("Failed to build call params: {}", e)),
    };

    let prep = match page.execute(call_params).await {
        Ok(resp) => {
            if let Some(exc) = &resp.result.exception_details {
                return cdp_error(format!("Fill failed: {}", exc.text));
            }
            resp.result.result.value.unwrap_or(Value::Null)
        }
        Err(e) => return cdp_error(format!("Fill failed on uid={}: {}", uid, e)),
    };

    let strategy = prep
        .get("strategy")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if strategy == "rich_editor_keyboard" {
        if let Err(e) = page.execute(InsertTextParams::new(value.clone())).await {
            return cdp_error(format!(
                "Fill failed on uid={} with CDP text insertion: {}",
                uid, e
            ));
        }
    }

    let verify_fn = r#"function() {
        function textOf(el) {
            if (!el) return "";
            if (el.tagName === "SELECT") {
                const selected = el.options && el.selectedIndex >= 0 ? el.options[el.selectedIndex] : null;
                const selectedValue = selected ? selected.value : (el.value || "");
                const selectedText = selected ? (selected.textContent || "").replace(/\s+/g, " ").trim() : "";
                return [selectedValue, selectedText].filter(Boolean).join("\n");
            }
            if ("value" in el) return el.value || "";
            return (el.innerText || el.textContent || "").replace(/\s+/g, " ").trim();
        }
        function findRichEditor(el) {
            if (el && el.isContentEditable) return el;
            if (!el || !el.querySelector) return null;
            return el.querySelector([
                "[contenteditable='true']",
                "[contenteditable='plaintext-only']",
                ".ql-editor",
                ".ProseMirror",
                "[data-lexical-editor='true']",
                "[role='textbox'][contenteditable]"
            ].join(","));
        }
        const target = findRichEditor(this) || this;
        return { observedText: textOf(target), active: document.activeElement === target };
    }"#;
    let verify_params = match CallFunctionOnParams::builder()
        .function_declaration(verify_fn)
        .object_id(object_id)
        .return_by_value(true)
        .await_promise(true)
        .build()
    {
        Ok(p) => p,
        Err(e) => return cdp_error(format!("Failed to build verification params: {}", e)),
    };
    let observed_text = match page.execute(verify_params).await {
        Ok(resp) => {
            if let Some(exc) = &resp.result.exception_details {
                return cdp_error(format!("Fill verification failed: {}", exc.text));
            }
            resp.result
                .result
                .value
                .and_then(|v| {
                    v.get("observedText")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default()
        }
        Err(e) => return cdp_error(format!("Fill verification failed on uid={}: {}", uid, e)),
    };

    let observed = super::observed_fill_status(strategy, &observed_text, &value);
    let rich_hint = if strategy == "rich_editor_keyboard" {
        "; rich editor used CDP keyboard insertion. If this is a chat composer and the message is ready, use cdp_press_key({\"key\":\"Enter\"}) or find/click an enabled Send control to submit."
    } else {
        ""
    };

    let result = CallToolResult::success(vec![Content::text(format!(
        "Filled uid={} '{}' ({}) with '{}' (strategy={}, {}{})",
        uid, node_name, node_role, value, strategy, observed, rich_hint
    ))]);
    super::finish_after_action(result, include_snapshot, cdp_client).await
}

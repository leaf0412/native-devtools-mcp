//! macOS AX dispatch integration tests.
//!
//! Requires live apps (Calculator, TextEdit, Terminal). All tests are
//! `#[ignore]`-gated to match the existing AX / CDP smoke pattern — run
//! locally with:
//!
//!     cargo test --test ax_dispatch_tests -- --ignored --nocapture
//!
//! These tests assert the headline invariants from the design doc:
//! - Generation tagging defeats stale-uid dispatch (Calculator).
//! - ax_set_value writes AXValue without stealing focus (TextEdit + Terminal).
//! - ax_click presses without stealing focus (Calculator + Terminal).
//! - uid_not_found is raised for syntactically valid-but-missing uids.
//! - Bbox is emitted on snapshot lines whose elements expose position+size.
//!
//! # Execution model
//!
//! The existing CDP harness at `tests/harness/mod.rs` is CDP-specific
//! (spawns headless Chrome) and does not drive `ServerHandler::call_tool`.
//! Rather than build a full server harness, these tests drive the tool
//! functions directly — each tool handler is already `pub` in
//! `native_devtools_mcp::tools::*` and takes an explicit
//! `Arc<AxSession>`, so we share one session across the calls that make up
//! a scenario. This is a deliberate tradeoff: we lose end-to-end
//! `call_tool` dispatch coverage in favor of a copy-pasteable test that
//! compiles against the current tree. End-to-end dispatch is already
//! covered by the tool-gating tests.

#![cfg(target_os = "macos")]

use native_devtools_mcp::macos::ax::collect_ax_tree_indexed;
use native_devtools_mcp::tools::ax_click::{ax_click, AxClickParams};
use native_devtools_mcp::tools::ax_select::{ax_select, AxSelectParams};
use native_devtools_mcp::tools::ax_session::AxSession;
use native_devtools_mcp::tools::ax_set_value::{ax_set_value, AxSetValueParams};
use native_devtools_mcp::tools::ax_snapshot::format_snapshot;
use native_devtools_mcp::tools::navigation::{
    focus_window, list_apps, FocusWindowParams, ListAppsParams,
};
use rmcp::model::CallToolResult;
use std::sync::Arc;

/// Build a fresh `AxSession` for one scenario — isolates tests from each
/// other (no shared generation counter, no bleed through).
fn new_session() -> Arc<AxSession> {
    Arc::new(AxSession::new())
}

/// Take a snapshot against the shared session, matching the server's
/// `call_tool` arm: walk AX tree, swap into session, format with generation.
async fn snapshot(session: &Arc<AxSession>, app_name: Option<&str>) -> String {
    let (nodes, refs) =
        collect_ax_tree_indexed(app_name).expect("collect_ax_tree_indexed should succeed");
    let generation = session.create_snapshot(refs).await;
    format_snapshot(&nodes, Some(generation))
}

/// Bring an app to front using the coord-based focus_window tool.
fn focus(app_name: &str) -> CallToolResult {
    focus_window(FocusWindowParams {
        window_id: None,
        app_name: Some(app_name.to_string()),
        pid: None,
    })
}

/// List currently running apps via the `list_apps` tool.
fn list_apps_all() -> CallToolResult {
    list_apps(ListAppsParams {
        app_name: None,
        user_apps_only: None,
    })
}

fn extract_text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("")
}

fn parse_json(s: &str) -> serde_json::Value {
    serde_json::from_str(s).expect("response body should be JSON")
}

fn extract_uid_for_named_button(snapshot: &str, button_name: &str) -> String {
    for line in snapshot.lines() {
        if line.contains(&format!("\"{}\"", button_name)) {
            if let Some(after) = line.split_whitespace().next() {
                if let Some(uid) = after.strip_prefix("uid=") {
                    return uid.to_string();
                }
            }
        }
    }
    panic!(
        "no uid for button {} in snapshot:\n{}",
        button_name, snapshot
    );
}

/// Find the line in a snapshot corresponding to the Calculator display
/// (text/staticText role) and extract its `value="..."` attribute, if any.
/// Returns None when the display is blank or absent.
///
/// Calculator's display is a text/readout element whose `AXValue` is the
/// shown number. This is an actual oracle — the "5" button label is always
/// present in the tree regardless of which button was pressed.
fn extract_calculator_display_value(snapshot: &str) -> Option<String> {
    // The display line looks something like:
    //   `uid=aNgG text "Display" value="5"`
    // or without a name:
    //   `uid=aNgG text value="5"`
    for line in snapshot.lines() {
        let depth = line.len() - line.trim_start().len();
        if depth > 6 {
            continue; // heuristic: display is near the top of the tree
        }
        let trimmed = line.trim_start();
        // Line must be an AX text node and carry a `value="..."` attribute.
        if !(trimmed.contains(" text ") || trimmed.contains(" text\t")) {
            continue;
        }
        if let Some(v_start) = trimmed.find("value=\"") {
            let rest = &trimmed[v_start + "value=\"".len()..];
            if let Some(end) = rest.find('"') {
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

// === Happy-path: Calculator + ax_click ===

#[tokio::test]
#[ignore]
async fn ax_click_presses_calculator_five_button() {
    // Requires Calculator to be running; start with a cleared display
    // (press AC / Clear before running to keep the test deterministic).
    let session = new_session();

    let snap_text = snapshot(&session, Some("Calculator")).await;
    let five_uid = extract_uid_for_named_button(&snap_text, "5");
    let before = extract_calculator_display_value(&snap_text).unwrap_or_default();

    let click = ax_click(AxClickParams { uid: five_uid, include_snapshot: false, app_name: None }, session.clone()).await;
    let body = parse_json(&extract_text(&click));
    assert_eq!(body["ok"], true);
    assert_eq!(body["dispatched_via"], "AXPress");

    // Oracle: the display value must CHANGE and end in "5" — merely
    // finding the "5" button label in the tree proves nothing because it
    // is always present regardless of dispatch.
    let snap2_text = snapshot(&session, Some("Calculator")).await;
    let after = extract_calculator_display_value(&snap2_text)
        .expect("Calculator display should expose a value after AXPress");
    assert_ne!(
        before, after,
        "display value must change after AXPress (before={before:?}, after={after:?})"
    );
    assert!(
        after.ends_with('5'),
        "display should end in '5' after AXPress on the 5 button (got {after:?})"
    );
}

// === Stale-gen replay — the D1.C1 regression test ===

#[tokio::test]
#[ignore]
async fn ax_click_stale_generation_returns_snapshot_expired() {
    let session = new_session();

    let snap1 = snapshot(&session, Some("Calculator")).await;
    let five_g1 = extract_uid_for_named_button(&snap1, "5");

    // Fresh snapshot bumps the generation. Don't use its uids.
    let _snap2 = snapshot(&session, Some("Calculator")).await;

    let click = ax_click(AxClickParams { uid: five_g1, include_snapshot: false, app_name: None }, session.clone()).await;
    assert_eq!(click.is_error, Some(true));
    let body = parse_json(&extract_text(&click));
    assert_eq!(
        body["error"]["code"], "snapshot_expired",
        "stale gen-1 uid must not resolve to gen-2 element"
    );
}

// === uid_not_found ===

#[tokio::test]
#[ignore]
async fn ax_click_unknown_uid_in_current_gen_returns_uid_not_found() {
    let session = new_session();

    let snap_text = snapshot(&session, Some("Calculator")).await;
    // Extract the current generation.
    let first_line = snap_text.lines().next().expect("non-empty snapshot");
    let uid_token = first_line
        .split_whitespace()
        .next()
        .unwrap()
        .strip_prefix("uid=")
        .unwrap();
    let (_, gen_part) = uid_token.split_once('g').unwrap();
    let gen: u64 = gen_part.parse().unwrap();

    let missing = format!("a99999g{}", gen);
    let click = ax_click(AxClickParams { uid: missing, include_snapshot: false, app_name: None }, session.clone()).await;
    assert_eq!(click.is_error, Some(true));
    let body = parse_json(&extract_text(&click));
    assert_eq!(body["error"]["code"], "uid_not_found");
}

// === Not-dispatchable ===

#[tokio::test]
#[ignore]
async fn ax_click_on_decorative_label_returns_not_dispatchable_with_fallback() {
    let session = new_session();

    let snap_text = snapshot(&session, Some("Calculator")).await;
    // Find a static-text / generic node with a bbox.
    let decorative_line = snap_text
        .lines()
        .find(|l| (l.contains(" text ") || l.contains(" generic ")) && l.contains("bbox=("))
        .expect("calculator should contain at least one non-pressable node with a bbox");
    let decorative_uid = decorative_line
        .split_whitespace()
        .next()
        .unwrap()
        .strip_prefix("uid=")
        .unwrap()
        .to_string();

    let click = ax_click(
        AxClickParams {
            uid: decorative_uid,
            include_snapshot: false,
            app_name: None,
        },
        session.clone(),
    )
    .await;
    assert_eq!(click.is_error, Some(true));
    let body = parse_json(&extract_text(&click));
    assert_eq!(body["error"]["code"], "not_dispatchable");
    assert!(
        body["error"]["fallback"].is_object(),
        "fallback should be populated when bbox is readable"
    );
    assert!(body["error"]["fallback"]["x"].as_f64().unwrap() > 0.0);
    assert!(body["error"]["fallback"]["y"].as_f64().unwrap() > 0.0);
}

// === Bbox presence on snapshot ===

#[tokio::test]
#[ignore]
async fn take_ax_snapshot_emits_bbox_on_positioned_nodes() {
    let session = new_session();
    let snap_text = snapshot(&session, Some("Calculator")).await;
    let five_line = snap_text
        .lines()
        .find(|l| l.contains("\"5\""))
        .expect("Calculator should expose a '5' button");
    assert!(
        five_line.contains("bbox=("),
        "positioned button should carry a bbox: {}",
        five_line
    );
    // Parse the bbox for format validation — four comma-separated numerics.
    let bbox_start = five_line.find("bbox=(").unwrap() + "bbox=(".len();
    let bbox_end = five_line[bbox_start..].find(')').unwrap();
    let parts: Vec<&str> = five_line[bbox_start..bbox_start + bbox_end]
        .split(',')
        .collect();
    assert_eq!(parts.len(), 4, "bbox must have four fields");
    for p in parts {
        p.parse::<i64>().expect("bbox components must be integers");
    }
}

/// Return the name of the currently-active (frontmost) app by calling the
/// `list_apps` tool and inspecting each entry's `is_active` boolean.
fn active_app_name() -> String {
    let r = list_apps_all();
    let text = extract_text(&r);
    let apps: Vec<serde_json::Value> =
        serde_json::from_str(&text).expect("list_apps should return a JSON array");
    let active = apps
        .iter()
        .find(|a| a.get("is_active").and_then(|v| v.as_bool()) == Some(true))
        .expect("at least one app should be is_active=true");
    active
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .expect("active app should have a name")
}

// === Focus preservation — ax_click (AXPress path) ===

#[tokio::test]
#[ignore]
async fn ax_click_preserves_focus_while_calculator_stays_background() {
    // Requires Calculator and Terminal both running, and Calculator's
    // display cleared (press AC) before starting so the display oracle is
    // deterministic.
    let session = new_session();

    let snap_text = snapshot(&session, Some("Calculator")).await;
    let five_uid = extract_uid_for_named_button(&snap_text, "5");
    let before = extract_calculator_display_value(&snap_text).unwrap_or_default();

    // Bring Terminal to front.
    let _focus = focus("Terminal");

    // Sanity: Terminal really is frontmost before we dispatch.
    assert_eq!(
        active_app_name(),
        "Terminal",
        "precondition: Terminal should be frontmost before ax_click"
    );

    // Click Calculator's 5 button without stealing focus.
    let click = ax_click(AxClickParams { uid: five_uid, include_snapshot: false, app_name: None }, session.clone()).await;
    let body = parse_json(&extract_text(&click));
    assert_eq!(body["ok"], true);

    // Focus invariant: Terminal must STILL be the active app after dispatch.
    assert_eq!(
        active_app_name(),
        "Terminal",
        "ax_click must not steal focus from Terminal"
    );

    // Dispatch invariant: Calculator's display value actually changed and
    // ends in "5". Label presence alone is not a valid oracle.
    let snap2_text = snapshot(&session, Some("Calculator")).await;
    let after = extract_calculator_display_value(&snap2_text)
        .expect("Calculator display should have a value after AXPress");
    assert_ne!(
        before, after,
        "display value must change after AXPress (before={before:?}, after={after:?})"
    );
    assert!(
        after.ends_with('5'),
        "display should end in '5' after pressing the 5 button (got {after:?})"
    );
}

// === Focus preservation — ax_set_value (TextEdit) ===

#[tokio::test]
#[ignore]
async fn ax_set_value_preserves_focus_writing_textedit_while_terminal_is_front() {
    let session = new_session();

    // Snapshot TextEdit FIRST, while we can still find it via app_name.
    // app_name targets the snapshot at a specific app regardless of
    // frontmost state — load-bearing because focus moves to Terminal next.
    let snap_text = snapshot(&session, Some("TextEdit")).await;
    // Find the document text area uid.
    let doc_line = snap_text
        .lines()
        .find(|l| l.contains("textbox") || l.contains("textarea"))
        .expect("TextEdit should expose a document text area");
    let doc_uid = doc_line
        .split_whitespace()
        .next()
        .unwrap()
        .strip_prefix("uid=")
        .unwrap()
        .to_string();

    // Switch focus to Terminal.
    let _ = focus("Terminal");

    // Precondition: Terminal really is frontmost.
    assert_eq!(
        active_app_name(),
        "Terminal",
        "precondition: Terminal should be frontmost before ax_set_value"
    );

    // Dispatch value write.
    let set = ax_set_value(
        AxSetValueParams {
            uid: doc_uid,
            text: "hello".to_string(),
            include_snapshot: false,
            app_name: None,
        },
        session.clone(),
    )
    .await;
    let body = parse_json(&extract_text(&set));
    assert_eq!(body["ok"], true, "body was {}", body);
    assert_eq!(body["dispatched_via"], "AXSetAttributeValue");

    // Focus invariant: Terminal must still be frontmost.
    assert_eq!(
        active_app_name(),
        "Terminal",
        "ax_set_value must not steal focus from Terminal"
    );

    // Dispatch invariant: TextEdit's document value is now "hello". Use a
    // fresh session for the verification snapshot so we don't bump the
    // session the previous dispatch used.
    let verify_session = new_session();
    let snap2_text = snapshot(&verify_session, Some("TextEdit")).await;
    assert!(
        snap2_text.contains("value=\"hello\""),
        "TextEdit should reflect the written value"
    );
}

// === ax_select — sidebar row selection in System Settings ===

/// Open a System Settings pane in the background (no focus steal) so the
/// test runs deterministically without cursor/focus contention.
///
/// Does not block — callers poll via `snapshot_with_sidebar_rows` for the
/// pane to reach a snapshot-worthy state.
fn open_system_settings_pane(pane_id: &str) {
    let url = format!("x-apple.systempreferences:{}", pane_id);
    let status = std::process::Command::new("open")
        .args(["-g", &url])
        .status()
        .expect("`open` should be invocable");
    assert!(
        status.success(),
        "`open -g {}` should succeed; got {:?}",
        url,
        status
    );
}

#[derive(Clone, Debug)]
struct SidebarRow {
    uid: String,
    selected: bool,
    /// First labeled descendant inside the row — usually the cell's text.
    label: String,
}

/// Collect all rows from a snapshot along with their selected state and the
/// first labeled descendant (row cells typically carry the visible text as a
/// quoted string like `"Privacy & Security"`).
fn extract_sidebar_rows(snapshot: &str) -> Vec<SidebarRow> {
    let lines: Vec<&str> = snapshot.lines().collect();
    let mut rows = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !(line.contains(" row ") || line.contains(" row\t")) {
            continue;
        }
        let Some(uid) = line
            .split_whitespace()
            .next()
            .and_then(|t| t.strip_prefix("uid="))
        else {
            continue;
        };
        let selected = line.contains("selected");
        let row_indent = line.len() - line.trim_start().len();
        let mut label = String::new();
        for follow in lines.iter().skip(i + 1) {
            let follow_indent = follow.len() - follow.trim_start().len();
            if follow_indent <= row_indent {
                break;
            }
            if let Some(start) = follow.find('"') {
                let rest = &follow[start + 1..];
                if let Some(end) = rest.find('"') {
                    if end > 0 {
                        label = rest[..end].to_string();
                        break;
                    }
                }
            }
        }
        rows.push(SidebarRow {
            uid: uid.to_string(),
            selected,
            label,
        });
    }
    rows
}

/// Poll `snapshot` until the target app's AX tree exposes at least `min_rows`
/// rows or the deadline expires. Returns the snapshot text on success.
async fn snapshot_with_sidebar_rows(
    session: &Arc<AxSession>,
    app_name: &str,
    min_rows: usize,
) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let text = snapshot(session, Some(app_name)).await;
        if extract_sidebar_rows(&text).len() >= min_rows {
            return text;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "{} did not expose {} sidebar rows within 5s; last snapshot:\n{}",
                app_name, min_rows, text
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
}

/// Smoke test: open System Settings' Privacy & Security pane, pick any
/// sidebar row whose text differs from the currently-selected one,
/// dispatch `ax_select` against it, and assert the selection moved. The
/// test is tolerant of locale and version drift — it does not require a
/// specific row to be present, only that at least two rows exist and we
/// can flip between them.
#[tokio::test]
#[ignore]
async fn ax_select_moves_sidebar_selection_in_system_settings() {
    open_system_settings_pane("com.apple.preference.security");

    let session = new_session();
    let snap_text = snapshot_with_sidebar_rows(&session, "System Settings", 2).await;
    let rows = extract_sidebar_rows(&snap_text);

    let previously_selected = rows.iter().find(|r| r.selected).cloned();
    let target = rows
        .iter()
        .find(|r| !r.selected)
        .expect("at least one sidebar row should be non-selected to target")
        .clone();

    let result = ax_select(
        AxSelectParams {
            uid: target.uid.clone(),
            include_snapshot: false,
            app_name: None,
        },
        session.clone(),
    )
    .await;
    let body = parse_json(&extract_text(&result));
    assert_eq!(
        body["ok"], true,
        "ax_select should succeed on a live sidebar row; body={}",
        body
    );
    assert_eq!(body["dispatched_via"], "AXSelectedRows");

    // Re-snapshot in a fresh session (uids don't overlap); match rows by label.
    let verify_session = new_session();
    let snap2_text = snapshot(&verify_session, Some("System Settings")).await;
    let rows2 = extract_sidebar_rows(&snap2_text);

    let now_selected = rows2
        .iter()
        .find(|r| r.label == target.label)
        .unwrap_or_else(|| {
            panic!(
                "target row (label={:?}) missing from post-dispatch snapshot",
                target.label
            )
        })
        .selected;
    assert!(
        now_selected,
        "target row (label={:?}) should be selected after ax_select",
        target.label
    );

    if let Some(prev) = previously_selected {
        if prev.label != target.label {
            if let Some(still) = rows2.iter().find(|r| r.label == prev.label) {
                assert!(
                    !still.selected,
                    "previously-selected row (label={:?}) should no longer be selected",
                    prev.label
                );
            }
        }
    }
}

/// Dispatching `ax_select` at a uid that has no `AXRow` ancestor must
/// return the `no_row_ancestor` error envelope with the fallback bbox set
/// to the starting element's centre — not panic, not succeed.
#[tokio::test]
#[ignore]
async fn ax_select_on_non_row_element_returns_no_row_ancestor() {
    // Calculator has no AXRow anywhere in its tree, so every uid is a
    // negative case.
    let session = new_session();
    let snap_text = snapshot(&session, Some("Calculator")).await;
    let five_uid = extract_uid_for_named_button(&snap_text, "5");

    let result = ax_select(AxSelectParams { uid: five_uid, include_snapshot: false, app_name: None }, session.clone()).await;
    assert_eq!(result.is_error, Some(true));
    let body = parse_json(&extract_text(&result));
    assert_eq!(body["error"]["code"], "no_row_ancestor");
    // Fallback centre should be populated from the button's bbox.
    assert!(
        body["error"]["fallback"].is_object(),
        "fallback should be populated when the starting element has a bbox"
    );
}

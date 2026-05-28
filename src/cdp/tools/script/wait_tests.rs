//! Pure unit tests for `wait.rs`. Kept in a sibling file (referenced via
//! `#[path]`) so the production `wait.rs` stays under the 600-line cap
//! even with the boundary-locking table tests for `SemanticWaitParams::from_raw`
//! and `select_strategy`.

use super::*;

// --- SemanticWaitParams::from_raw clamping table -----------------

#[test]
fn from_raw_defaults_match_documented_constants() {
    let params = SemanticWaitParams::from_raw(None, None, None, None, None, None, false);
    assert_eq!(params.timeout_ms, DEFAULT_PAGE_CHANGE_WAIT_TIMEOUT_MS);
    assert_eq!(params.poll_interval_ms, DEFAULT_PAGE_CHANGE_POLL_MS);
    assert_eq!(params.stable_ms, DEFAULT_PAGE_CHANGE_STABLE_MS);
    assert_eq!(params.condition, "semantic_delta");
    assert!(params.scope_uid.is_none());
    assert!(params.goal.is_none());
    assert!(!params.include_snapshot);
}

#[test]
fn from_raw_timeout_caps_at_max() {
    let params = SemanticWaitParams::from_raw(
        None,
        None,
        None,
        Some(MAX_PAGE_CHANGE_WAIT_TIMEOUT_MS + 10_000),
        None,
        None,
        false,
    );
    assert_eq!(params.timeout_ms, MAX_PAGE_CHANGE_WAIT_TIMEOUT_MS);
}

#[test]
fn from_raw_poll_interval_clamps_low_and_high() {
    let low = SemanticWaitParams::from_raw(None, None, None, None, Some(0), None, false);
    assert_eq!(low.poll_interval_ms, MIN_PAGE_CHANGE_POLL_MS);
    let high = SemanticWaitParams::from_raw(
        None,
        None,
        None,
        None,
        Some(MAX_PAGE_CHANGE_POLL_MS + 1),
        None,
        false,
    );
    assert_eq!(high.poll_interval_ms, MAX_PAGE_CHANGE_POLL_MS);
}

#[test]
fn from_raw_stable_ms_clamps_low_and_high() {
    let low = SemanticWaitParams::from_raw(None, None, None, None, None, Some(0), false);
    assert_eq!(low.stable_ms, MIN_PAGE_CHANGE_STABLE_MS);
    let high = SemanticWaitParams::from_raw(
        None,
        None,
        None,
        None,
        None,
        Some(MAX_PAGE_CHANGE_STABLE_MS + 1),
        false,
    );
    assert_eq!(high.stable_ms, MAX_PAGE_CHANGE_STABLE_MS);
}

#[test]
fn from_raw_condition_defaults_when_absent() {
    let params = SemanticWaitParams::from_raw(None, None, None, None, None, None, false);
    assert_eq!(params.condition, "semantic_delta");
}

#[test]
fn from_raw_condition_preserved_when_given() {
    let params = SemanticWaitParams::from_raw(
        None,
        Some("item_count_changed".to_string()),
        None,
        None,
        None,
        None,
        false,
    );
    assert_eq!(params.condition, "item_count_changed");
}

#[test]
fn from_raw_whitespace_only_scope_becomes_none() {
    let empty = SemanticWaitParams::from_raw(
        Some(String::new()),
        None,
        None,
        None,
        None,
        None,
        false,
    );
    assert!(empty.scope_uid.is_none());
    let spaces = SemanticWaitParams::from_raw(
        Some("   ".to_string()),
        None,
        None,
        None,
        None,
        None,
        false,
    );
    assert!(spaces.scope_uid.is_none());
}

#[test]
fn from_raw_non_blank_scope_preserved() {
    let params = SemanticWaitParams::from_raw(
        Some("d3".to_string()),
        None,
        None,
        None,
        None,
        None,
        false,
    );
    assert_eq!(params.scope_uid.as_deref(), Some("d3"));
}

// --- select_strategy dispatch table ------------------------------

fn params_with_scope(raw: Option<&str>) -> SemanticWaitParams {
    SemanticWaitParams::from_raw(
        raw.map(String::from),
        None,
        None,
        None,
        None,
        None,
        false,
    )
}

#[test]
fn select_strategy_routes_scope_to_scoped() {
    assert_eq!(
        select_strategy(&params_with_scope(Some("d3"))),
        WaitQuadrant::Scoped
    );
}

#[test]
fn select_strategy_routes_no_scope_to_page() {
    assert_eq!(
        select_strategy(&params_with_scope(None)),
        WaitQuadrant::Page
    );
}

#[test]
fn select_strategy_routes_empty_scope_to_page() {
    // from_raw normalises "" -> None, so select_strategy sees None.
    assert_eq!(
        select_strategy(&params_with_scope(Some(""))),
        WaitQuadrant::Page
    );
}

#[test]
fn select_strategy_routes_whitespace_scope_to_page() {
    assert_eq!(
        select_strategy(&params_with_scope(Some("   "))),
        WaitQuadrant::Page
    );
}

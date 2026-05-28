//! Unit tests for hover tracker types and polling state machine.
//! Split out of `tracker.rs` to keep that file under the 600-line cap.

use super::*;

#[test]
fn test_parse_hover_element_full() {
    let json = serde_json::json!({
        "name": "File",
        "role": "AXMenuBarItem",
        "label": "File menu",
        "value": null,
        "bounds": { "x": 100.0, "y": 200.0, "width": 40.0, "height": 22.0 },
        "app_name": "Finder",
        "pid": 1234
    });
    let el = parse_hover_element(&json);
    assert_eq!(el.name, Some("File".to_string()));
    assert_eq!(el.role, Some("AXMenuBarItem".to_string()));
    assert_eq!(el.label, Some("File menu".to_string()));
    assert_eq!(el.app_name, Some("Finder".to_string()));
    assert_eq!(el.pid, Some(1234));
    assert_eq!(
        el.bounds,
        Some(ElementBounds {
            x: 100.0,
            y: 200.0,
            width: 40.0,
            height: 22.0
        })
    );
}

#[test]
fn test_parse_hover_element_empty() {
    let json = serde_json::json!({});
    let el = parse_hover_element(&json);
    assert_eq!(el.name, None);
    assert_eq!(el.role, None);
    assert_eq!(el.bounds, None);
}

#[test]
fn test_elements_equal_same() {
    let a = HoverElement {
        name: Some("File".into()),
        role: Some("AXMenuBarItem".into()),
        label: Some("label".into()),
        bounds: Some(ElementBounds {
            x: 10.0,
            y: 20.0,
            width: 30.0,
            height: 40.0,
        }),
        app_name: Some("Finder".into()),
        pid: Some(1),
    };
    let b = HoverElement {
        name: Some("File".into()),
        role: Some("AXMenuBarItem".into()),
        label: Some("different label".into()),
        bounds: Some(ElementBounds {
            x: 10.0,
            y: 20.0,
            width: 30.0,
            height: 40.0,
        }),
        app_name: Some("Finder".into()),
        pid: Some(999),
    };
    assert!(elements_equal(&a, &b));
}

#[test]
fn test_elements_equal_different_role() {
    let a = HoverElement {
        name: Some("File".into()),
        role: Some("AXMenuBarItem".into()),
        label: None,
        bounds: None,
        app_name: None,
        pid: None,
    };
    let b = HoverElement {
        name: Some("File".into()),
        role: Some("AXButton".into()),
        label: None,
        bounds: None,
        app_name: None,
        pid: None,
    };
    assert!(!elements_equal(&a, &b));
}

#[test]
fn test_elements_equal_different_name() {
    let a = HoverElement {
        name: Some("File".into()),
        role: Some("AXMenuBarItem".into()),
        label: None,
        bounds: None,
        app_name: None,
        pid: None,
    };
    let b = HoverElement {
        name: Some("Edit".into()),
        role: Some("AXMenuBarItem".into()),
        label: None,
        bounds: None,
        app_name: None,
        pid: None,
    };
    assert!(!elements_equal(&a, &b));
}

#[test]
fn test_drain_events_clears_buffer() {
    let ts = now_millis();
    let events = Arc::new(Mutex::new(vec![HoverEvent {
        timestamp_ms: ts,
        cursor: CursorPosition { x: 1.0, y: 2.0 },
        element: HoverElement {
            name: Some("A".into()),
            role: None,
            label: None,

            bounds: None,
            app_name: None,
            pid: None,
        },
        dwell_ms: 50,
        timeout: false,
    }]));
    let cancel = CancellationToken::new();
    let tracker = HoverTracker::new(
        events.clone(),
        tokio::runtime::Runtime::new().unwrap().spawn(async {}),
        cancel,
    );

    let drained = tracker.drain_events();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].timestamp_ms, ts);

    // Second drain should be empty
    let drained2 = tracker.drain_events();
    assert!(drained2.is_empty());
}

#[test]
fn test_hover_event_serialization_omits_timeout_when_false() {
    let event = HoverEvent {
        timestamp_ms: now_millis(),
        cursor: CursorPosition { x: 1.0, y: 2.0 },
        element: HoverElement {
            name: Some("A".into()),
            role: None,
            label: None,

            bounds: None,
            app_name: None,
            pid: None,
        },
        dwell_ms: 50,
        timeout: false,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(!json.contains("timeout"));
}

#[tokio::test]
async fn test_start_polling_cancellation() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let cancel = CancellationToken::new();

    let handle = start_polling(
        events.clone(),
        cancel.clone(),
        None, // no app_name
        50,   // 50ms poll interval
        1000, // 1s max duration
        0,    // no dwell threshold
    );

    // Cancel immediately
    cancel.cancel();
    // Task should finish promptly
    tokio::time::timeout(std::time::Duration::from_millis(500), handle)
        .await
        .expect("task should finish after cancel")
        .expect("task should not panic");
}

#[tokio::test]
async fn test_start_polling_max_duration_timeout() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let cancel = CancellationToken::new();

    let handle = start_polling(
        events.clone(),
        cancel.clone(),
        None,
        50,  // 50ms poll interval
        500, // 500ms max duration
        0,   // no dwell threshold
    );

    // AX queries can be slow; allow generous margin
    tokio::time::timeout(std::time::Duration::from_millis(3000), handle)
        .await
        .expect("task should auto-stop after max duration")
        .expect("task should not panic");

    // If any element was confirmed before timeout, the last event should be
    // a timeout sentinel. If no element was confirmed (e.g. AX was too slow),
    // there may be no events at all — both are valid outcomes.
    let evts = events.lock().unwrap();
    if let Some(last) = evts.last() {
        assert!(last.timeout, "last event should be a timeout sentinel");
    }
}

#[test]
fn test_truncate_field_short_string() {
    assert_eq!(truncate_field("hello"), "hello");
}

#[test]
fn test_truncate_field_exact_limit() {
    let s = "a".repeat(MAX_FIELD_LEN);
    assert_eq!(truncate_field(&s), s);
}

#[test]
fn test_truncate_field_long_string() {
    let s = "a".repeat(MAX_FIELD_LEN + 50);
    let result = truncate_field(&s);
    assert!(result.len() <= MAX_FIELD_LEN + "…".len());
    assert!(result.ends_with('…'));
}

#[test]
fn test_truncate_field_multibyte() {
    // Ensure we don't panic on multi-byte chars at the boundary
    let s = "é".repeat(MAX_FIELD_LEN); // each é is 2 bytes
    let result = truncate_field(&s);
    assert!(result.ends_with('…'));
}

#[test]
fn test_parse_hover_element_truncates_long_name() {
    let long_name = "x".repeat(500);
    let json = serde_json::json!({
        "role": "AXStaticText",
        "name": long_name,
    });
    let el = parse_hover_element(&json);
    let name = el.name.unwrap();
    assert!(name.len() <= MAX_FIELD_LEN + "…".len());
    assert!(name.ends_with('…'));
}

#[test]
fn test_parse_hover_element_drops_value() {
    let json = serde_json::json!({
        "role": "AXTextArea",
        "value": "some text content",
    });
    let el = parse_hover_element(&json);
    // value field is not captured in HoverElement
    assert_eq!(el.role, Some("AXTextArea".to_string()));
}

#[test]
fn test_hover_event_serialization_includes_timeout_when_true() {
    let event = HoverEvent {
        timestamp_ms: now_millis(),
        cursor: CursorPosition { x: 1.0, y: 2.0 },
        element: HoverElement {
            name: None,
            role: None,
            label: None,

            bounds: None,
            app_name: None,
            pid: None,
        },
        dwell_ms: 500,
        timeout: true,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"timeout\":true"));
}

//! Hover tracking state and event types.
//!
//! Manages a background polling task that tracks cursor position and
//! accessibility element changes, emitting events on transitions.

use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::tools::now_millis;

/// A hover dwell event — emitted when the cursor leaves an element (or tracking ends).
#[derive(Debug, Clone, Serialize)]
pub struct HoverEvent {
    /// Absolute Unix milliseconds when the cursor first arrived at this element
    pub timestamp_ms: u64,
    /// Cursor position when the element was first entered
    pub cursor: CursorPosition,
    /// The accessibility element that was hovered
    pub element: HoverElement,
    /// How long the cursor stayed on this element (ms)
    pub dwell_ms: u64,
    /// If true, tracking auto-stopped due to max duration
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub timeout: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CursorPosition {
    pub x: f64,
    pub y: f64,
}

/// Accessibility element info captured during hover.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct HoverElement {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<ElementBounds>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ElementBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Active hover tracking session.
pub struct HoverTracker {
    events: Arc<Mutex<Vec<HoverEvent>>>,
    task_handle: JoinHandle<()>,
    cancel: CancellationToken,
}

impl HoverTracker {
    pub fn new(
        events: Arc<Mutex<Vec<HoverEvent>>>,
        task_handle: JoinHandle<()>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            events,
            task_handle,
            cancel,
        }
    }

    /// Check if the background polling task has finished (due to timeout or error).
    pub fn is_finished(&self) -> bool {
        self.task_handle.is_finished()
    }

    /// Drain all buffered events, returning them and clearing the buffer.
    pub fn drain_events(&self) -> Vec<HoverEvent> {
        let mut events = self.events.lock().unwrap();
        events.drain(..).collect()
    }

    /// Cancel tracking, await task shutdown, then drain remaining events.
    ///
    /// Drains after the task finishes to avoid losing late events from
    /// in-flight `spawn_blocking` calls. Aborts the task if it doesn't
    /// stop within 500ms (e.g. slow AX query).
    pub async fn cancel_and_drain(self) -> Vec<HoverEvent> {
        self.cancel.cancel();
        let Self {
            events,
            mut task_handle,
            ..
        } = self;
        if tokio::time::timeout(std::time::Duration::from_millis(500), &mut task_handle)
            .await
            .is_err()
        {
            task_handle.abort();
        }
        let mut buf = events.lock().unwrap();
        buf.drain(..).collect()
    }
}

/// Max characters for string fields in hover events.
/// Keeps output compact — full element text (e.g. terminal buffers) is noise for hover tracking.
const MAX_FIELD_LEN: usize = 100;

/// Truncate a string to `MAX_FIELD_LEN`, appending "…" if truncated.
fn truncate_field(s: &str) -> String {
    if s.len() <= MAX_FIELD_LEN {
        s.to_string()
    } else {
        // Find a char boundary at or before MAX_FIELD_LEN
        let mut end = MAX_FIELD_LEN;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

/// Parse a `serde_json::Value` (from `element_at_point`) into a `HoverElement`.
pub fn parse_hover_element(value: &serde_json::Value) -> HoverElement {
    let str_field = |key: &str| -> Option<String> {
        value.get(key).and_then(|v| v.as_str()).map(truncate_field)
    };

    HoverElement {
        name: str_field("name"),
        role: str_field("role"),
        label: str_field("label"),
        bounds: value.get("bounds").map(|b| ElementBounds {
            x: b.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0),
            y: b.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0),
            width: b.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0),
            height: b.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0),
        }),
        app_name: str_field("app_name"),
        pid: value.get("pid").and_then(|v| v.as_i64()).map(|p| p as i32),
    }
}

/// Check if two elements are the same (by role + name + bounds).
pub fn elements_equal(a: &HoverElement, b: &HoverElement) -> bool {
    a.role == b.role && a.name == b.name && a.bounds == b.bounds
}

/// Snapshot of an element that has met (or is being evaluated against) the dwell threshold.
/// Used for both the confirmed hover and the candidate hover.
struct HoverEntry {
    element: HoverElement,
    since: Instant,
    enter_ms: u64,
    cursor: (f64, f64),
}

impl HoverEntry {
    /// Build a leave/timeout event from this entry's stored state.
    /// `left_at` is the monotonic instant the cursor departed this element.
    fn into_event(self, left_at: Instant, timeout: bool) -> HoverEvent {
        HoverEvent {
            timestamp_ms: self.enter_ms,
            cursor: CursorPosition {
                x: self.cursor.0,
                y: self.cursor.1,
            },
            element: self.element,
            dwell_ms: left_at.duration_since(self.since).as_millis() as u64,
            timeout,
        }
    }
}

/// Start the hover polling background task.
///
/// Polls cursor position + element_at_point every `poll_interval_ms`,
/// pushing a `HoverEvent` when the element under the cursor changes and
/// the cursor has dwelled on the new element for at least `min_dwell_ms`.
/// This filters out pass-through elements during fast mouse movement.
/// Stops when `cancel` is triggered or `max_duration_ms` elapses.
pub fn start_polling(
    events: Arc<Mutex<Vec<HoverEvent>>>,
    cancel: CancellationToken,
    app_name: Option<String>,
    poll_interval_ms: u32,
    max_duration_ms: u32,
    min_dwell_ms: u32,
) -> JoinHandle<()> {
    // Use Arc<str> to avoid cloning the string on every poll tick
    let app_name: Option<Arc<str>> = app_name.map(|s| Arc::from(s.as_str()));

    tokio::spawn(async move {
        let start = Instant::now();
        let max_duration = std::time::Duration::from_millis(max_duration_ms as u64);
        let poll_interval = std::time::Duration::from_millis(poll_interval_ms as u64);
        let min_dwell = std::time::Duration::from_millis(min_dwell_ms as u64);

        // The element currently being hovered (confirmed after meeting dwell threshold).
        // We emit an event about this element when the cursor leaves it.
        let mut confirmed: Option<HoverEntry> = None;
        // Monotonic instant when the cursor first left the confirmed element
        // (i.e. when the first candidate appeared). Persists across candidate
        // replacements so pass-through elements don't inflate the dwell.
        let mut first_departure: Option<Instant> = None;

        // A candidate element that differs from confirmed but hasn't met dwell threshold yet.
        let mut candidate: Option<HoverEntry> = None;

        loop {
            // Check cancellation
            if cancel.is_cancelled() {
                return;
            }

            // Check max duration
            if start.elapsed() >= max_duration {
                // Emit the confirmed element's dwell before stopping.
                // Use first_departure if cursor had already left, otherwise now.
                if let Some(entry) = confirmed {
                    let left_at = first_departure.unwrap_or_else(Instant::now);
                    events.lock().unwrap().push(entry.into_event(left_at, true));
                }
                return;
            }

            // Get cursor position + element in a single spawn_blocking call
            let app = app_name.clone();
            let poll_result = tokio::task::spawn_blocking(move || {
                let cursor = get_cursor_position_sync()?;
                let element = element_at_point_for_hover(cursor.0, cursor.1, app.as_deref())?;
                Ok::<_, String>((cursor, element))
            })
            .await;

            let (cursor, current_element) = match poll_result {
                Ok(Ok(result)) => result,
                _ => {
                    tokio::time::sleep(poll_interval).await;
                    continue;
                }
            };

            // Is this element different from the last confirmed one?
            let differs_from_confirmed = match &confirmed {
                Some(c) => !elements_equal(&c.element, &current_element),
                None => true, // First element is always new
            };

            if !differs_from_confirmed {
                // Cursor moved back to confirmed element — discard candidate
                candidate = None;
                first_departure = None;
                tokio::time::sleep(poll_interval).await;
                continue;
            }

            // Element differs from confirmed — check candidate state
            let cand_matches = candidate
                .as_ref()
                .is_some_and(|c| elements_equal(&c.element, &current_element));

            if cand_matches {
                let cand = candidate.as_ref().unwrap();
                // Same candidate — check if dwell threshold met
                if cand.since.elapsed() >= min_dwell {
                    // Emit event about the element being LEFT.
                    // Use first_departure (when cursor first left) rather than
                    // cand.since (when cursor arrived at the final candidate),
                    // so pass-through elements don't inflate the dwell.
                    if let Some(prev) = confirmed.take() {
                        let departed = first_departure.unwrap_or(cand.since);
                        events
                            .lock()
                            .unwrap()
                            .push(prev.into_event(departed, false));
                    }
                    // Promote candidate to confirmed
                    confirmed = candidate.take();
                    first_departure = None;
                }
                // else: keep waiting
            } else {
                // New candidate (or different from previous candidate).
                // Record first departure time only on the initial candidate
                // after a confirmed element — subsequent replacements keep it.
                if first_departure.is_none() && confirmed.is_some() {
                    first_departure = Some(Instant::now());
                }
                candidate = Some(HoverEntry {
                    element: current_element,
                    since: Instant::now(),
                    enter_ms: now_millis(),
                    cursor,
                });
            }

            tokio::time::sleep(poll_interval).await;
        }
    })
}

/// Get cursor position synchronously (fast CGEvent read, no spawn_blocking needed).
fn get_cursor_position_sync() -> Result<(f64, f64), String> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::input::get_cursor_position()
    }
    #[cfg(target_os = "windows")]
    {
        crate::windows::input::get_cursor_position()
    }
}

/// Query element_at_point for hover tracking (wraps platform call).
fn element_at_point_for_hover(
    x: f64,
    y: f64,
    app_name: Option<&str>,
) -> Result<HoverElement, String> {
    #[cfg(target_os = "macos")]
    {
        let value = crate::macos::ax::element_at_point(x, y, app_name)?;
        Ok(parse_hover_element(&value))
    }
    #[cfg(target_os = "windows")]
    {
        let value = crate::windows::uia::element_at_point(x, y, app_name)?;
        Ok(parse_hover_element(&value))
    }
}

#[cfg(test)]
#[path = "tracker_tests.rs"]
mod tracker_tests;

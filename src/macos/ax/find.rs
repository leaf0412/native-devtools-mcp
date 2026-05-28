//! AX-tree search and probe entry points: text search, name enumeration,
//! and point-based element lookup. Built on top of `tree::walk_ax_tree`
//! and the `attr` read seam.

use super::app::{
    app_name_for_pid, frontmost_pid, get_pid_for_element, pid_for_app_name, pid_for_window,
};
use super::attr;
use super::ffi::{
    AXUIElementCopyElementAtPosition, AXUIElementCreateApplication, AXUIElementCreateSystemWide,
    AXUIElementRef, K_AX_ERROR_SUCCESS,
};
use super::tree::{hit_test_tree, walk_ax_tree};
use crate::macos::ocr::{TextBounds, TextMatch};
use std::ptr;

/// Find text in UI elements of an app's accessibility tree.
///
/// Searches the accessibility tree for elements whose AXTitle, AXValue, or
/// AXDescription contains the search string (case-insensitive).
/// Returns matching elements with screen coordinates for clicking.
pub fn find_text(search: &str, window_id: Option<u32>) -> Result<Vec<TextMatch>, String> {
    let debug = std::env::var("NATIVE_DEVTOOLS_DEBUG").is_ok();

    let pid = match window_id {
        Some(wid) => pid_for_window(wid)?,
        None => frontmost_pid()?,
    };

    if debug {
        eprintln!(
            "[DEBUG ax::find_text] search='{}', window_id={:?}, pid={}",
            search, window_id, pid
        );
    }

    let app_element = unsafe { AXUIElementCreateApplication(pid) };
    if app_element.is_null() {
        return Err(format!("Failed to create AXUIElement for pid {}", pid));
    }

    let search_lower = search.to_lowercase();
    let mut matches = Vec::new();
    let mut element_count: usize = 0;

    unsafe {
        walk_ax_tree(app_element, &mut element_count, 0, &mut |element| {
            // `.ok().flatten()` is the project-wide convention for re-conflating
            // attr::* `Result<Option<T>, AxError>` into the legacy Option<T>
            // shape — Err (FFI / Decode failure) collapses to None alongside
            // legitimately-absent values. See ax::attr module docs.
            let matched_text = ["AXTitle", "AXValue", "AXDescription"]
                .iter()
                .filter_map(|key| attr::string(element, key).ok().flatten())
                .find(|s| !s.is_empty() && s.to_lowercase().contains(search_lower.as_str()));

            if let Some(text) = matched_text {
                let position = attr::point(element, "AXPosition").ok().flatten();
                let size = attr::size(element, "AXSize").ok().flatten();
                if let (Some(position), Some(size)) = (position, size) {
                    if size.width > 0.0 && size.height > 0.0 {
                        let role = attr::string(element, "AXRole").ok().flatten();
                        let bounds = TextBounds {
                            x: position.x,
                            y: position.y,
                            width: size.width,
                            height: size.height,
                        };
                        matches.push(TextMatch {
                            text,
                            x: bounds.x + bounds.width / 2.0,
                            y: bounds.y + bounds.height / 2.0,
                            confidence: 1.0,
                            bounds,
                            role,
                        });
                    }
                }
            }
        });
        core_foundation::base::CFRelease(app_element as core_foundation::base::CFTypeRef);
    }

    if debug {
        eprintln!(
            "[DEBUG ax::find_text] found {} matches out of {} elements",
            matches.len(),
            element_count
        );
    }

    Ok(matches)
}

/// Container roles where `AXUIElementCopyElementAtPosition` may stop too
/// early (e.g. Electron/Chromium web views). When the hit element has one of
/// these roles, we drill deeper into AX children to find the most specific
/// element at the coordinates.
fn is_container_role(role: &str) -> bool {
    matches!(
        role,
        "AXScrollArea"
            | "AXWebArea"
            | "AXGroup"
            | "AXSplitGroup"
            | "AXLayoutArea"
            | "AXList"
            | "AXOutline"
            | "AXTable"
            | "AXBrowser"
    )
}

/// Get the accessibility element at the given screen coordinates.
///
/// Uses `AXUIElementCopyElementAtPosition` to find the element at (x, y).
/// If the result is a container (e.g. AXScrollArea in Electron apps), drills
/// deeper into AX children to find the most specific element.
/// If `app_name` is provided, scopes the lookup to that app; otherwise uses
/// a system-wide lookup.
pub fn element_at_point(
    x: f64,
    y: f64,
    app_name: Option<&str>,
) -> Result<serde_json::Value, String> {
    let root = if let Some(name) = app_name {
        let pid = pid_for_app_name(name)?;
        let el = unsafe { AXUIElementCreateApplication(pid) };
        if el.is_null() {
            return Err(format!("Failed to create AXUIElement for app '{}'", name));
        }
        el
    } else {
        let el = unsafe { AXUIElementCreateSystemWide() };
        if el.is_null() {
            return Err("Failed to create system-wide AXUIElement".to_string());
        }
        el
    };

    let mut element: AXUIElementRef = ptr::null_mut();
    let err = unsafe { AXUIElementCopyElementAtPosition(root, x as f32, y as f32, &mut element) };

    unsafe {
        core_foundation::base::CFRelease(root as core_foundation::base::CFTypeRef);
    }

    if err != K_AX_ERROR_SUCCESS || element.is_null() {
        return Err(format!("No accessibility element found at ({}, {})", x, y));
    }

    // Read PID early — needed for both paths and must happen before release.
    let pid = unsafe { get_pid_for_element(element) };

    // If the hit element is a container, try a full tree-walk hit-test
    // from the app root. Needed for Electron/Chromium apps where
    // AXUIElementCopyElementAtPosition returns a shallow container whose
    // element reference exposes no children.
    let role_str = attr::string(element, "AXRole").ok().flatten();
    let is_container = role_str.as_deref().is_some_and(is_container_role);

    let (name, role, subrole, label, value, bounds) = if is_container {
        unsafe {
            core_foundation::base::CFRelease(element as core_foundation::base::CFTypeRef);
        }
        let hit = pid.and_then(|p| unsafe {
            let app = AXUIElementCreateApplication(p);
            if app.is_null() {
                return None;
            }
            let result = hit_test_tree(app, x, y);
            core_foundation::base::CFRelease(app as core_foundation::base::CFTypeRef);
            result
        });
        match hit {
            Some(h) => (
                h.name,
                h.role,
                h.subrole,
                h.label,
                h.value,
                Some((h.position, h.size)),
            ),
            None => (None, role_str, None, None, None, None),
        }
    } else {
        let name = attr::string(element, "AXTitle").ok().flatten();
        let subrole = attr::string(element, "AXSubrole").ok().flatten();
        let label = attr::string(element, "AXDescription").ok().flatten();
        let value = attr::string(element, "AXValue").ok().flatten();
        let position = attr::point(element, "AXPosition").ok().flatten();
        let size = attr::size(element, "AXSize").ok().flatten();
        let bounds = match (position, size) {
            (Some(p), Some(s)) => Some((p, s)),
            _ => None,
        };
        unsafe {
            core_foundation::base::CFRelease(element as core_foundation::base::CFTypeRef);
        }
        (name, role_str, subrole, label, value, bounds)
    };

    // Resolve app name from PID
    let resolved_app_name = pid.and_then(app_name_for_pid);

    // Build response, omitting null fields
    let mut result = serde_json::Map::new();

    if let Some(r) = role {
        result.insert("role".to_string(), serde_json::Value::String(r));
    }
    if let Some(sr) = subrole {
        result.insert("subrole".to_string(), serde_json::Value::String(sr));
    }
    if let Some(n) = name {
        result.insert("name".to_string(), serde_json::Value::String(n));
    }
    if let Some(l) = label {
        result.insert("label".to_string(), serde_json::Value::String(l));
    }
    if let Some(v) = value {
        result.insert("value".to_string(), serde_json::Value::String(v));
    }
    if let Some((pos, size)) = bounds {
        result.insert(
            "bounds".to_string(),
            serde_json::json!({
                "x": pos.x,
                "y": pos.y,
                "width": size.width,
                "height": size.height,
            }),
        );
    }
    if let Some(p) = pid {
        result.insert("pid".to_string(), serde_json::Value::Number(p.into()));
    }
    if let Some(a) = resolved_app_name {
        result.insert("app_name".to_string(), serde_json::Value::String(a));
    }

    Ok(serde_json::Value::Object(result))
}

/// Collect all unique non-empty element names from the accessibility tree.
/// Used to provide a list of available elements when a search returns no
/// matches.
pub fn list_element_names(window_id: Option<u32>) -> Result<Vec<String>, String> {
    let pid = match window_id {
        Some(wid) => pid_for_window(wid)?,
        None => frontmost_pid()?,
    };

    let app_element = unsafe { AXUIElementCreateApplication(pid) };
    if app_element.is_null() {
        return Err(format!("Failed to create AXUIElement for pid {}", pid));
    }

    let mut names = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut element_count: usize = 0;

    unsafe {
        walk_ax_tree(app_element, &mut element_count, 0, &mut |element| {
            for key in &["AXTitle", "AXValue", "AXDescription"] {
                if let Some(text) = attr::string(element, key).ok().flatten() {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
                        names.push(trimmed.to_string());
                    }
                }
            }
        });
        core_foundation::base::CFRelease(app_element as core_foundation::base::CFTypeRef);
    }

    Ok(names)
}

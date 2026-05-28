//! macOS Accessibility API (AXUIElement) helpers.
//!
//! Uses the macOS Accessibility tree for:
//! - Text search: find UI elements by name (faster than OCR for standard controls)
//! - Window raising: bring windows to front via AXRaise (works for bundle-less apps)
//!
//! Module layout (post-T4 split — currently being migrated):
//! - `attr`: the sole owner of `AXUIElementCopyAttributeValue` (typed read seam).
//! - `dispatch`: write-side classified errors (`AXDispatchError`, `press_element`,
//!   `set_value_attribute`, `select_rows_attribute`).
//! - `tree`: tree walks (snapshot, hit-test, ancestor chain, bbox).
//! - `app`: PID resolution + window raising.
//! - `find`: text search + element-at-point + element name listing.
//! - `ffi`: extern "C" block + AX error/type constants + type aliases.

#[allow(dead_code, unused_imports)]
mod attr;
#[allow(dead_code, unused_imports)]
mod dispatch;
mod tree;
#[allow(dead_code, unused_imports)]
mod app;
mod find;
#[allow(dead_code, unused_imports)]
mod ffi;

// Public re-exports of relocated entry points so call sites continue
// resolving through `crate::macos::ax::*`. Phasing: as each step relocates
// more, this list grows; the module surface to external callers stays flat.
pub use find::{element_at_point, list_element_names};
pub use tree::collect_ax_tree_indexed;
pub(crate) use tree::{ancestor_role_chain, element_bbox};

use super::ocr::{TextBounds, TextMatch};
use core_foundation::array::{kCFTypeArrayCallBacks, CFArray, CFArrayCreate};
use core_foundation::base::{kCFAllocatorDefault, CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;
use core_graphics::geometry::{CGPoint, CGSize};
use objc::runtime::{Class, Object};
use objc::{msg_send, sel, sel_impl};
use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;

use ffi::*;

pub(super) const MAX_DEPTH: u32 = 50;
pub(super) const MAX_ELEMENTS: usize = 10_000;

/// Retained, thread-safe handle to an `AXUIElement`.
///
/// The raw `AXUIElementRef` (`*mut c_void`) is not `Send`/`Sync` by default,
/// but Apple documents the Accessibility API as safe to invoke from any
/// thread, and `CFRetain` / `CFRelease` are atomic. Cross-thread sharing is
/// real: `AXRef`s live inside `AxSession.current:
/// RwLock<Option<AxSnapshot>>`, held on `MacOSDevToolsServer` as
/// `ax_session: Arc<AxSession>`, and `ServerHandler::call_tool` runs on the
/// tokio multi-threaded runtime with `&self`. Any tool call can read the
/// session concurrently; `take_ax_snapshot` writes it under the write half
/// of the lock. The outer `Arc` makes clones free (no `CFRetain` per
/// handoff); the inner `Drop` preserves single-`CFRelease` per retained
/// element.
#[derive(Clone)]
pub struct AXRef(Arc<AXRefInner>);

struct AXRefInner(AXUIElementRef);

// SAFETY: see docs on `AXRef`. Apple's AX API is documented thread-safe;
// CFRetain/CFRelease are atomic.
unsafe impl Send for AXRefInner {}
unsafe impl Sync for AXRefInner {}

impl Drop for AXRefInner {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                core_foundation::base::CFRelease(self.0 as core_foundation::base::CFTypeRef);
            }
        }
    }
}

impl std::fmt::Debug for AXRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AXRef({:p})", self.0 .0)
    }
}

impl AXRef {
    /// Wrap a raw `AXUIElementRef` under the **create rule**: caller already
    /// holds a +1 refcount and transfers ownership to the `AXRef`. Drop will
    /// balance with a single `CFRelease`. Do not call `CFRelease` on the raw
    /// pointer after calling this.
    pub(crate) unsafe fn from_create(raw: AXUIElementRef) -> Self {
        AXRef(Arc::new(AXRefInner(raw)))
    }

    /// Wrap a raw `AXUIElementRef` under the **get rule**: the caller holds a
    /// borrowed reference. This function calls `CFRetain` to take ownership,
    /// and Drop will balance with `CFRelease`.
    pub(crate) unsafe fn from_get(raw: AXUIElementRef) -> Self {
        if !raw.is_null() {
            core_foundation::base::CFRetain(raw as core_foundation::base::CFTypeRef);
        }
        AXRef(Arc::new(AXRefInner(raw)))
    }

    /// Access the raw `AXUIElementRef` for FFI. The `AXRef` must outlive the
    /// borrow — the lifetime is bound to `&self`.
    pub(crate) fn as_raw(&self) -> AXUIElementRef {
        self.0 .0
    }
}

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
        tree::walk_ax_tree(app_element, &mut element_count, 0, &mut |element| {
            // `.ok().flatten()` is the project-wide convention for re-conflating
            // attr::* `Result<Option<T>, AxError>` into the legacy Option<T>
            // shape — Err (FFI / Decode failure) collapses to None alongside
            // legitimately-absent values, matching prior `get_string_attribute`
            // behavior. See ax::attr module docs.
            let matched_text = ["AXTitle", "AXValue", "AXDescription"]
                .iter()
                .filter_map(|attr| attr::string(element, attr).ok().flatten())
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

/// Get a string attribute from an AX element. Returns None if the attribute
/// doesn't exist or isn't a string.
///
/// Dead at the call-site level after step 5; deleted in step 8 once
/// `raise_windows`'s AXWindows read and `ax_parent` both route through
/// the seam. `#[allow(dead_code)]` is intentional and bounded.
#[allow(dead_code)]
unsafe fn get_string_attribute(element: AXUIElementRef, attr_name: &str) -> Option<String> {
    let attr = CFString::new(attr_name);
    let mut value_ref: core_foundation::base::CFTypeRef = ptr::null();
    let err = AXUIElementCopyAttributeValue(element, attr.as_concrete_TypeRef(), &mut value_ref);

    if err != K_AX_ERROR_SUCCESS || value_ref.is_null() {
        return None;
    }

    // value_ref is owned (create rule) — wrap it so it gets released.
    // downcast_into consumes cf_value, avoiding an extra retain/release cycle.
    let cf_string = CFType::wrap_under_create_rule(value_ref).downcast_into::<CFString>()?;
    Some(cf_string.to_string())
}

/// Get a boolean attribute from an AX element. Returns None if the attribute
/// doesn't exist or isn't a CFBoolean.
#[allow(dead_code)]
unsafe fn get_bool_attribute(element: AXUIElementRef, attr_name: &str) -> Option<bool> {
    let attr = CFString::new(attr_name);
    let mut value_ref: core_foundation::base::CFTypeRef = ptr::null();
    let err = AXUIElementCopyAttributeValue(element, attr.as_concrete_TypeRef(), &mut value_ref);

    if err != K_AX_ERROR_SUCCESS || value_ref.is_null() {
        return None;
    }

    // value_ref is owned (create rule) — wrap it so it gets released.
    let cf_bool = CFType::wrap_under_create_rule(value_ref).downcast_into::<CFBoolean>()?;
    Some(bool::from(cf_bool))
}

/// Get position (CGPoint) and size (CGSize) of an AX element.
/// Returns None if either attribute is missing.
#[allow(dead_code)]
unsafe fn get_position_and_size(element: AXUIElementRef) -> Option<(CGPoint, CGSize)> {
    let position: CGPoint = get_ax_value(element, "AXPosition", K_AX_VALUE_TYPE_CGPOINT)?;
    let size: CGSize = get_ax_value(element, "AXSize", K_AX_VALUE_TYPE_CGSIZE)?;
    Some((position, size))
}

/// Outcome of an `AXUIElementPerformAction` / `AXUIElementSetAttributeValue`
/// call, classified into the MCP error taxonomy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AXDispatchError {
    /// Action or attribute not supported by this element (element is not a
    /// valid dispatch target — e.g. decorative label, read-only field).
    NotDispatchable,
    /// Any other AX error code. Carries the raw integer for diagnostics.
    AxError(i32),
}

impl AXDispatchError {
    /// Classify the return value of `AXUIElementPerformAction(kAXPressAction)`.
    /// Returns `None` for success.
    pub fn from_press_code(code: i32) -> Option<Self> {
        match code {
            K_AX_ERROR_SUCCESS => None,
            K_AX_ERROR_ACTION_UNSUPPORTED => Some(AXDispatchError::NotDispatchable),
            other => Some(AXDispatchError::AxError(other)),
        }
    }

    /// Classify the return value of
    /// `AXUIElementSetAttributeValue(kAXValueAttribute, ...)`. Returns
    /// `None` for success.
    pub fn from_set_value_code(code: i32) -> Option<Self> {
        match code {
            K_AX_ERROR_SUCCESS => None,
            K_AX_ERROR_ATTRIBUTE_UNSUPPORTED | K_AX_ERROR_ILLEGAL_ARGUMENT => {
                Some(AXDispatchError::NotDispatchable)
            }
            other => Some(AXDispatchError::AxError(other)),
        }
    }
}

/// Perform `kAXPressAction` on an element. Returns `Ok(())` on success.
///
/// Narrowed to `pub(crate)` so external Rust consumers cannot combine it
/// with `AxSession::lookup` to recreate the lookup-then-dispatch race that
/// `AxSession::dispatch` exists to close. The blessed entry point for
/// dispatch is the session-pinned `dispatch` method.
pub(crate) fn press_element(element: &AXRef) -> Result<(), AXDispatchError> {
    let action = CFString::new("AXPress");
    let code = unsafe { AXUIElementPerformAction(element.as_raw(), action.as_concrete_TypeRef()) };
    match AXDispatchError::from_press_code(code) {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

/// Write `text` into an element's `kAXValueAttribute`. Returns `Ok(())` on
/// success.
///
/// This is value assignment, not key-event typing: the target app does not
/// observe keydown/keyup, does not see IME composition events, and does not
/// record the change on its undo stack. Elements whose role does not expose
/// a writable `kAXValueAttribute` return `NotDispatchable`.
///
/// Narrowed to `pub(crate)` so external Rust consumers cannot combine it
/// with `AxSession::lookup` to recreate the lookup-then-dispatch race that
/// `AxSession::dispatch` exists to close.
pub(crate) fn set_value_attribute(element: &AXRef, text: &str) -> Result<(), AXDispatchError> {
    let attr = CFString::new("AXValue");
    let value = CFString::new(text);
    let code = unsafe {
        AXUIElementSetAttributeValue(
            element.as_raw(),
            attr.as_concrete_TypeRef(),
            value.as_concrete_TypeRef() as core_foundation::base::CFTypeRef,
        )
    };
    match AXDispatchError::from_set_value_code(code) {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

/// Write `rows` into the `AXSelectedRows` attribute of an outline/table.
/// Returns `Ok(())` on success.
///
/// Callers must have already walked up to the enclosing `AXOutline` /
/// `AXTable` and verified the row is a direct descendant. The MVP selects
/// exactly one row per call — the list shape leaves room for multi-row
/// selection later without re-plumbing the FFI boundary.
///
/// Narrowed to `pub(crate)` so external Rust consumers cannot combine it
/// with `AxSession::lookup` to recreate the lookup-then-dispatch race that
/// `AxSession::dispatch` exists to close.
pub(crate) fn select_rows_attribute(
    container: &AXRef,
    rows: &[&AXRef],
) -> Result<(), AXDispatchError> {
    let attr = CFString::new("AXSelectedRows");
    // Build a CFArray<AXUIElementRef>. `kCFTypeArrayCallBacks` tells CFArray
    // to CFRetain each pointer on insert and CFRelease on destruction, which
    // is what we want for `AXUIElementRef` — a CFType under the hood. Using
    // `CFArray::from_copyable` would pass a null callback set, producing a
    // bag of unmanaged raw pointers that AX probably still accepts but that
    // leaks refcount semantics across the FFI boundary.
    let raw_ptrs: Vec<*const c_void> = rows.iter().map(|r| r.as_raw() as *const c_void).collect();
    let cf_array_ref = unsafe {
        CFArrayCreate(
            kCFAllocatorDefault,
            raw_ptrs.as_ptr(),
            raw_ptrs.len() as core_foundation::base::CFIndex,
            &kCFTypeArrayCallBacks,
        )
    };
    let cf_array: CFArray<*const c_void> = unsafe { CFArray::wrap_under_create_rule(cf_array_ref) };
    let code = unsafe {
        AXUIElementSetAttributeValue(
            container.as_raw(),
            attr.as_concrete_TypeRef(),
            cf_array.as_concrete_TypeRef() as core_foundation::base::CFTypeRef,
        )
    };
    match AXDispatchError::from_set_value_code(code) {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

/// Extract a typed value (CGPoint or CGSize) from an AXValue attribute.
#[allow(dead_code)]
unsafe fn get_ax_value<T: Default>(
    element: AXUIElementRef,
    attr_name: &str,
    ax_value_type: u32,
) -> Option<T> {
    let attr = CFString::new(attr_name);
    let mut value_ref: core_foundation::base::CFTypeRef = ptr::null();
    let err = AXUIElementCopyAttributeValue(element, attr.as_concrete_TypeRef(), &mut value_ref);

    if err != K_AX_ERROR_SUCCESS || value_ref.is_null() {
        return None;
    }

    let mut result = T::default();
    let ok = AXValueGetValue(
        value_ref as AXValueRef,
        ax_value_type,
        &mut result as *mut T as *mut c_void,
    );

    core_foundation::base::CFRelease(value_ref);

    if ok {
        Some(result)
    } else {
        None
    }
}

/// Get the PID that owns a given window ID, using CGWindowListCopyWindowInfo.
pub(super) fn pid_for_window(window_id: u32) -> Result<i32, String> {
    let window = super::window::find_window_by_id(window_id)?
        .ok_or_else(|| format!("Window {} not found", window_id))?;
    i32::try_from(window.owner_pid)
        .map_err(|_| format!("PID {} exceeds i32 range", window.owner_pid))
}

/// Get the application name for a PID via NSRunningApplication.
pub(super) fn app_name_for_pid(pid: i32) -> Option<String> {
    unsafe {
        let app: *mut Object = msg_send![
            Class::get("NSRunningApplication")?,
            runningApplicationWithProcessIdentifier: pid
        ];
        if app.is_null() {
            return None;
        }
        let name_ns: *mut Object = msg_send![app, localizedName];
        if name_ns.is_null() {
            return None;
        }
        let utf8_ptr: *const std::ffi::c_char = msg_send![name_ns, UTF8String];
        if utf8_ptr.is_null() {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr(utf8_ptr)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// Get the PID of the frontmost application via NSWorkspace.
pub(crate) fn frontmost_pid() -> Result<i32, String> {
    unsafe {
        let cls = Class::get("NSWorkspace").ok_or("NSWorkspace class not available")?;
        let workspace: *mut Object = msg_send![cls, sharedWorkspace];
        if workspace.is_null() {
            return Err("NSWorkspace.sharedWorkspace returned nil".to_string());
        }
        let app: *mut Object = msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return Err("No frontmost application found".to_string());
        }
        let pid: i32 = msg_send![app, processIdentifier];
        Ok(pid)
    }
}

/// Resolve app_name to PID by finding the first matching window.
pub(super) fn pid_for_app_name(app_name: &str) -> Result<i32, String> {
    let windows = super::window::find_windows_by_app(app_name)
        .map_err(|e| format!("Failed to find windows: {}", e))?;
    let win = windows.first().ok_or_else(|| {
        format!(
            "No app found matching '{}'. Use list_apps to find the correct app name.",
            app_name
        )
    })?;
    i32::try_from(win.owner_pid).map_err(|_| format!("PID {} exceeds i32 range", win.owner_pid))
}

/// Get the PID of the process that owns an AX element.
pub(super) unsafe fn get_pid_for_element(element: AXUIElementRef) -> Option<i32> {
    let mut pid: i32 = 0;
    if AXUIElementGetPid(element, &mut pid) == K_AX_ERROR_SUCCESS {
        Some(pid)
    } else {
        None
    }
}

/// Raise all windows of an application to the front using the Accessibility API.
///
/// Two-step approach:
/// 1. Set AXFrontmost on the app element (equivalent to System Events `set frontmost`)
/// 2. AXRaise on each window (physically brings windows to front)
///
/// Step 1 is critical for apps without a proper macOS app bundle (e.g. Tauri dev builds)
/// where NSRunningApplication.activate reports success but doesn't bring windows to front.
pub fn raise_windows(pid: i32) -> bool {
    let debug = std::env::var("NATIVE_DEVTOOLS_DEBUG").is_ok();

    unsafe {
        let app_element = AXUIElementCreateApplication(pid);
        if app_element.is_null() {
            if debug {
                eprintln!(
                    "[DEBUG ax::raise_windows] Failed to create AXUIElement for pid {}",
                    pid
                );
            }
            return false;
        }

        // Step 1: Set AXFrontmost on the app element to make it the frontmost process.
        // This is the programmatic equivalent of AppleScript:
        //   tell application "System Events" to set frontmost of process "X" to true
        let frontmost_attr = CFString::new("AXFrontmost");
        let frontmost_err = AXUIElementSetAttributeValue(
            app_element,
            frontmost_attr.as_concrete_TypeRef(),
            core_foundation::boolean::CFBoolean::true_value().as_CFTypeRef(),
        );
        if debug {
            eprintln!(
                "[DEBUG ax::raise_windows] AXFrontmost set for pid {} (err={})",
                pid, frontmost_err
            );
        }

        // Step 2: AXRaise each window to bring them to front in the window order.
        let windows_attr = CFString::new("AXWindows");
        let mut windows_ref: core_foundation::base::CFTypeRef = ptr::null();
        let err = AXUIElementCopyAttributeValue(
            app_element,
            windows_attr.as_concrete_TypeRef(),
            &mut windows_ref,
        );

        let mut raised = frontmost_err == K_AX_ERROR_SUCCESS;

        if err == K_AX_ERROR_SUCCESS && !windows_ref.is_null() {
            let windows: CFArray<*const c_void> = CFArray::wrap_under_create_rule(windows_ref as _);
            let raise_action = CFString::new("AXRaise");

            for i in 0..windows.len() {
                let window = *windows.get_unchecked(i) as AXUIElementRef;
                let result = AXUIElementPerformAction(window, raise_action.as_concrete_TypeRef());
                if result == K_AX_ERROR_SUCCESS {
                    raised = true;
                } else if debug {
                    eprintln!(
                        "[DEBUG ax::raise_windows] AXRaise failed for window {} (err={})",
                        i, result
                    );
                }
            }

            if debug {
                eprintln!(
                    "[DEBUG ax::raise_windows] pid={}, windows={}, raised={}",
                    pid,
                    windows.len(),
                    raised
                );
            }
        } else if debug {
            eprintln!(
                "[DEBUG ax::raise_windows] No AXWindows for pid {} (err={})",
                pid, err
            );
        }

        core_foundation::base::CFRelease(app_element as core_foundation::base::CFTypeRef);
        raised
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Search for "9" in the frontmost app (Calculator).
    /// Requires Calculator to be running and in the foreground.
    /// Run with: cargo test test_ax_find_text_calculator -- --ignored --nocapture
    #[test]
    #[ignore]
    fn test_ax_find_text_calculator() {
        let results = find_text("9", None).expect("find_text should succeed");
        println!("AX find_text results for '9' (no window_id):");
        for m in &results {
            println!(
                "  '{}' at ({:.1}, {:.1}) bounds=({:.1}, {:.1}, {:.1}x{:.1})",
                m.text, m.x, m.y, m.bounds.x, m.bounds.y, m.bounds.width, m.bounds.height
            );
        }
        assert!(
            !results.is_empty(),
            "Should find at least one match for '9'"
        );
        for m in &results {
            assert!(m.x > 0.0, "x coordinate should be positive");
            assert!(m.y > 0.0, "y coordinate should be positive");
        }
    }

    /// Search for "9" using Calculator's window_id.
    /// Requires Calculator to be running.
    /// Run with: cargo test test_ax_find_text_with_window_id -- --ignored --nocapture
    #[test]
    #[ignore]
    fn test_ax_find_text_with_window_id() {
        let windows = crate::macos::window::find_windows_by_app("Calculator")
            .expect("find_windows_by_app should succeed");
        let calc_window = windows
            .first()
            .expect("Calculator must be running for this test");

        println!("Calculator window id: {}", calc_window.id);

        let results =
            find_text("9", Some(calc_window.id)).expect("find_text with window_id should succeed");
        println!(
            "AX find_text results for '9' (window_id={}):",
            calc_window.id
        );
        for m in &results {
            println!(
                "  '{}' at ({:.1}, {:.1}) bounds=({:.1}, {:.1}, {:.1}x{:.1})",
                m.text, m.x, m.y, m.bounds.x, m.bounds.y, m.bounds.width, m.bounds.height
            );
        }
        assert!(
            !results.is_empty(),
            "Should find at least one match for '9' with window_id"
        );
    }

    /// Test element_at_point with Calculator.
    /// Requires Calculator to be running and visible.
    /// Run with: cargo test test_ax_element_at_point_calculator -- --ignored --nocapture
    #[test]
    #[ignore]
    fn test_ax_element_at_point_calculator() {
        // First find Calculator's "5" button to get known coordinates
        let matches = find_text("5", None).expect("find_text should succeed");
        let button = matches
            .iter()
            .find(|m| m.text == "5")
            .expect("Should find the '5' button");

        // Now query element_at_point at those coordinates
        let result = element_at_point(button.x, button.y, Some("Calculator"))
            .expect("element_at_point should succeed");

        println!(
            "element_at_point result: {}",
            serde_json::to_string_pretty(&result).unwrap()
        );

        // Verify we got a meaningful result
        assert!(result.get("role").is_some(), "Should have a role");
        assert!(result.get("bounds").is_some(), "Should have bounds");
        assert!(result.get("pid").is_some(), "Should have a pid");
        assert!(result.get("app_name").is_some(), "Should have an app_name");
    }

    /// Test element_at_point with system-wide lookup (no app_name).
    /// Requires Calculator to be running and in the foreground.
    /// Run with: cargo test test_ax_element_at_point_system_wide -- --ignored --nocapture
    #[test]
    #[ignore]
    fn test_ax_element_at_point_system_wide() {
        let matches = find_text("5", None).expect("find_text should succeed");
        let button = matches
            .iter()
            .find(|m| m.text == "5")
            .expect("Should find the '5' button");

        let result =
            element_at_point(button.x, button.y, None).expect("element_at_point should succeed");

        println!(
            "element_at_point (system-wide): {}",
            serde_json::to_string_pretty(&result).unwrap()
        );

        assert!(result.get("role").is_some(), "Should have a role");
    }

    #[test]
    fn ax_dispatch_error_from_press_code() {
        // kAXErrorSuccess
        assert!(AXDispatchError::from_press_code(0).is_none());
        // kAXErrorActionUnsupported = -25206
        assert!(matches!(
            AXDispatchError::from_press_code(-25206),
            Some(AXDispatchError::NotDispatchable)
        ));
        // Any other non-zero code is a generic AXError.
        match AXDispatchError::from_press_code(-25204) {
            Some(AXDispatchError::AxError(-25204)) => (),
            other => panic!("expected AxError(-25204), got {:?}", other),
        }
    }

    #[test]
    fn ax_dispatch_error_from_set_value_code() {
        // Success
        assert!(AXDispatchError::from_set_value_code(0).is_none());
        // kAXErrorAttributeUnsupported = -25205
        assert!(matches!(
            AXDispatchError::from_set_value_code(-25205),
            Some(AXDispatchError::NotDispatchable)
        ));
        // kAXErrorIllegalArgument = -25204
        assert!(matches!(
            AXDispatchError::from_set_value_code(-25204),
            Some(AXDispatchError::NotDispatchable)
        ));
        // Anything else is generic.
        match AXDispatchError::from_set_value_code(-25212) {
            Some(AXDispatchError::AxError(-25212)) => (),
            other => panic!("expected AxError(-25212), got {:?}", other),
        }
    }

    /// Calculator has well-known bbox attributes on its buttons. Smoke test that
    /// `element_bbox` returns `Some` for the "5" button's AXRef and that the
    /// dimensions look plausible (positive width/height).
    #[test]
    #[ignore]
    fn test_element_bbox_calculator_five_button() {
        // This test runs under `cargo test -- --ignored` and requires
        // Calculator to be running.
        let (nodes, refs) = collect_ax_tree_indexed(Some("Calculator"))
            .expect("collect_ax_tree_indexed should succeed");

        // Find the "5" button node.
        let five = nodes
            .iter()
            .find(|n| n.name.as_deref() == Some("5"))
            .expect("Calculator should expose a '5' button");

        let r = refs
            .get(&five.uid)
            .expect("refs map must contain every uid");
        let bbox =
            unsafe { element_bbox(r.as_raw()) }.expect("5 button should expose position + size");
        assert!(bbox.w > 0.0);
        assert!(bbox.h > 0.0);

        // Node-level bbox should also be populated and match.
        let node_bbox = five
            .bbox
            .expect("node bbox should be populated during walk");
        assert!((node_bbox.x - bbox.x).abs() < 0.5);
        assert!((node_bbox.y - bbox.y).abs() < 0.5);
    }

    /// `AXRef::from_create` must NOT CFRetain (caller already owns a +1); Drop
    /// must CFRelease exactly once. `AXRef::from_get` must CFRetain (caller
    /// holds a borrowed reference); Drop must CFRelease exactly once. Cloning
    /// does not touch CFRetain/CFRelease (the inner Arc does the work).
    ///
    /// We verify by wrapping a `CFData` (heap-allocated — short `CFString`s
    /// can be optimized as tagged pointers with `retain_count = i64::MAX`,
    /// which makes the observable arithmetic meaningless). Retain/release
    /// are defined at the CFType layer so any heap-allocated CF type works.
    #[test]
    fn test_ax_ref_retain_release_balance_against_cfdata() {
        use core_foundation::base::{CFGetRetainCount, CFRetain, CFTypeRef, TCFType};
        use core_foundation::data::CFData;

        let d = CFData::from_buffer(&[1u8, 2, 3, 4, 5, 6, 7, 8]);
        let raw: CFTypeRef = d.as_concrete_TypeRef() as CFTypeRef;
        // Bump to +2 so we can observe `from_create` NOT adding another retain
        // and the final Drop balancing it cleanly.
        unsafe {
            CFRetain(raw);
        }
        let before = unsafe { CFGetRetainCount(raw) };
        assert!(
            (2..isize::MAX).contains(&before),
            "expected a finite retain count >= 2, got {before} — CFData should be heap-allocated"
        );

        // `from_create` transfers the +1 we just added — no extra retain.
        let aref = unsafe { super::AXRef::from_create(raw as *mut _) };
        let during = unsafe { CFGetRetainCount(raw) };
        assert_eq!(
            during, before,
            "from_create must not CFRetain — transfer ownership of existing +1"
        );

        // Clone is Arc-level — no extra CFRetain.
        let clone = aref.clone();
        let during2 = unsafe { CFGetRetainCount(raw) };
        assert_eq!(during2, before, "Arc clone must not touch CFRetain");

        drop(clone);
        let after_clone_drop = unsafe { CFGetRetainCount(raw) };
        assert_eq!(
            after_clone_drop, before,
            "dropping a clone while other Arcs live must not CFRelease"
        );

        drop(aref);
        let after_last_drop = unsafe { CFGetRetainCount(raw) };
        assert_eq!(
            after_last_drop,
            before - 1,
            "final Drop must CFRelease exactly once (count {before} -> {after_last_drop})"
        );

        // And `from_get` must CFRetain on construction.
        let before_get = unsafe { CFGetRetainCount(raw) };
        let aref2 = unsafe { super::AXRef::from_get(raw as *mut _) };
        let during_get = unsafe { CFGetRetainCount(raw) };
        assert_eq!(during_get, before_get + 1, "from_get must CFRetain once");
        drop(aref2);
        let after_get_drop = unsafe { CFGetRetainCount(raw) };
        assert_eq!(
            after_get_drop, before_get,
            "from_get + drop must be net-zero"
        );
    }
}

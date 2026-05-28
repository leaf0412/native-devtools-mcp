//! App / PID resolution + window raising. The non-AX-tree side of the
//! macos::ax surface — PID lookups via NSRunningApplication / NSWorkspace,
//! and the AXFrontmost + AXRaise window-raising routine.
//!
//! `raise_windows`'s AXWindows read routes through `attr::array` (the
//! T4 read seam); the AXFrontmost set + per-window AXRaise stay inline
//! because the seam covers reads only — see the function's doc.

use super::attr;
use super::ffi::{
    AXUIElementCreateApplication, AXUIElementGetPid, AXUIElementRef, AXUIElementSetAttributeValue,
    K_AX_ERROR_SUCCESS,
};
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use objc::runtime::{Class, Object};
use objc::{msg_send, sel, sel_impl};

/// Get the PID that owns a given window ID, using CGWindowListCopyWindowInfo.
pub(super) fn pid_for_window(window_id: u32) -> Result<i32, String> {
    let window = crate::macos::window::find_window_by_id(window_id)?
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
    let windows = crate::macos::window::find_windows_by_app(app_name)
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
///
/// # Safety
/// `element` must be a live, retained `AXUIElementRef`.
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
///
/// ## Read/write seam asymmetry (intentional, T4)
/// The AXWindows *read* routes through `attr::array` (the typed read
/// seam). The AXFrontmost *set* and per-window AXRaise *perform-action*
/// stay inline because the T4 seam covers reads only — writes are
/// handled today by `set_value_attribute` / `press_element` /
/// `select_rows_attribute` on `AXRef`-typed elements, and `raise_windows`
/// operates on raw FFI handles before any `AXRef` exists. A future
/// write seam (mirroring `attr::*` but classifying via `AXDispatchError`)
/// would collapse these into typed setters; YAGNI until a third
/// raw-FFI writer appears.
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
        // Inline write — see fn-doc on read/write asymmetry.
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
        // AXWindows read goes through the attr seam; the per-window AXRaise
        // stays inline (write seam out of scope — see fn-doc).
        let windows_result = attr::array(app_element, "AXWindows");

        let mut raised = frontmost_err == K_AX_ERROR_SUCCESS;

        match &windows_result {
            Ok(Some(windows)) => {
                let raise_action = CFString::new("AXRaise");
                for i in 0..windows.len() {
                    let window = *windows.get_unchecked(i) as AXUIElementRef;
                    let result = super::ffi::AXUIElementPerformAction(
                        window,
                        raise_action.as_concrete_TypeRef(),
                    );
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
            }
            Ok(None) => {
                if debug {
                    eprintln!(
                        "[DEBUG ax::raise_windows] No AXWindows for pid {} (absent)",
                        pid
                    );
                }
            }
            Err(e) => {
                if debug {
                    eprintln!(
                        "[DEBUG ax::raise_windows] AXWindows read failed for pid {} (err={:?})",
                        pid, e
                    );
                }
            }
        }

        core_foundation::base::CFRelease(app_element as core_foundation::base::CFTypeRef);
        raised
    }
}

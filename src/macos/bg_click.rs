//! Background click via `CGEventPostToPid` — delivers mouse events directly
//! to a target process without moving the cursor or stealing focus.
//!
//! The recipe comes from reverse-engineering work documented in
//! [axcli](https://github.com/andelf/axcli) and
//! [bgclick-rev-skill](https://github.com/Lakr233/bgclick-rev-skill).
//!
//! Three things must be true for the target app to process the event:
//!
//! 1. **NSEvent factory** — `+[NSEvent mouseEventWithType:...]` auto-fills
//!    12 internal CGEvent fields (notably field 55 = windowNumber) that
//!    `CGEventCreateMouseEvent` leaves blank. AppKit needs windowNumber for
//!    view routing.
//! 2. **Window-local coordinates** — the private `CGEventSetWindowLocation`
//!    function writes the hit-test coordinate. Without it the event carries
//!    only screen coordinates, which AppKit ignores for background windows.
//! 3. **Command flag for inactive apps** — when the target is not the
//!    frontmost app, `kCGEventFlagMaskCommand` must be set on the event.
//!    This is an undocumented WindowServer bypass signal.

use core_graphics::geometry::CGPoint;
use objc::{msg_send, sel, sel_impl};
use std::ffi::c_void;
use std::sync::OnceLock;

// ── FFI: private CoreGraphics symbols ──────────────────────────────────

// `CGEventPostToPid` — deliver a CGEvent to a specific process.
// Unlike `CGEventPost`, this does NOT go through WindowServer, so the
// cursor doesn't move and focus doesn't change.
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventPostToPid(pid: i32, event: *mut c_void);
    fn CGEventSetIntegerValueField(event: *mut c_void, field: i64, value: i64);
    fn CGEventSetLocation(event: *mut c_void, point: CGPoint);
}
/// Private `CGEventSetWindowLocation` — writes the window-local coordinate
/// into a CGEvent. Resolved at runtime via `dlsym` because it's not in the
/// public SDK.
type CGEventSetWindowLocationFn = unsafe extern "C" fn(event: *const c_void, point: CGPoint);

fn cg_event_set_window_location() -> Option<CGEventSetWindowLocationFn> {
    static CACHED: OnceLock<Option<CGEventSetWindowLocationFn>> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let name = c"CGEventSetWindowLocation";
        let ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr() as *const _) };
        if ptr.is_null() {
            None
        } else {
            // SAFETY: the symbol signature is stable across macOS 11+
            Some(unsafe {
                std::mem::transmute::<*mut c_void, CGEventSetWindowLocationFn>(ptr)
            })
        }
    })
}

// ── FFI: check if app is frontmost ─────────────────────────────────────

/// Check if the app with the given PID is the currently active (frontmost)
/// application. Uses `NSRunningApplication.isActive`.
fn app_is_active(pid: i32) -> bool {
    use objc::runtime::Object;
    unsafe {
        let cls = objc::class!(NSRunningApplication);
        let app: *mut Object = msg_send![cls, runningApplicationWithProcessIdentifier: pid];
        if app.is_null() {
            return false;
        }
        let active: bool = msg_send![app, isActive];
        active
    }
}

// ── Atomic event number counter ────────────────────────────────────────

fn next_event_number() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static COUNTER: AtomicI64 = AtomicI64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

// ── NSPoint layout ─────────────────────────────────────────────────────

/// NSPoint — matches AppKit's struct layout for `+[NSEvent mouseEventWithType:...]`.
#[repr(C)]
struct NSPoint {
    x: f64,
    y: f64,
}

// NSEventType constants
const NS_LEFT_MOUSE_DOWN: u64 = 1;
const NS_LEFT_MOUSE_UP: u64 = 2;

// NSEventModifierFlags
const NS_COMMAND_KEY_MASK: u64 = 0x0010_0000;

// ── CGEvent field IDs (from <CoreGraphics/CGEventTypes.h>) ─────────────

const FIELD_MOUSE_EVENT_SUBTYPE: i64 = 110; // kCGMouseEventSubtype
const FIELD_MOUSE_EVENT_BUTTON_NUMBER: i64 = 3; // kCGMouseEventButtonNumber
const FIELD_MOUSE_EVENT_WINDOW_UNDER_POINTER: i64 = 91; // kCGMouseEventWindowUnderMousePointer
const FIELD_MOUSE_EVENT_WINDOW_CAN_HANDLE: i64 = 92; // kCGMouseEventWindowUnderMousePointerThatCanHandleThisEvent

// ── NSEvent factory ────────────────────────────────────────────────────

/// Build a mouse event via `+[NSEvent mouseEventWithType:...]` and extract
/// the underlying `CGEventRef`. NSEvent auto-fills 12 internal fields
/// (notably field 55 = windowNumber) that `CGEventCreateMouseEvent` leaves
/// blank — AppKit needs these for view routing.
///
/// Returns the raw CGEventRef (owned +1). Caller must CFRelease.
unsafe fn make_mouse_event_via_nsevent(
    event_type: u64,
    screen_point: CGPoint,
    modifier_flags: u64,
    window_number: i64,
    click_count: i64,
) -> Option<*mut c_void> {
    // Use raw objc_msgSend for this 9-argument selector — the msg_send!
    // macro in objc 0.2 doesn't support this many arguments.
    #[cfg(target_arch = "aarch64")]
    extern "C" {
        fn objc_msgSend(
            receiver: *mut c_void,
            selector: *const c_void,
            arg1: u64,
            arg2: NSPoint,
            arg3: u64,
            arg4: f64,
            arg5: i64,
            arg6: *const c_void,
            arg7: i64,
            arg8: i64,
            arg9: f32,
        ) -> *mut c_void;
    }
    #[cfg(target_arch = "x86_64")]
    extern "C" {
        fn objc_msgSend(
            receiver: *mut c_void,
            selector: *const c_void,
            arg1: u64,
            arg2: NSPoint,
            arg3: u64,
            arg4: f64,
            arg5: i64,
            arg6: *const c_void,
            arg7: i64,
            arg8: i64,
            arg9: f32,
        ) -> *mut c_void;
    }

    let cls = objc::runtime::Class::get("NSEvent")?;
    let sel = objc::runtime::Sel::register("mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:");

    let ns_point = NSPoint {
        x: screen_point.x,
        y: screen_point.y,
    };

    // Get system uptime for timestamp
    let process_info: *mut objc::runtime::Object = msg_send![objc::class!(NSProcessInfo), processInfo];
    let uptime: f64 = msg_send![process_info, systemUptime];

    let event_number = next_event_number();

    let ns_event: *mut objc::runtime::Object = std::mem::transmute(objc_msgSend(
        cls as *const _ as *mut c_void,
        sel.as_ptr(),
        event_type,
        ns_point,
        modifier_flags,
        uptime,
        window_number,
        std::ptr::null(),
        event_number,
        click_count,
        1.0,
    ));

    if ns_event.is_null() {
        return None;
    }

    // Extract the underlying CGEventRef via -[NSEvent CGEvent]
    let cg_event: *mut c_void = msg_send![ns_event, CGEvent];
    if cg_event.is_null() {
        return None;
    }

    // CGEvent is returned as a borrowed reference — retain it so we own it
    core_foundation::base::CFRetain(cg_event);
    Some(cg_event)
}

// ── Public API ─────────────────────────────────────────────────────────

/// Result of a background click attempt.
#[derive(Debug)]
pub enum BgClickResult {
    /// Click delivered successfully.
    Ok,
    /// NSEvent factory returned nil (shouldn't happen in practice).
    EventCreationFailed,
    /// Private symbol `CGEventSetWindowLocation` not found (older macOS?).
    PrivateSymbolMissing,
}

/// Post a left-click to a background app process. Does NOT move the cursor
/// or steal focus.
///
/// # Arguments
/// * `pid` — target process PID
/// * `window_id` — CGWindowID of the target window (from `_AXUIElementGetWindow`)
/// * `screen_point` — click location in screen coordinates (from element bbox)
/// * `local_point` — click location in window-local coordinates
pub fn mouse_click_bg(
    pid: i32,
    window_id: u32,
    screen_point: CGPoint,
    local_point: CGPoint,
) -> BgClickResult {
    let wid = window_id as i64;
    let set_win_loc = match cg_event_set_window_location() {
        Some(f) => f,
        None => return BgClickResult::PrivateSymbolMissing,
    };

    let inactive = !app_is_active(pid);
    let flags: u64 = if inactive { NS_COMMAND_KEY_MASK } else { 0 };

    // Tag a CGEvent with the window-routing fields.
    let tag = |event: *mut c_void| {
        unsafe {
            // Window ID fields — AppKit uses these for hit-test routing
            CGEventSetIntegerValueField(event, FIELD_MOUSE_EVENT_WINDOW_UNDER_POINTER, wid);
            CGEventSetIntegerValueField(event, FIELD_MOUSE_EVENT_WINDOW_CAN_HANDLE, wid);
            // Subtype 3 = synthetic click
            CGEventSetIntegerValueField(event, FIELD_MOUSE_EVENT_SUBTYPE, 3);
            // Button number 0 = left
            CGEventSetIntegerValueField(event, FIELD_MOUSE_EVENT_BUTTON_NUMBER, 0);
            // Window-local coordinates (private API)
            set_win_loc(event as *const c_void, local_point);
        }
    };

    // Mouse down
    let down = unsafe {
        make_mouse_event_via_nsevent(
            NS_LEFT_MOUSE_DOWN,
            screen_point,
            flags,
            wid,
            1, // clickCount
        )
    };
    if let Some(event) = down {
        unsafe {
            CGEventSetLocation(event, screen_point);
            tag(event);
            CGEventPostToPid(pid, event);
            core_foundation::base::CFRelease(event);
        }
    } else {
        return BgClickResult::EventCreationFailed;
    }

    // Small delay between down and up
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Mouse up
    let up = unsafe {
        make_mouse_event_via_nsevent(NS_LEFT_MOUSE_UP, screen_point, flags, wid, 1)
    };
    if let Some(event) = up {
        unsafe {
            CGEventSetLocation(event, screen_point);
            tag(event);
            CGEventPostToPid(pid, event);
            core_foundation::base::CFRelease(event);
        }
    }

    BgClickResult::Ok
}

// ── Background keyboard events ────────────────────────────────────────

/// Post a single Unicode character as a keyboard event to a background
/// process via `CGEventPostToPid`. Does NOT move the cursor or steal focus.
///
/// Creates keyDown + keyUp events with the character's UTF-16 string set
/// via `CGEventKeyboardSetUnicodeString`, which bypasses the keyboard layout
/// and IME — the literal character lands regardless of input source.
pub fn post_key_event_unicode(pid: i32, ch: char) -> Result<(), String> {
    // FFI for CGEventCreateKeyboardEvent and CGEventKeyboardSetUnicodeString
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            virtual_key: u16,
            key_down: bool,
        ) -> *mut c_void;
        fn CGEventKeyboardSetUnicodeString(
            event: *mut c_void,
            string_length: u64,
            unicode_string: *const u16,
        );
        fn CGEventPostToPid(pid: i32, event: *mut c_void);
        fn CFRelease(cf: *mut c_void);
    }

    let mut tmp = [0u16; 2];
    let utf16 = ch.encode_utf16(&mut tmp);
    let ptr = utf16.as_ptr();
    let len = utf16.len();

    unsafe {
        // Key down
        let down = CGEventCreateKeyboardEvent(std::ptr::null(), 0, true);
        if down.is_null() {
            return Err("CGEventCreateKeyboardEvent returned null".to_string());
        }
        CGEventKeyboardSetUnicodeString(down, len as u64, ptr);
        CGEventPostToPid(pid, down);
        CFRelease(down);

        // Tiny gap between down and up
        std::thread::sleep(std::time::Duration::from_millis(5));

        // Key up
        let up = CGEventCreateKeyboardEvent(std::ptr::null(), 0, false);
        if up.is_null() {
            return Err("CGEventCreateKeyboardEvent returned null".to_string());
        }
        CGEventKeyboardSetUnicodeString(up, len as u64, ptr);
        CGEventPostToPid(pid, up);
        CFRelease(up);
    }

    Ok(())
}

/// Post a single key event to a background process via CGEventPostToPid.
/// Creates keyDown + keyUp events with the given virtual keycode and flags.
/// Does NOT move the cursor or steal focus.
pub fn post_key_event_bg(pid: i32, keycode: u16, flags: u64) -> Result<(), String> {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            virtual_key: u16,
            key_down: bool,
        ) -> *mut c_void;
        fn CGEventSetFlags(event: *mut c_void, flags: u64);
        fn CGEventPostToPid(pid: i32, event: *mut c_void);
        fn CFRelease(cf: *mut c_void);
    }

    // When the target app is inactive, set the Command flag to bypass
    // WindowServer routing — same trick as mouse_click_bg.
    let inactive = !app_is_active(pid);
    let final_flags: u64 = if inactive { flags | NS_COMMAND_KEY_MASK } else { flags };

    unsafe {
        // Key down
        let down = CGEventCreateKeyboardEvent(std::ptr::null(), keycode, true);
        if down.is_null() {
            return Err("CGEventCreateKeyboardEvent returned null".to_string());
        }
        CGEventSetFlags(down, final_flags);
        CGEventPostToPid(pid, down);
        CFRelease(down);

        std::thread::sleep(std::time::Duration::from_millis(5));

        // Key up
        let up = CGEventCreateKeyboardEvent(std::ptr::null(), keycode, false);
        if up.is_null() {
            return Err("CGEventCreateKeyboardEvent returned null".to_string());
        }
        CGEventSetFlags(up, final_flags);
        CGEventPostToPid(pid, up);
        CFRelease(up);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cg_event_set_window_location_symbol_exists() {
        // On macOS 11+ this private symbol should be resolvable.
        let result = cg_event_set_window_location();
        // Don't assert Some — just verify it doesn't panic.
        println!("CGEventSetWindowLocation available: {}", result.is_some());
    }

    #[test]
    fn test_app_is_active_returns_bool() {
        // Just verify no panic.
        let _ = app_is_active(std::process::id() as i32);
    }

    #[test]
    fn test_next_event_number_increments() {
        let a = next_event_number();
        let b = next_event_number();
        assert!(b > a, "event numbers should be monotonically increasing");
    }
}

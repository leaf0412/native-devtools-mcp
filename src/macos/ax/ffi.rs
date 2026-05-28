//! Raw FFI surface for the macOS Accessibility framework.
//!
//! Pure relocation of the `extern "C"` block, opaque type aliases, and AX
//! status / value-type constants that previously lived in `ax/mod.rs`. No
//! semantics change — call sites import via `super::ffi::*`.

use std::ffi::c_void;

// AXUIElement opaque type
pub(super) type AXUIElementRef = *mut c_void;
// AXValue opaque type
pub(super) type AXValueRef = *mut c_void;

// AXValueType constants
pub(super) const K_AX_VALUE_TYPE_CGPOINT: u32 = 1;
pub(super) const K_AX_VALUE_TYPE_CGSIZE: u32 = 2;

// AX error codes. Values from Apple's
// `<ApplicationServices/HIServices/AXError.h>`.
pub(super) const K_AX_ERROR_SUCCESS: i32 = 0;
pub(super) const K_AX_ERROR_ILLEGAL_ARGUMENT: i32 = -25204;
pub(super) const K_AX_ERROR_ATTRIBUTE_UNSUPPORTED: i32 = -25205;
pub(super) const K_AX_ERROR_ACTION_UNSUPPORTED: i32 = -25206;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub(super) fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    pub(super) fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: core_foundation::string::CFStringRef,
        value: *mut core_foundation::base::CFTypeRef,
    ) -> i32;
    pub(super) fn AXValueGetValue(
        value: AXValueRef,
        value_type: u32,
        value_ptr: *mut c_void,
    ) -> bool;
    pub(super) fn AXUIElementPerformAction(
        element: AXUIElementRef,
        action: core_foundation::string::CFStringRef,
    ) -> i32;
    pub(super) fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: core_foundation::string::CFStringRef,
        value: core_foundation::base::CFTypeRef,
    ) -> i32;
    pub(super) fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    pub(super) fn AXUIElementCopyElementAtPosition(
        application: AXUIElementRef,
        x: f32,
        y: f32,
        element: *mut AXUIElementRef,
    ) -> i32;
    pub(super) fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> i32;
}

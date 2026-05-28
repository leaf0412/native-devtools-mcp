//! AX write-side dispatchers: classified errors + the three blessed
//! writers (`press_element`, `set_value_attribute`, `select_rows_attribute`).
//!
//! `AXDispatchError` lives here, not in `attr::AxError`, because the
//! read and write boundaries have different success/failure shapes:
//! reads can legitimately be "absent" (`Ok(None)`), writes either land
//! or they fail — there is no in-between to surface. See ax::attr for
//! the read seam's `AxError`.

use super::ffi::{
    AXUIElementPerformAction, AXUIElementSetAttributeValue, K_AX_ERROR_ACTION_UNSUPPORTED,
    K_AX_ERROR_ATTRIBUTE_UNSUPPORTED, K_AX_ERROR_ILLEGAL_ARGUMENT, K_AX_ERROR_SUCCESS,
};
use super::AXRef;
use core_foundation::array::{kCFTypeArrayCallBacks, CFArray, CFArrayCreate};
use core_foundation::base::{kCFAllocatorDefault, TCFType};
use core_foundation::string::CFString;
use std::ffi::c_void;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}

//! Typed read seam for `AXUIElementCopyAttributeValue`.
//!
//! `copy_raw` is the only call site in the crate that invokes
//! `AXUIElementCopyAttributeValue`; every public read goes through one of the
//! six typed wrappers (`string` / `boolean` / `point` / `size` / `element` /
//! `array`). This concentrates the unsafe CFRetain/CFRelease accounting in
//! a single auditable function.
//!
//! ## Error model — `Err(AxError)` vs `Ok(None)`
//! - `Ok(Some(v))` — attribute present and decoded as the requested type.
//! - `Ok(None)` — attribute legitimately absent
//!   (`kAXErrorNoValue` / `kAXErrorAttributeUnsupported`,
//!   or `value_ref == nullptr` after a "success" code).
//! - `Err(Ffi)` — unexpected non-success AX status code.
//! - `Err(Decode)` — value present, but downcast to the requested CF type
//!   failed (programmer error: wrong wrapper for the key).
//!
//! Call sites that historically returned `Option<T>` re-conflate Err + None
//! via `.ok().flatten()` — this is the explicit, grep-visible degradation
//! convention used throughout `ax::*`. Never collapse it inside the seam.

use super::ffi::{
    AXUIElementCopyAttributeValue, AXUIElementRef, AXValueGetValue, AXValueRef,
    K_AX_ERROR_ATTRIBUTE_UNSUPPORTED, K_AX_ERROR_SUCCESS, K_AX_VALUE_TYPE_CGPOINT,
    K_AX_VALUE_TYPE_CGSIZE,
};
use super::AXRef;
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;
use core_graphics::geometry::{CGPoint, CGSize};
use std::ffi::c_void;
use std::ptr;

// `kAXErrorNoValue` from `<HIServices/AXError.h>`. Not defined in `ffi.rs`
// because the read seam is its sole consumer.
const K_AX_ERROR_NO_VALUE: i32 = -25212;

/// Failure modes for an AX attribute read.
///
/// Distinct from `super::AXDispatchError`, which classifies the write side
/// (`AXUIElementPerformAction` / `AXUIElementSetAttributeValue`). Reads and
/// writes intentionally use separate error vocabularies — the success/empty
/// boundary is different (`Ok(None)` for "absent" is meaningless on a write).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AxError {
    /// Unexpected non-success AX status code from
    /// `AXUIElementCopyAttributeValue`.
    Ffi(i32),
    /// Value was present but did not downcast to the requested CF type — the
    /// `&'static str` payload is the attribute key for diagnostics.
    Decode(&'static str),
}

/// Sole owner of `AXUIElementCopyAttributeValue` in the crate.
///
/// Returns `Ok(None)` when the attribute is legitimately absent
/// (`kAXErrorNoValue` / `kAXErrorAttributeUnsupported`, or a successful read
/// that produced a null `CFTypeRef`). Returns `Err(AxError::Ffi)` for any
/// other non-success code so the caller can surface unexpected failures
/// instead of silently degrading.
///
/// # Safety
/// `element` must be a live, retained `AXUIElementRef`.
unsafe fn copy_raw(element: AXUIElementRef, key: &'static str) -> Result<Option<CFType>, AxError> {
    let attr = CFString::new(key);
    let mut value_ref: core_foundation::base::CFTypeRef = ptr::null();
    let code = AXUIElementCopyAttributeValue(element, attr.as_concrete_TypeRef(), &mut value_ref);

    match code {
        K_AX_ERROR_SUCCESS => {
            if value_ref.is_null() {
                Ok(None)
            } else {
                // CFType under create rule — Drop will CFRelease when the
                // returned value (or any downcast wrapper) is dropped.
                Ok(Some(CFType::wrap_under_create_rule(value_ref)))
            }
        }
        K_AX_ERROR_NO_VALUE | K_AX_ERROR_ATTRIBUTE_UNSUPPORTED => Ok(None),
        other => Err(AxError::Ffi(other)),
    }
}

/// Read a string attribute (e.g. `AXTitle`, `AXValue`, `AXRole`).
pub(super) fn string(element: AXUIElementRef, key: &'static str) -> Result<Option<String>, AxError> {
    let cf = match unsafe { copy_raw(element, key)? } {
        Some(v) => v,
        None => return Ok(None),
    };
    let s = cf
        .downcast_into::<CFString>()
        .ok_or(AxError::Decode(key))?;
    Ok(Some(s.to_string()))
}

/// Read a boolean attribute (e.g. `AXFocused`, `AXEnabled`).
pub(super) fn boolean(element: AXUIElementRef, key: &'static str) -> Result<Option<bool>, AxError> {
    let cf = match unsafe { copy_raw(element, key)? } {
        Some(v) => v,
        None => return Ok(None),
    };
    let b = cf
        .downcast_into::<CFBoolean>()
        .ok_or(AxError::Decode(key))?;
    Ok(Some(bool::from(b)))
}

/// Read a CGPoint AXValue (e.g. `AXPosition`).
pub(super) fn point(element: AXUIElementRef, key: &'static str) -> Result<Option<CGPoint>, AxError> {
    unsafe { ax_value::<CGPoint>(element, key, K_AX_VALUE_TYPE_CGPOINT) }
}

/// Read a CGSize AXValue (e.g. `AXSize`).
pub(super) fn size(element: AXUIElementRef, key: &'static str) -> Result<Option<CGSize>, AxError> {
    unsafe { ax_value::<CGSize>(element, key, K_AX_VALUE_TYPE_CGSIZE) }
}

/// Shared helper for `AXValue`-typed attributes (CGPoint / CGSize). Not
/// exposed publicly — `point` and `size` are the named entry points so
/// call sites read at the attribute level, not the AXValueType level.
unsafe fn ax_value<T: Default>(
    element: AXUIElementRef,
    key: &'static str,
    ax_value_type: u32,
) -> Result<Option<T>, AxError> {
    let cf = match copy_raw(element, key)? {
        Some(v) => v,
        None => return Ok(None),
    };
    let raw = cf.as_CFTypeRef() as AXValueRef;
    let mut result = T::default();
    let ok = AXValueGetValue(raw, ax_value_type, &mut result as *mut T as *mut c_void);
    if ok {
        Ok(Some(result))
    } else {
        Err(AxError::Decode(key))
    }
}

/// Read an element-typed attribute (e.g. `AXParent`).
///
/// Transfers the `AXUIElementCopyAttributeValue` +1 refcount directly into
/// the returned `AXRef`: `std::mem::forget(cf)` suppresses the `CFType`'s
/// Drop-time `CFRelease`, and `AXRef::from_create` takes ownership of that
/// same +1. The net retain count change is zero.
pub(super) fn element(element: AXUIElementRef, key: &'static str) -> Result<Option<AXRef>, AxError> {
    let cf = match unsafe { copy_raw(element, key)? } {
        Some(v) => v,
        None => return Ok(None),
    };
    let raw = cf.as_CFTypeRef() as AXUIElementRef;
    // +1 ownership transfer: forget the CFType wrapper so its Drop does NOT
    // CFRelease; `AXRef::from_create` adopts the same +1.
    std::mem::forget(cf);
    Ok(Some(unsafe { AXRef::from_create(raw) }))
}

/// Read an array-typed attribute (e.g. `AXChildren`, `AXWindows`).
///
/// Same +1 transfer as `element`: `std::mem::forget(cf)` suppresses the
/// CFType Drop and `CFArray::wrap_under_create_rule` adopts the +1.
pub(super) fn array(
    element: AXUIElementRef,
    key: &'static str,
) -> Result<Option<CFArray<*const c_void>>, AxError> {
    let cf = match unsafe { copy_raw(element, key)? } {
        Some(v) => v,
        None => return Ok(None),
    };
    let raw = cf.as_CFTypeRef();
    // +1 ownership transfer: forget the CFType wrapper so its Drop does NOT
    // CFRelease; `CFArray::wrap_under_create_rule` adopts the same +1.
    std::mem::forget(cf);
    Ok(Some(unsafe { CFArray::wrap_under_create_rule(raw as _) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::data::CFData;
    use core_foundation::string::CFString;

    /// Helper that mirrors the post-`copy_raw` decode branch for `string`,
    /// against a synthesized `CFType` value. We cannot drive `copy_raw`
    /// itself in unit tests (it requires a live AXUIElementRef), but the
    /// decode dispatch is what we actually want to characterize — proves
    /// that a value-present-wrong-type case yields `Err(Decode(key))`,
    /// not a silent `Ok(None)`.
    fn decode_as_string(cf: CFType, key: &'static str) -> Result<Option<String>, AxError> {
        let s = cf
            .downcast_into::<CFString>()
            .ok_or(AxError::Decode(key))?;
        Ok(Some(s.to_string()))
    }

    fn decode_as_boolean(cf: CFType, key: &'static str) -> Result<Option<bool>, AxError> {
        let b = cf
            .downcast_into::<CFBoolean>()
            .ok_or(AxError::Decode(key))?;
        Ok(Some(bool::from(b)))
    }

    #[test]
    fn attr_decode_string_on_cfstring_succeeds() {
        let cf = CFString::new("hello").as_CFType();
        let out = decode_as_string(cf, "AXTitle").expect("decode should succeed");
        assert_eq!(out.as_deref(), Some("hello"));
    }

    #[test]
    fn attr_decode_string_on_cfdata_errors() {
        // CFData under "AXTitle" key is exactly the wrong-type-for-key case
        // copy_raw can produce in the wild — must surface as Err(Decode),
        // not be swallowed as Ok(None).
        let cf = CFData::from_buffer(&[1u8, 2, 3]).as_CFType();
        let out = decode_as_string(cf, "AXTitle");
        assert_eq!(out, Err(AxError::Decode("AXTitle")));
    }

    #[test]
    fn attr_decode_boolean_on_cfboolean_succeeds() {
        let cf = CFBoolean::true_value().as_CFType();
        let out = decode_as_boolean(cf, "AXFocused").expect("decode should succeed");
        assert_eq!(out, Some(true));
    }

    /// Mirror the `attr::element` ownership transfer sequence —
    /// `std::mem::forget(cf); AXRef::from_create(raw)` — against a
    /// heap-allocated CFData (CFString can be tagged-pointer with
    /// `retain_count = i64::MAX`, which makes refcount arithmetic
    /// meaningless). The seam owns this pattern in two places
    /// (`attr::element`, `attr::array`); test the more leak-prone
    /// element variant directly.
    ///
    /// Invariants:
    /// - retain count unchanged across the transfer (the +1 from the
    ///   simulated copy_raw return moves into AXRef without an extra
    ///   CFRetain),
    /// - drops by exactly 1 when the AXRef is dropped (no leak, no
    ///   double-CFRelease).
    #[test]
    fn attr_element_transfers_create_rule_without_leak_or_double_free() {
        use core_foundation::base::{CFGetRetainCount, CFRetain, CFTypeRef};

        let d = CFData::from_buffer(&[42u8; 32]);
        let raw: CFTypeRef = d.as_concrete_TypeRef() as CFTypeRef;

        // Bump to +2 so we can observe the transfer and the AXRef::Drop
        // releasing the +1 we simulate-handed-over.
        unsafe {
            CFRetain(raw);
        }
        let before = unsafe { CFGetRetainCount(raw) };
        assert!(
            (2..isize::MAX).contains(&before),
            "expected a finite retain count >= 2, got {before} — CFData should be heap-allocated"
        );

        // Stage what `attr::element` does after copy_raw returns a CFType:
        // wrap the raw under create rule (same as copy_raw), then transfer
        // via mem::forget into AXRef::from_create.
        let cf = unsafe { CFType::wrap_under_create_rule(raw) };
        let raw_for_axref = cf.as_CFTypeRef() as super::super::ffi::AXUIElementRef;
        std::mem::forget(cf);
        let aref = unsafe { super::super::AXRef::from_create(raw_for_axref) };

        let during = unsafe { CFGetRetainCount(raw) };
        assert_eq!(
            during, before,
            "attr::element transfer must not CFRetain — the copy_raw +1 \
             moves into AXRef without adding a second retain"
        );

        drop(aref);
        let after = unsafe { CFGetRetainCount(raw) };
        assert_eq!(
            after,
            before - 1,
            "AXRef::Drop after attr::element transfer must CFRelease exactly \
             once (count {before} -> {after})"
        );
    }
}

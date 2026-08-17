//! Structured input-failure diagnostics shared by the input tools.
//!
//! The problem this solves: every input tool used to gate on a single
//! `AXIsProcessTrustedWithOptions` probe and, when it returned `false`,
//! report a blanket "Accessibility permission required" message. That
//! conflated several genuinely different failures:
//!
//! * the host process genuinely lacks TCC trust,
//! * the target app / window does not exist,
//! * the target process is not reachable,
//! * the background-dispatch machinery is unsupported on this macOS,
//! * the NSEvent factory failed to produce an event,
//! * the event was posted but its effect was never verified.
//!
//! Every failure now carries a stable `code`, the `stage` at which it
//! happened, and (where a probe already ran) a `permission` snapshot, so a
//! caller can tell "no permission" from "permission is fine but the target
//! window is gone" from "event posted but unverified".

use rmcp::model::{CallToolResult, Content};
use serde_json::json;

/// The pipeline stage at which an input failure occurred.
///
/// This is a forward-looking vocabulary: not every stage has a producing
/// code yet (e.g. `Verification` is produced by the upcoming post-dispatch
/// verification slice). The enum is intentionally complete so callers can
/// match on the full taxonomy without churn.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputStage {
    /// Checking whether the host process is trusted.
    PermissionProbe,
    /// Resolving the target app / window / process.
    TargetResolution,
    /// Resolving an element or coordinates inside the target.
    ElementResolution,
    /// Checking whether the requested dispatch mechanism is even available.
    EventCapability,
    /// Creating the platform event object.
    EventCreation,
    /// Posting the event to the target.
    Dispatch,
    /// Verifying that the intended UI change actually happened.
    Verification,
}

impl InputStage {
    pub fn as_str(self) -> &'static str {
        match self {
            InputStage::PermissionProbe => "permission_probe",
            InputStage::TargetResolution => "target_resolution",
            InputStage::ElementResolution => "element_resolution",
            InputStage::EventCapability => "event_capability",
            InputStage::EventCreation => "event_creation",
            InputStage::Dispatch => "dispatch",
            InputStage::Verification => "verification",
        }
    }
}

/// A stable machine-readable input error code.
///
/// As above: a shared taxonomy where some codes are produced today and a
/// few (`TargetProcessUnavailable`, `BackgroundEventDispatchedUnverified`)
/// are reserved for later stages of the diagnostics work.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputErrorCode {
    /// The host process is not trusted for Accessibility input.
    AccessibilityUntrusted,
    /// The named app could not be found / is not running.
    TargetAppNotFound,
    /// The app is running but has no visible window.
    TargetWindowNotFound,
    /// The target process is not reachable for dispatch.
    TargetProcessUnavailable,
    /// The requested coordinates are invalid for the target.
    InvalidTargetCoordinates,
    /// Background dispatch is unsupported on this platform / macOS version.
    BackgroundDispatchUnsupported,
    /// The platform event factory failed to create an event.
    BackgroundEventCreationFailed,
    /// The event was posted but its effect was not verified.
    BackgroundEventDispatchedUnverified,
}

impl InputErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            InputErrorCode::AccessibilityUntrusted => "accessibility_untrusted",
            InputErrorCode::TargetAppNotFound => "target_app_not_found",
            InputErrorCode::TargetWindowNotFound => "target_window_not_found",
            InputErrorCode::TargetProcessUnavailable => "target_process_unavailable",
            InputErrorCode::InvalidTargetCoordinates => "invalid_target_coordinates",
            InputErrorCode::BackgroundDispatchUnsupported => "background_dispatch_unsupported",
            InputErrorCode::BackgroundEventCreationFailed => "background_event_creation_failed",
            InputErrorCode::BackgroundEventDispatchedUnverified => {
                "background_event_dispatched_unverified"
            }
        }
    }

    pub fn stage(self) -> InputStage {
        match self {
            InputErrorCode::AccessibilityUntrusted => InputStage::PermissionProbe,
            InputErrorCode::TargetAppNotFound
            | InputErrorCode::TargetWindowNotFound
            | InputErrorCode::TargetProcessUnavailable => InputStage::TargetResolution,
            InputErrorCode::InvalidTargetCoordinates => InputStage::ElementResolution,
            InputErrorCode::BackgroundDispatchUnsupported => InputStage::EventCapability,
            InputErrorCode::BackgroundEventCreationFailed => InputStage::EventCreation,
            InputErrorCode::BackgroundEventDispatchedUnverified => InputStage::Verification,
        }
    }

    pub fn retryable(self) -> bool {
        match self {
            InputErrorCode::AccessibilityUntrusted
            | InputErrorCode::TargetAppNotFound
            | InputErrorCode::TargetWindowNotFound
            | InputErrorCode::TargetProcessUnavailable
            | InputErrorCode::BackgroundEventCreationFailed => true,
            InputErrorCode::InvalidTargetCoordinates
            | InputErrorCode::BackgroundDispatchUnsupported
            | InputErrorCode::BackgroundEventDispatchedUnverified => false,
        }
    }

    pub fn recommended_action(self) -> &'static str {
        match self {
            InputErrorCode::AccessibilityUntrusted => {
                "grant Accessibility to the host app in System Settings, then restart it"
            }
            InputErrorCode::TargetAppNotFound => "launch the app or correct the app name",
            InputErrorCode::TargetWindowNotFound => {
                "bring the app window to the foreground or open a window"
            }
            InputErrorCode::TargetProcessUnavailable => "check the target process is still running",
            InputErrorCode::InvalidTargetCoordinates => {
                "re-derive coordinates from a fresh AX snapshot or screenshot"
            }
            InputErrorCode::BackgroundDispatchUnsupported => {
                "use ax_click/ax_set_value or fall back to a plain (foreground) click"
            }
            InputErrorCode::BackgroundEventCreationFailed => {
                "retry once, then fall back to a plain (foreground) click"
            }
            InputErrorCode::BackgroundEventDispatchedUnverified => {
                "re-observe the UI (AX snapshot or screenshot) to confirm the effect"
            }
        }
    }
}

/// A point-in-time snapshot of the host process's permissions, included in
/// errors so callers can distinguish "no permission" from "permission fine
/// but the operation failed elsewhere".
#[derive(Debug, Clone, serde::Serialize)]
pub struct PermissionStatus {
    /// `AXIsProcessTrustedWithOptions` result for the MCP server process.
    pub accessibility_trusted: bool,
}

fn error_body(
    code: InputErrorCode,
    message: impl Into<String>,
    permission: Option<&PermissionStatus>,
) -> serde_json::Value {
    let mut error = json!({
        "code": code.as_str(),
        "stage": code.stage().as_str(),
        "message": message.into(),
        "retryable": code.retryable(),
        "recommended_action": code.recommended_action(),
    });
    if let Some(p) = permission {
        error["permission"] = json!({
            "accessibility_trusted": p.accessibility_trusted,
        });
    }
    json!({ "error": error })
}

/// Build a structured `CallToolResult::error`. The body is a JSON object so
/// callers can branch on `code`; `message` remains human-readable.
pub fn error(
    code: InputErrorCode,
    message: impl Into<String>,
    permission: Option<&PermissionStatus>,
) -> CallToolResult {
    CallToolResult::error(vec![Content::text(
        error_body(code, message, permission).to_string(),
    )])
}

/// Build a structured success for a background event that was *posted* but
/// whose effect was not verified. This is deliberately not a plain
/// "clicked successfully" message: `CGEventPostToPid` gives no synchronous
/// confirmation that the target app actually processed the event.
pub fn dispatched_unverified(kind: &str, target: &str, detail: &str) -> CallToolResult {
    let body = json!({
        "ok": true,
        "status": "dispatched_unverified",
        "dispatch": "CGEventPostToPid",
        "kind": kind,
        "target": target,
        "detail": detail,
        "note": "The event was posted to the target process, but no UI state change was verified. Re-observe the UI (AX snapshot or screenshot) to confirm the effect."
    });
    CallToolResult::success(vec![Content::text(body.to_string())])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_text(r: &CallToolResult) -> String {
        r.content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn accessibility_untrusted_maps_to_permission_stage() {
        assert_eq!(
            InputErrorCode::AccessibilityUntrusted.as_str(),
            "accessibility_untrusted"
        );
        assert_eq!(
            InputErrorCode::AccessibilityUntrusted.stage(),
            InputStage::PermissionProbe
        );
        assert!(InputErrorCode::AccessibilityUntrusted.retryable());
    }

    #[test]
    fn target_window_not_found_maps_to_target_resolution() {
        assert_eq!(
            InputErrorCode::TargetWindowNotFound.as_str(),
            "target_window_not_found"
        );
        assert_eq!(
            InputErrorCode::TargetWindowNotFound.stage(),
            InputStage::TargetResolution
        );
    }

    #[test]
    fn background_dispatch_unsupported_is_not_retryable() {
        assert_eq!(
            InputErrorCode::BackgroundDispatchUnsupported.as_str(),
            "background_dispatch_unsupported"
        );
        assert!(!InputErrorCode::BackgroundDispatchUnsupported.retryable());
    }

    #[test]
    fn error_serializes_code_stage_message_and_permission() {
        let perm = PermissionStatus {
            accessibility_trusted: false,
        };
        let r = error(
            InputErrorCode::AccessibilityUntrusted,
            "host process is not trusted",
            Some(&perm),
        );
        assert_eq!(r.is_error, Some(true));
        let body: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(body["error"]["code"], "accessibility_untrusted");
        assert_eq!(body["error"]["stage"], "permission_probe");
        assert_eq!(body["error"]["retryable"], true);
        assert_eq!(body["error"]["permission"]["accessibility_trusted"], false);
    }

    #[test]
    fn error_omits_permission_when_absent() {
        let r = error(InputErrorCode::TargetWindowNotFound, "no window", None);
        let body: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(body["error"]["code"], "target_window_not_found");
        assert!(body["error"].get("permission").is_none());
    }

    #[test]
    fn dispatched_unverified_marks_status_not_success_claim() {
        let r = dispatched_unverified("click", "飞书", "posted mouse down/up");
        assert_eq!(r.is_error, Some(false));
        let body: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["status"], "dispatched_unverified");
        assert_eq!(body["dispatch"], "CGEventPostToPid");
    }
}

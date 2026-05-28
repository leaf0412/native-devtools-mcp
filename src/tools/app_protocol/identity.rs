/// Identity validation result
#[derive(Debug, PartialEq)]
pub enum IdentityValidationResult {
    /// Validation passed
    Ok,
    /// Bundle ID mismatch
    BundleIdMismatch {
        expected: String,
        actual: String,
        actual_app_name: String,
    },
    /// App name mismatch
    AppNameMismatch {
        expected: String,
        actual: String,
        actual_bundle_id: String,
    },
}

/// Validates bundle ID (exact, case-sensitive match)
pub fn validate_bundle_id(expected: &str, actual: &str) -> bool {
    expected == actual
}

/// Validates app name (case-insensitive, whitespace-trimmed)
pub fn validate_app_name(expected: &str, actual: &str) -> bool {
    expected.trim().eq_ignore_ascii_case(actual.trim())
}

/// Validates app identity against expected values from runtime info
pub fn validate_identity(
    expected_bundle_id: Option<&str>,
    expected_app_name: Option<&str>,
    info: &serde_json::Value,
) -> IdentityValidationResult {
    let actual_bundle_id = info.get("bundleId").and_then(|v| v.as_str()).unwrap_or("");
    let actual_app_name = info
        .get("appName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    // Validate bundle ID if expected
    if let Some(expected) = expected_bundle_id {
        if !validate_bundle_id(expected, actual_bundle_id) {
            return IdentityValidationResult::BundleIdMismatch {
                expected: expected.to_string(),
                actual: actual_bundle_id.to_string(),
                actual_app_name: actual_app_name.to_string(),
            };
        }
    }

    // Validate app name if expected
    if let Some(expected) = expected_app_name {
        if !validate_app_name(expected, actual_app_name) {
            return IdentityValidationResult::AppNameMismatch {
                expected: expected.to_string(),
                actual: actual_app_name.to_string(),
                actual_bundle_id: actual_bundle_id.to_string(),
            };
        }
    }

    IdentityValidationResult::Ok
}

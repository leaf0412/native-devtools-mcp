//! Query tools: get_displays, find_text, element_at_point.

use crate::platform::{display, ocr};
use crate::tools::registry::{json_to_object, ToolContext, ToolHandler};
use rmcp::model::{CallToolResult, Content};
use rmcp::{model::Tool, Error as McpError};
use serde::Deserialize;
use std::sync::Arc;

// ============================================================================
// Get Displays
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct GetDisplaysParams {}

pub fn get_displays(_params: GetDisplaysParams) -> CallToolResult {
    match display::get_displays() {
        Ok(displays) => match serde_json::to_string_pretty(&displays) {
            Ok(json) => CallToolResult::success(vec![Content::text(json)]),
            Err(e) => CallToolResult::error(vec![Content::text(format!(
                "Failed to serialize displays: {}",
                e
            ))]),
        },
        Err(e) => CallToolResult::error(vec![Content::text(format!(
            "Failed to get displays: {}",
            e
        ))]),
    }
}

// ============================================================================
// Find Text (Accessibility + OCR)
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct FindTextParams {
    pub text: String,
    /// Optional display ID to search on. If omitted, searches the main display.
    /// Ignored when window_id or app_name is provided.
    pub display_id: Option<u32>,
    /// Window ID to scope the search to a specific window.
    pub window_id: Option<u32>,
    /// Application name to scope the search to a specific app's window.
    pub app_name: Option<String>,
    /// Enable language correction (helps with word accuracy but hurts single-character
    /// detection). Defaults to false, which is better for UI automation.
    #[serde(default)]
    pub uses_language_correction: bool,
}

pub fn find_text(params: FindTextParams) -> CallToolResult {
    let debug = std::env::var("NATIVE_DEVTOOLS_DEBUG").is_ok();

    // Resolve window_id from app_name if provided
    let window_id = match (params.window_id, &params.app_name) {
        (Some(id), _) => Some(id),
        (None, Some(app_name)) => match crate::platform::find_windows_by_app(app_name) {
            Ok(windows) if !windows.is_empty() => Some(windows[0].id),
            Ok(_) => {
                return CallToolResult::error(vec![Content::text(format!(
                    "No window found for app '{}'",
                    app_name
                ))]);
            }
            Err(e) => {
                return CallToolResult::error(vec![Content::text(format!(
                    "Failed to find window: {}",
                    e
                ))]);
            }
        },
        (None, None) => None,
    };

    // Primary: try accessibility tree search
    match find_text_accessibility(&params.text, window_id) {
        Ok(mut matches) if !matches.is_empty() => {
            rank_matches(&mut matches, &params.text);
            return serialize_matches(&matches);
        }
        Ok(_) if debug => {
            eprintln!(
                "[DEBUG find_text] no accessibility matches for '{}', trying OCR",
                params.text
            );
        }
        Err(e) if debug => {
            eprintln!(
                "[DEBUG find_text] accessibility failed for '{}': {}, trying OCR",
                params.text, e
            );
        }
        _ => {}
    }

    // Fallback: OCR
    let ocr_result = if let Some(wid) = window_id {
        find_text_in_window(&params.text, wid, params.uses_language_correction)
    } else {
        #[cfg(target_os = "macos")]
        {
            ocr::find_text(
                &params.text,
                params.display_id,
                params.uses_language_correction,
            )
        }
        #[cfg(target_os = "windows")]
        {
            ocr::find_text(&params.text, params.display_id)
        }
    };

    match ocr_result {
        Ok(ref matches) if !matches.is_empty() => serialize_matches(matches),
        Ok(_) => empty_result_with_available_elements(&params.text, window_id, debug),
        Err(e) => CallToolResult::error(vec![Content::text(e)]),
    }
}

// ============================================================================
// Element At Point
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct ElementAtPointParams {
    pub x: f64,
    pub y: f64,
    pub app_name: Option<String>,
}

pub fn element_at_point(params: ElementAtPointParams) -> CallToolResult {
    let result = element_at_point_platform(params.x, params.y, params.app_name.as_deref());
    match result {
        Ok(value) => match serde_json::to_string_pretty(&value) {
            Ok(json) => CallToolResult::success(vec![Content::text(json)]),
            Err(e) => {
                CallToolResult::error(vec![Content::text(format!("Failed to serialize: {}", e))])
            }
        },
        Err(e) => CallToolResult::error(vec![Content::text(e)]),
    }
}

fn element_at_point_platform(
    x: f64,
    y: f64,
    app_name: Option<&str>,
) -> Result<serde_json::Value, String> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::ax::element_at_point(x, y, app_name)
    }
    #[cfg(target_os = "windows")]
    {
        crate::windows::uia::element_at_point(x, y, app_name)
    }
}

/// Try to find text using the platform accessibility API.
fn find_text_accessibility(
    search: &str,
    window_id: Option<u32>,
) -> Result<Vec<ocr::TextMatch>, String> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::ax::find_text(search, window_id)
    }
    #[cfg(target_os = "windows")]
    {
        // TODO: support targeting specific window_id via ElementFromHandle(hwnd)
        let _ = window_id;
        crate::windows::uia::find_text(search)
    }
}

/// Collect all visible element names using the platform accessibility API.
fn list_element_names_accessibility(window_id: Option<u32>) -> Result<Vec<String>, String> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::ax::list_element_names(window_id)
    }
    #[cfg(target_os = "windows")]
    {
        let _ = window_id;
        crate::windows::uia::list_element_names()
    }
}

const MAX_HINT_ELEMENTS: usize = 200;

/// Build a "no matches found" hint JSON string with available element names.
/// Shared between desktop `find_text` and `android_find_text`.
///
/// `available_elements` are real accessibility nodes, but for accessibility-opaque
/// apps (custom-drawn UIs that expose only the menu bar to the AX tree) they do
/// NOT represent the on-screen content. We surface that uncertainty explicitly so
/// the caller doesn't mistake the menu items for "everything that's available",
/// and point them at the OCR/coordinate path that can actually see custom content.
pub fn build_no_matches_hint(search: &str, available_elements: &[String]) -> String {
    let capped: Vec<&str> = available_elements
        .iter()
        .take(MAX_HINT_ELEMENTS)
        .map(|s| s.as_str())
        .collect();
    let hint = serde_json::json!({
        "message": format!(
            "No matches found for \"{}\" in the accessibility tree or via OCR. \
             The app may render content outside the accessibility tree (custom-drawn UI), \
             so `available_elements` below may only list the menu bar and miss the real content. \
             Retry with take_screenshot(include_ocr=true) and click by the returned coordinates.",
            search
        ),
        "available_elements": capped,
    });
    hint.to_string()
}

/// Build an empty result with a list of available UI elements as a hint.
fn empty_result_with_available_elements(
    search: &str,
    window_id: Option<u32>,
    debug: bool,
) -> CallToolResult {
    let mut content = vec![Content::text("[]")];

    match list_element_names_accessibility(window_id) {
        Ok(names) => {
            if debug && !names.is_empty() {
                eprintln!(
                    "[DEBUG find_text] listing {} available element names as hint",
                    names.len()
                );
            }
            content.push(Content::text(build_no_matches_hint(search, &names)));
        }
        Err(e) if debug => {
            eprintln!("[DEBUG find_text] failed to list element names: {}", e);
        }
        _ => {}
    }

    CallToolResult::success(content)
}

/// Serialize text matches to a JSON CallToolResult.
/// Rank find_text results so that exact matches and interactive elements appear first.
///
/// Ranking priority (lower score = higher rank):
///   0 — exact match + interactive element
///   1 — exact match + non-interactive element
///   2 — substring match + interactive element
///   3 — substring match + non-interactive element
///
/// Within the same rank, original tree-traversal order is preserved (stable sort).
fn rank_matches(matches: &mut [ocr::TextMatch], search: &str) {
    let search_lower = search.to_lowercase();
    matches.sort_by_key(|m| {
        let is_exact = m.text.to_lowercase() == search_lower;
        let is_interactive = m.role.as_deref().is_some_and(is_interactive_role);
        match (is_exact, is_interactive) {
            (true, true) => 0u8,
            (true, false) => 1,
            (false, true) => 2,
            (false, false) => 3,
        }
    });
}

/// Check whether an accessibility role represents an interactive element.
///
/// Covers both macOS AXRole names (e.g. "AXButton") and Windows UIA control
/// type names (e.g. "Button").
fn is_interactive_role(role: &str) -> bool {
    matches!(
        role,
        // macOS AXRoles
        "AXButton"
        | "AXTextField"
        | "AXTextArea"
        | "AXLink"
        | "AXCheckBox"
        | "AXRadioButton"
        | "AXPopUpButton"
        | "AXMenuButton"
        | "AXSlider"
        | "AXIncrementor"
        | "AXComboBox"
        | "AXMenuItem"
        | "AXTabGroup"
        | "AXTab"
        // Windows UIA control types
        | "Button"
        | "Edit"
        | "Hyperlink"
        | "CheckBox"
        | "RadioButton"
        | "ComboBox"
        | "Slider"
        | "Spinner"
        | "MenuItem"
        | "TabItem"
        | "ListItem"
        | "TreeItem"
        | "DataItem"
        | "SplitButton"
    )
}

fn serialize_matches(matches: &[ocr::TextMatch]) -> CallToolResult {
    match serde_json::to_string_pretty(matches) {
        Ok(json) => CallToolResult::success(vec![Content::text(json)]),
        Err(e) => CallToolResult::error(vec![Content::text(format!("Failed to serialize: {}", e))]),
    }
}

/// Run OCR scoped to a single window and return matching text with screen coordinates.
fn find_text_in_window(
    search: &str,
    window_id: u32,
    uses_language_correction: bool,
) -> Result<Vec<ocr::TextMatch>, String> {
    let screenshot = crate::platform::capture_window(window_id)
        .map_err(|e| format!("Failed to capture window: {}", e))?;

    #[cfg(target_os = "macos")]
    let mut matches = ocr::ocr_image(
        &screenshot.png_data,
        Some(screenshot.scale_factor),
        uses_language_correction,
    )?;
    #[cfg(target_os = "windows")]
    let mut matches = {
        let _ = uses_language_correction; // Windows OCR doesn't support this param
        ocr::ocr_image(&screenshot.png_data, Some(screenshot.scale_factor))?
    };

    // Offset OCR coordinates from image-relative to screen-absolute
    for m in &mut matches {
        m.x += screenshot.origin_x;
        m.y += screenshot.origin_y;
        m.bounds.x += screenshot.origin_x;
        m.bounds.y += screenshot.origin_y;
    }

    // Filter by search term
    let search_lower = search.to_lowercase();
    matches.retain(|m| m.text.to_lowercase().contains(&search_lower));
    matches.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(matches)
}

/// `get_displays` MCP tool handler.
pub struct GetDisplays;

#[async_trait::async_trait]
impl ToolHandler for GetDisplays {
    fn name(&self) -> &'static str {
        "get_displays"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "get_displays",
            "Get information about all connected displays including bounds, scale factors, and resolution.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    async fn call(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<CallToolResult, McpError> {
        let params: GetDisplaysParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(get_displays(params))
    }
}

/// `find_text` MCP tool handler.
pub struct FindText;

#[async_trait::async_trait]
impl ToolHandler for FindText {
    fn name(&self) -> &'static str {
        "find_text"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "find_text",
            "PREFERRED for clicking buttons/labels by name. Finds text on screen using the platform accessibility API (macOS Accessibility, Windows UI Automation) with OCR fallback, and returns screen coordinates ready for the click tool. Use this instead of visually estimating coordinates from screenshots. Can be scoped to a specific app window for faster, more precise results. Note: accessibility results use semantic element names (e.g., 'All Clear' instead of 'AC', 'Subtract' instead of '\u{2212}'), so search by meaning rather than displayed symbol. When no matches are found, the response includes an available_elements array listing all UI element names in the target window — use this to find the correct name and retry. Requires macOS 10.15+ or Windows 10 1903+.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Text to search for (case-insensitive substring match). Matches against accessibility element names first (e.g., 'All Clear', 'Subtract'), then falls back to OCR on visible text."
                    },
                    "app_name": {
                        "type": "string",
                        "description": "Application name to scope the search to a specific app's window (e.g., 'Calculator'). Faster and avoids false matches from other windows."
                    },
                    "window_id": {
                        "type": "integer",
                        "description": "Window ID to scope the search to a specific window"
                    },
                    "display_id": {
                        "type": "integer",
                        "description": "Display ID to search on. Use get_displays to list available displays. If omitted, searches the main display. Ignored when window_id or app_name is provided."
                    },
                    "uses_language_correction": {
                        "type": "boolean",
                        "description": "Enable language correction for better word accuracy in OCR fallback. Default is false, which is better for UI automation (buttons, labels, single characters). Has no effect when results come from the accessibility API."
                    }
                }
            }))),
        )
    }

    async fn call(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<CallToolResult, McpError> {
        let params: FindTextParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(find_text(params))
    }
}

/// `element_at_point` MCP tool handler.
pub struct ElementAtPoint;

#[async_trait::async_trait]
impl ToolHandler for ElementAtPoint {
    fn name(&self) -> &'static str {
        "element_at_point"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "element_at_point",
            "Given screen coordinates (x, y), return the accessibility element at that point (name, role, label, value, bounds, pid, app_name). Optional app_name param to scope the lookup to a specific application for faster, more precise results.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "required": ["x", "y"],
                "properties": {
                    "x": {
                        "type": "number",
                        "description": "Screen X coordinate"
                    },
                    "y": {
                        "type": "number",
                        "description": "Screen Y coordinate"
                    },
                    "app_name": {
                        "type": "string",
                        "description": "Application name to scope the lookup to a specific app (e.g., 'Calculator'). Faster and avoids ambiguity at window edges."
                    }
                }
            }))),
        )
    }

    async fn call(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<CallToolResult, McpError> {
        let params: ElementAtPointParams = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(element_at_point(params))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::ocr::{TextBounds, TextMatch};

    fn make_match(text: &str, role: Option<&str>) -> TextMatch {
        TextMatch {
            text: text.to_string(),
            x: 0.0,
            y: 0.0,
            confidence: 1.0,
            bounds: TextBounds {
                x: 0.0,
                y: 0.0,
                width: 50.0,
                height: 20.0,
            },
            role: role.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_no_matches_hint_guides_to_ocr_for_opaque_apps() {
        // For accessibility-opaque apps the listed elements (e.g. menu bar) are
        // misleading on their own; the hint must steer the caller to the OCR +
        // coordinate path instead of treating the menu items as the full UI.
        let hint = build_no_matches_hint("发送", &["File".to_string(), "Edit".to_string()]);
        let parsed: serde_json::Value =
            serde_json::from_str(&hint).expect("hint should be valid JSON");
        let message = parsed["message"].as_str().expect("message should be a string");

        assert!(message.contains("发送"), "message should echo the search term");
        assert!(
            message.contains("take_screenshot(include_ocr=true)"),
            "message must point to the OCR screenshot path: {message}"
        );
        assert!(
            message.contains("coordinates"),
            "message must mention clicking by coordinates: {message}"
        );
        assert!(
            message.contains("outside the accessibility tree"),
            "message must explain custom-drawn content is invisible to AX: {message}"
        );
        // Real elements are still preserved, just no longer presented as authoritative.
        assert_eq!(parsed["available_elements"], serde_json::json!(["File", "Edit"]));
    }

    #[test]
    fn test_rank_exact_match_before_substring() {
        let mut matches = vec![
            make_match("2×3", None), // substring match, no role
            make_match("2", None),   // exact match, no role
        ];
        rank_matches(&mut matches, "2");
        assert_eq!(matches[0].text, "2");
        assert_eq!(matches[1].text, "2×3");
    }

    #[test]
    fn test_rank_interactive_before_static() {
        let mut matches = vec![
            make_match("Submit", Some("AXStaticText")), // static
            make_match("Submit", Some("AXButton")),     // interactive
        ];
        rank_matches(&mut matches, "Submit");
        assert_eq!(matches[0].role.as_deref(), Some("AXButton"));
        assert_eq!(matches[1].role.as_deref(), Some("AXStaticText"));
    }

    #[test]
    fn test_rank_exact_interactive_is_top() {
        let mut matches = vec![
            make_match("2×3", Some("AXButton")),   // substring + interactive
            make_match("2", Some("AXStaticText")), // exact + static
            make_match("2×3", Some("AXStaticText")), // substring + static
            make_match("2", Some("AXButton")),     // exact + interactive
        ];
        rank_matches(&mut matches, "2");
        assert_eq!(matches[0].text, "2");
        assert_eq!(matches[0].role.as_deref(), Some("AXButton"));
        assert_eq!(matches[1].text, "2");
        assert_eq!(matches[1].role.as_deref(), Some("AXStaticText"));
        assert_eq!(matches[2].text, "2×3");
        assert_eq!(matches[2].role.as_deref(), Some("AXButton"));
        assert_eq!(matches[3].text, "2×3");
        assert_eq!(matches[3].role.as_deref(), Some("AXStaticText"));
    }

    #[test]
    fn test_rank_preserves_order_within_same_rank() {
        let mut matches = vec![
            make_match("Open", Some("AXButton")),
            make_match("Open", Some("AXMenuItem")),
        ];
        rank_matches(&mut matches, "Open");
        // Both are exact + interactive, original order preserved (stable sort)
        assert_eq!(matches[0].role.as_deref(), Some("AXButton"));
        assert_eq!(matches[1].role.as_deref(), Some("AXMenuItem"));
    }

    #[test]
    fn test_rank_case_insensitive_exact_match() {
        let mut matches = vec![
            make_match("SUBMIT button", Some("AXStaticText")),
            make_match("submit", Some("AXButton")),
        ];
        rank_matches(&mut matches, "Submit");
        assert_eq!(matches[0].text, "submit");
        assert_eq!(matches[1].text, "SUBMIT button");
    }

    #[test]
    fn test_rank_no_role_treated_as_non_interactive() {
        let mut matches = vec![
            make_match("OK", None),             // exact, no role (OCR)
            make_match("OK", Some("AXButton")), // exact, interactive
        ];
        rank_matches(&mut matches, "OK");
        assert_eq!(matches[0].role.as_deref(), Some("AXButton"));
        assert_eq!(matches[1].role, None);
    }

    #[test]
    fn test_is_interactive_role_macos() {
        assert!(is_interactive_role("AXButton"));
        assert!(is_interactive_role("AXTextField"));
        assert!(is_interactive_role("AXLink"));
        assert!(is_interactive_role("AXCheckBox"));
        assert!(is_interactive_role("AXMenuItem"));
        assert!(!is_interactive_role("AXStaticText"));
        assert!(!is_interactive_role("AXGroup"));
        assert!(!is_interactive_role("AXImage"));
        assert!(!is_interactive_role("AXScrollArea"));
    }

    #[test]
    fn test_is_interactive_role_windows() {
        assert!(is_interactive_role("Button"));
        assert!(is_interactive_role("Edit"));
        assert!(is_interactive_role("Hyperlink"));
        assert!(is_interactive_role("CheckBox"));
        assert!(is_interactive_role("MenuItem"));
        assert!(!is_interactive_role("Text"));
        assert!(!is_interactive_role("Group"));
        assert!(!is_interactive_role("Image"));
        assert!(!is_interactive_role("Pane"));
    }
}

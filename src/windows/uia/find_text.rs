use super::super::ocr::{TextBounds, TextMatch};
use super::tree::{element_text_properties, uia_control_type_name};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, TreeScope, TreeScope_Descendants, TreeScope_Element,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

pub(super) const MAX_DEPTH: u32 = 50;
pub(super) const MAX_ELEMENTS: usize = 10_000;

/// Enumerate all UIA elements in the foreground window, calling `visitor` on each
/// element's name (non-empty names only). Returns early with an empty result if
/// no foreground window is available.
fn for_each_element_name(mut visitor: impl FnMut(&str)) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)
            .map_err(|e| format!("Failed to create IUIAutomation: {}", e))?;

        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return Ok(());
        }

        let root = automation
            .ElementFromHandle(hwnd)
            .map_err(|e| format!("Failed to get element from foreground window: {}", e))?;

        let condition = automation
            .CreateTrueCondition()
            .map_err(|e| format!("Failed to create condition: {}", e))?;

        let scope = TreeScope(TreeScope_Element.0 | TreeScope_Descendants.0);
        let elements = root
            .FindAll(scope, &condition)
            .map_err(|e| format!("FindAll failed: {}", e))?;

        let count = elements
            .Length()
            .map_err(|e| format!("Failed to get element count: {}", e))?;

        for i in 0..count {
            let elem = match elements.GetElement(i) {
                Ok(e) => e,
                Err(_) => continue,
            };

            let name = match elem.CurrentName() {
                Ok(n) => n.to_string(),
                Err(_) => continue,
            };

            if !name.is_empty() {
                visitor(&name);
            }
        }

        Ok(())
    }
}

/// Check if any of the element's text properties contain the search string (case-insensitive).
/// Returns the first matching text, or None. Checks name, then value, then help text.
pub(super) fn match_element_text(
    name: Option<&str>,
    value: Option<&str>,
    help: Option<&str>,
    search_lower: &str,
) -> Option<String> {
    [name, value, help]
        .into_iter()
        .flatten()
        .find(|text| text.to_lowercase().contains(search_lower))
        .map(|s| s.to_string())
}

/// Find text in UI elements of the foreground window using UIA.
///
/// Searches the accessibility tree of the foreground window for elements
/// whose Name, Value, or HelpText property contains the search string (case-insensitive).
/// Returns matching elements with screen coordinates for clicking.
pub fn find_text(search: &str) -> Result<Vec<TextMatch>, String> {
    let debug = std::env::var("NATIVE_DEVTOOLS_DEBUG").is_ok();
    let search_lower = search.to_lowercase();
    let mut matches = Vec::new();

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)
            .map_err(|e| format!("Failed to create IUIAutomation: {}", e))?;

        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return Ok(Vec::new());
        }

        let root = automation
            .ElementFromHandle(hwnd)
            .map_err(|e| format!("Failed to get element from foreground window: {}", e))?;

        if debug {
            let name = root.CurrentName().unwrap_or_default();
            eprintln!(
                "[DEBUG uia::find_text] search='{}', foreground_hwnd={:?}, window_name='{}'",
                search, hwnd, name
            );
        }

        let condition = automation
            .CreateTrueCondition()
            .map_err(|e| format!("Failed to create condition: {}", e))?;

        let scope = TreeScope(TreeScope_Element.0 | TreeScope_Descendants.0);
        let elements = root
            .FindAll(scope, &condition)
            .map_err(|e| format!("FindAll failed: {}", e))?;

        let count = elements
            .Length()
            .map_err(|e| format!("Failed to get element count: {}", e))?;

        if debug {
            eprintln!("[DEBUG uia::find_text] scanning {} elements", count);
        }

        let mut seen_centers: std::collections::HashSet<(i32, i32)> =
            std::collections::HashSet::new();

        for i in 0..count {
            let elem = match elements.GetElement(i) {
                Ok(e) => e,
                Err(_) => continue,
            };

            // Get bounding rectangle first; skip zero-size elements.
            let rect = match elem.CurrentBoundingRectangle() {
                Ok(r) => r,
                Err(_) => continue,
            };

            let width = (rect.right - rect.left) as f64;
            let height = (rect.bottom - rect.top) as f64;
            if width <= 0.0 || height <= 0.0 {
                continue;
            }

            // Collect the three text properties.
            let (name, value, help) = element_text_properties(&elem);

            // Find the first property that matches the search string.
            let matched_text = match_element_text(
                name.as_deref(),
                value.as_deref(),
                help.as_deref(),
                &search_lower,
            );

            let matched_text = match matched_text {
                Some(t) => t,
                None => continue,
            };

            let cx = rect.left as f64 + width / 2.0;
            let cy = rect.top as f64 + height / 2.0;

            // Deduplicate by center coordinates (quantized to 2px grid).
            let key = ((cx / 2.0) as i32, (cy / 2.0) as i32);
            if !seen_centers.insert(key) {
                continue;
            }

            let role = elem
                .CurrentControlType()
                .ok()
                .and_then(|ct| uia_control_type_name(ct.0));

            let bounds = TextBounds {
                x: rect.left as f64,
                y: rect.top as f64,
                width,
                height,
            };

            matches.push(TextMatch {
                text: matched_text,
                x: cx,
                y: cy,
                confidence: 1.0,
                bounds,
                role,
            });
        }

        if debug {
            eprintln!(
                "[DEBUG uia::find_text] found {} matches out of {} elements",
                matches.len(),
                count
            );
        }
    }

    Ok(matches)
}

/// Collect all unique non-empty element names from the UIA tree of the foreground window.
/// Used to provide a list of available elements when a search returns no matches.
pub fn list_element_names() -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for_each_element_name(|name| {
        let trimmed = name.trim();
        if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
            names.push(trimmed.to_string());
        }
    })?;

    Ok(names)
}

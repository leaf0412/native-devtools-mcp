use super::tree::{build_element_json, find_smallest_element_at_point, resolve_app_pids};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};

/// Container control types that warrant a descendant search for a more specific element.
const CONTAINER_TYPES: &[i32] = &[
    50032, // Window
    50033, // Pane
    50026, // Group
    50014, // ScrollBar
];

/// Get the UI Automation element at the given screen coordinates.
///
/// Uses `IUIAutomation::ElementFromPoint` to find the element at (x, y).
/// When `app_name` is provided, verifies the element belongs to that app by PID;
/// if not, walks descendants filtered by PID.
/// When the result is a container type (Window, Pane, Group, ScrollBar), walks
/// descendants to find the smallest-area element containing the point.
/// Returns a JSON object with the element's attributes.
pub fn element_at_point(
    x: f64,
    y: f64,
    app_name: Option<&str>,
) -> Result<serde_json::Value, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)
            .map_err(|e| format!("Failed to create IUIAutomation: {}", e))?;

        let point = windows::Win32::Foundation::POINT {
            x: x as i32,
            y: y as i32,
        };

        let mut elem = automation
            .ElementFromPoint(point)
            .map_err(|e| format!("No accessibility element found at ({}, {}): {}", x, y, e))?;

        // Step 1: App-name scoping — verify the element belongs to the target app.
        if let Some(name) = app_name {
            let target_pids = resolve_app_pids(name);
            if !target_pids.is_empty() {
                let elem_pid = elem.CurrentProcessId().unwrap_or(0);
                if !target_pids.contains(&elem_pid) {
                    // Element doesn't belong to target app — walk descendants to find one that does.
                    if let Some(scoped) =
                        find_smallest_element_at_point(&automation, &elem, x, y, Some(&target_pids))
                    {
                        elem = scoped;
                    }
                }
            }
        }

        // Step 2: Container fallback — if the element is a container, find a more specific child.
        let control_type = elem.CurrentControlType().map(|ct| ct.0).unwrap_or(0);
        if CONTAINER_TYPES.contains(&control_type) {
            if let Some(deeper) = find_smallest_element_at_point(&automation, &elem, x, y, None) {
                elem = deeper;
            }
        }

        build_element_json(&elem)
    }
}

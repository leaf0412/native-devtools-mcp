//! Window enumeration and management for Windows.

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::mem;
use std::os::windows::ffi::OsStringExt;
use windows::core::PWSTR;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, GW_OWNER,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: u32,
    pub name: Option<String>,
    pub owner_name: String,
    pub owner_pid: i64,
    pub bounds: WindowBounds,
    pub layer: i64,
    pub is_on_screen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WindowBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

struct WindowEnumData {
    windows: Vec<WindowInfo>,
}

/// List all visible windows on screen.
pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    let mut data = WindowEnumData {
        windows: Vec::new(),
    };

    unsafe {
        let result = EnumWindows(
            Some(window_enum_callback),
            LPARAM(&mut data as *mut _ as isize),
        );

        if result.is_err() {
            return Err("EnumWindows failed".to_string());
        }
    }

    Ok(data.windows)
}

unsafe extern "system" fn window_enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let data = &mut *(lparam.0 as *mut WindowEnumData);

    // Skip invisible windows
    if !IsWindowVisible(hwnd).as_bool() {
        return TRUE;
    }

    // Skip windows with no title
    let title_len = GetWindowTextLengthW(hwnd);
    if title_len == 0 {
        return TRUE;
    }

    // Skip windows that have an owner (popup windows, tooltips, etc.)
    if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
        if !owner.is_invalid() {
            return TRUE;
        }
    }

    // Get window title
    let mut title_buf: Vec<u16> = vec![0; (title_len + 1) as usize];
    let copied = GetWindowTextW(hwnd, &mut title_buf);
    let name = if copied > 0 {
        Some(
            OsString::from_wide(&title_buf[..copied as usize])
                .to_string_lossy()
                .into_owned(),
        )
    } else {
        None
    };

    // Get owner process ID
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));

    // Get process name
    let owner_name = get_process_name(pid).unwrap_or_default();

    // Get window bounds - prefer DWM extended frame bounds for accuracy
    let bounds = get_window_bounds(hwnd);

    data.windows.push(WindowInfo {
        id: hwnd.0 as usize as u32,
        name,
        owner_name,
        owner_pid: pid as i64,
        bounds,
        layer: 0, // Windows doesn't have the same layer concept as macOS
        is_on_screen: true,
    });

    TRUE
}

/// Get window bounds, preferring DWM extended frame bounds.
fn get_window_bounds(hwnd: HWND) -> WindowBounds {
    let mut rect = RECT::default();

    // Try DWM extended frame bounds first (more accurate, excludes invisible borders)
    let dwm_result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut _ as *mut _,
            mem::size_of::<RECT>() as u32,
        )
    };

    if dwm_result.is_err() {
        // Fall back to GetWindowRect
        unsafe {
            let _ = GetWindowRect(hwnd, &mut rect);
        }
    }

    WindowBounds {
        x: rect.left as f64,
        y: rect.top as f64,
        width: (rect.right - rect.left) as f64,
        height: (rect.bottom - rect.top) as f64,
    }
}

/// Get the executable name for a process ID.
fn get_process_name(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        let mut buf: Vec<u16> = vec![0; 260];
        let mut size = buf.len() as u32;

        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        );

        let _ = windows::Win32::Foundation::CloseHandle(handle);

        if result.is_ok() && size > 0 {
            let path = OsString::from_wide(&buf[..size as usize])
                .to_string_lossy()
                .into_owned();
            // Extract just the filename
            path.rsplit('\\').next().map(|s| s.to_string())
        } else {
            None
        }
    }
}

/// Find a window by its ID (HWND as u32).
pub fn find_window_by_id(window_id: u32) -> Result<Option<WindowInfo>, String> {
    Ok(list_windows()?.into_iter().find(|w| w.id == window_id))
}

/// Find windows by application name (case-insensitive substring match).
///
/// Returns ALL matching windows, sorted best-first so callers can take
/// `windows[0]` as the main window. Parity with the macOS implementation, which
/// sorts on-screen-before-off-screen then by descending area. On Windows
/// `list_windows` already filters to visible windows and every result has
/// `is_on_screen == true`, so the on-screen tiebreak is a no-op here and we sort
/// by descending area only.
pub fn find_windows_by_app(app_name: &str) -> Result<Vec<WindowInfo>, String> {
    let needle = app_name.to_lowercase();
    let mut windows: Vec<WindowInfo> = list_windows()?
        .into_iter()
        .filter(|w| w.owner_name.to_lowercase().contains(&needle))
        .collect();
    windows.sort_by(|a, b| window_area(b).total_cmp(&window_area(a)));
    Ok(windows)
}

fn window_area(window: &WindowInfo) -> f64 {
    window.bounds.width * window.bounds.height
}

/// Get HWND from window ID.
pub fn hwnd_from_id(window_id: u32) -> HWND {
    HWND(window_id as usize as *mut std::ffi::c_void)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_windows() {
        let windows = list_windows().expect("list_windows should succeed");
        // On a typical Windows system, there should be at least one window
        println!("Found {} windows", windows.len());
        for w in &windows {
            println!("  {} - {:?} ({})", w.id, w.name, w.owner_name);
        }
    }
}

use super::lifecycle::nsstring_to_string;
use cocoa::base::{id, nil};
use objc::{class, msg_send, sel, sel_impl};

/// Known Chrome-family bundle identifiers.
const CHROME_BUNDLE_IDS: &[&str] = &[
    "com.google.Chrome",
    "com.google.Chrome.canary",
    "com.brave.Browser",
    "com.microsoft.edgemac",
    "company.thebrowser.Browser",
    "org.chromium.Chromium",
];

/// Check if an app is a Chrome-family browser by its bundle ID.
pub fn is_chrome_browser(bundle_id: Option<&str>, _app_name: &str) -> bool {
    bundle_id.is_some_and(|bid| CHROME_BUNDLE_IDS.contains(&bid))
}

/// Check if a running app is an Electron app by inspecting its bundle for
/// `Contents/Frameworks/Electron Framework.framework`.
pub fn is_electron_app_by_pid(pid: i32) -> bool {
    bundle_path_for_pid(pid)
        .map(|p| is_electron_bundle(&p))
        .unwrap_or(false)
}

/// Check if a non-running app is Electron by searching standard application
/// directories for `<app_name>.app` and inspecting the bundle.
pub fn is_electron_app_by_name(app_name: &str) -> bool {
    find_app_bundle(app_name)
        .map(|p| is_electron_bundle(&p))
        .unwrap_or(false)
}

fn is_electron_bundle(bundle_path: &str) -> bool {
    std::path::Path::new(bundle_path)
        .join("Contents/Frameworks/Electron Framework.framework")
        .exists()
}

/// Get the `.app` bundle path for a running app by PID.
fn bundle_path_for_pid(pid: i32) -> Option<String> {
    unsafe {
        let app: id = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        if app == nil {
            return None;
        }
        let url: id = msg_send![app, bundleURL];
        if url == nil {
            return None;
        }
        let path: id = msg_send![url, path];
        if path == nil {
            return None;
        }
        Some(nsstring_to_string(path))
    }
}

/// Search standard application directories for `<app_name>.app`.
fn find_app_bundle(app_name: &str) -> Option<String> {
    let dirs = [
        "/Applications",
        "/System/Applications",
        "/System/Applications/Utilities",
    ];
    let home_apps = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join("Applications"));

    for dir in dirs
        .iter()
        .map(std::path::PathBuf::from)
        .chain(home_apps.into_iter())
    {
        let candidate = dir.join(format!("{}.app", app_name));
        if candidate.exists() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // MARK: - is_chrome_browser tests

    #[test]
    fn test_chrome_bundle_ids_detected() {
        for bid in CHROME_BUNDLE_IDS {
            assert!(
                is_chrome_browser(Some(bid), "irrelevant"),
                "Expected ChromeBrowser for bundle_id={}",
                bid,
            );
        }
    }

    #[test]
    fn test_non_chrome_bundle_id() {
        assert!(!is_chrome_browser(Some("com.apple.Safari"), "Safari"));
    }

    #[test]
    fn test_no_bundle_id() {
        assert!(!is_chrome_browser(None, "anything"));
    }

    // MARK: - is_electron_bundle tests

    #[test]
    fn test_electron_bundle_detected() {
        let dir = tempfile::tempdir().unwrap();
        let framework_path = dir
            .path()
            .join("Contents/Frameworks/Electron Framework.framework");
        std::fs::create_dir_all(&framework_path).unwrap();

        assert!(is_electron_bundle(dir.path().to_str().unwrap()));
    }

    #[test]
    fn test_non_electron_bundle() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_electron_bundle(dir.path().to_str().unwrap()));
    }

    #[test]
    fn test_nonexistent_path_not_electron() {
        assert!(!is_electron_bundle("/nonexistent/path"));
    }
}

//! Attach to the user's *existing* default-profile Chrome via its
//! `DevToolsActivePort` file.
//!
//! ## Why this exists
//!
//! Chrome 136 introduced a security restriction: the `--remote-debugging-port`
//! flag is silently ignored when Chrome is launched against the default user
//! profile. Chrome 144 re-opened a narrow path: the new
//! `chrome://inspect/#remote-debugging` "Allow remote debugging" toggle
//! *does* expose a CDP endpoint on the default profile, but only over a
//! direct WebSocket — Chrome 144 returns **HTTP 404** for the standard
//! `/json/version` discovery endpoint, breaking any client that does
//! "GET /json/version → connect to the returned `webSocketDebuggerUrl`".
//!
//! chromiumoxide's `Browser::connect(http://...)` is one such client. The
//! fix is to skip HTTP discovery and pass a `ws://` URL directly. That URL
//! is exactly what Chrome writes into `<userDataDir>/DevToolsActivePort`:
//!
//! ```text
//! 9222
//! /devtools/browser/<uuid>
//! ```
//!
//! ## Functions
//!
//! - [`parse_devtools_active_port`] — tolerant parser for that two-line
//!   file. Pure, no I/O, unit-tested in `tests/auto_connect_tests.rs`.
//! - [`default_chrome_profile_path`] — platform-resolved path to Chrome's
//!   default user-data dir (where `DevToolsActivePort` lives).
//! - [`read_devtools_active_port`] — file I/O wrapper around the parser.
//! - [`build_ws_url`] — assembles the `ws://127.0.0.1:<port><path>` URL
//!   from a parsed endpoint.
//! - [`connect_default_chrome`] — orchestrates parse → `CdpClient::connect_ws`
//!   for the tool handler.
//!
//! ## Non-goals
//!
//! - Other Chromium-based browsers (Edge, Brave, Arc, Vivaldi). The
//!   `DevToolsActivePort` location differs per browser; this module only
//!   resolves stable Google Chrome on macOS / Linux / Windows.
//! - Chrome Canary / Beta / Dev channels. Stable only for now — channel
//!   selection can be added when there's a real need.
//! - Windows profile paths that diverge when `User Data` lives on a custom
//!   drive. We honour `%LOCALAPPDATA%` and assume the default install.

use std::path::PathBuf;

use crate::cdp::CdpClient;

/// Parsed contents of a `DevToolsActivePort` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevToolsEndpoint {
    pub port: u16,
    pub ws_path: String,
}

/// Parse the contents of a `DevToolsActivePort` file.
///
/// The format is two lines: a decimal port number and an absolute WebSocket
/// path. This parser is intentionally lenient: it strips trailing whitespace
/// (including `\r`) per line and ignores leading whitespace, but it does
/// NOT auto-prefix `/` to the ws_path — Chrome always writes the leading
/// slash, and silently adding one would mask a malformed file.
pub fn parse_devtools_active_port(content: &str) -> Result<DevToolsEndpoint, String> {
    let mut lines = content.lines();

    let raw_port = lines
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "DevToolsActivePort is empty or missing the port line".to_string())?;

    let port: u16 = raw_port
        .parse()
        .map_err(|e| format!("Invalid port '{}' in DevToolsActivePort: {}", raw_port, e))?;
    if port == 0 {
        return Err("Invalid port 0 in DevToolsActivePort (must be 1-65535)".into());
    }

    let ws_path = lines
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "DevToolsActivePort is missing the WebSocket path line".to_string()
        })?
        .to_string();

    Ok(DevToolsEndpoint { port, ws_path })
}

/// Resolve the default Chrome user-data directory on this platform.
///
/// Returns `None` when the platform's well-known location cannot be derived
/// (e.g. `HOME` / `LOCALAPPDATA` unset). The caller should surface a clear
/// "Chrome profile directory not found" error rather than guessing.
pub fn default_chrome_profile_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|h| {
            PathBuf::from(h)
                .join("Library")
                .join("Application Support")
                .join("Google")
                .join("Chrome")
        })
    }
    #[cfg(target_os = "linux")]
    {
        // Linux Chrome uses XDG-style config: ~/.config/google-chrome.
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("google-chrome"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Google").join("Chrome").join("User Data"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// Read `DevToolsActivePort` from Chrome's default user-data directory.
///
/// Returns a precise error if the file is missing or unreadable so the tool
/// handler can surface a hint about the `chrome://inspect/#remote-debugging`
/// toggle.
pub fn read_devtools_active_port() -> Result<DevToolsEndpoint, String> {
    let dir = default_chrome_profile_path().ok_or_else(|| {
        "Cannot determine the Chrome user-data directory on this platform \
         (HOME / LOCALAPPDATA not set)"
            .to_string()
    })?;
    let path = dir.join("DevToolsActivePort");
    let content = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "DevToolsActivePort not found at {}: {}. \
             Open Chrome, navigate to chrome://inspect/#remote-debugging, \
             enable 'Allow remote debugging for this browser instance', \
             then try again.",
            path.display(),
            e
        )
    })?;
    parse_devtools_active_port(&content)
}

/// Build the loopback WebSocket URL Chromiumoxide expects.
pub fn build_ws_url(endpoint: &DevToolsEndpoint) -> String {
    format!("ws://127.0.0.1:{}{}", endpoint.port, endpoint.ws_path)
}

/// Connect to the user's default-profile Chrome via its `DevToolsActivePort`.
///
/// On success, returns the connected [`CdpClient`] and the parsed endpoint
/// (so callers can log the actual port + ws path that were used).
pub async fn connect_default_chrome() -> Result<(CdpClient, DevToolsEndpoint), String> {
    let endpoint = read_devtools_active_port()?;
    let url = build_ws_url(&endpoint);
    let client = CdpClient::connect_ws(&url, endpoint.port).await?;
    Ok((client, endpoint))
}

#[cfg(test)]
mod unit_tests {
    //! Mirrors the integration tests in `tests/auto_connect_tests.rs` for
    //! cases that don't need any I/O. Kept here so a `cargo test --lib`
    //! run still exercises the pure parser without bringing up the
    //! integration harness.

    use super::*;

    #[test]
    fn parse_rejects_zero_port_inline() {
        assert!(parse_devtools_active_port("0\n/x\n").is_err());
    }

    #[test]
    fn build_ws_url_has_no_double_slash_at_port_boundary() {
        let ep = DevToolsEndpoint {
            port: 9222,
            ws_path: "/devtools/browser/abc".to_string(),
        };
        let url = build_ws_url(&ep);
        assert!(!url.contains("9222//"), "got double slash in {url}");
    }
}
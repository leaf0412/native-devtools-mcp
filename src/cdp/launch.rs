//! Launch a managed debug browser and connect to it in one step.
//!
//! Unlike `cdp_connect` (which attaches to a browser the user already started
//! with `--remote-debugging-port`), this spawns Chrome against a STABLE
//! dedicated profile directory so logins persist across runs: the user signs
//! in once and every later launch reuses that profile's cookies.
//!
//! It deliberately cannot attach to the user's normal default-profile Chrome:
//! Chrome 136+ refuses the debug port on the default profile, and a browser
//! already running without the flag cannot have it enabled retroactively. A
//! dedicated, persistent profile is the robust way to get a logged-in *and*
//! debuggable browser.

use super::CdpClient;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Resolve (and create) the stable debug-profile directory, e.g.
/// `~/.native-devtools-mcp/<profile>`. Rejects names that could escape the
/// managed directory rather than silently accepting them.
fn profile_dir(profile: &str) -> Result<PathBuf, String> {
    if profile.is_empty() || profile.contains(['/', '\\']) || profile.contains("..") {
        return Err(format!(
            "Invalid profile name '{}': must not be empty or contain path separators or '..'",
            profile
        ));
    }
    let home = home_dir().ok_or("Cannot resolve home directory")?;
    let dir = home.join(".native-devtools-mcp").join(profile);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create profile dir {}: {}", dir.display(), e))?;
    Ok(dir)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// Spawn Chrome as a new instance bound to `port`, using `dir` as its
/// user-data-dir so the profile (and its logins) persists.
///
/// No URL is passed on the command line: `open -na ... <url>` is unreliable
/// (the URL is often routed to an already-running Chrome rather than this new
/// instance). We open the target page over CDP after connecting instead.
fn spawn_chrome(port: u16, dir: &Path) -> Result<(), String> {
    let debug = format!("--remote-debugging-port={}", port);
    let data_dir = format!("--user-data-dir={}", dir.display());

    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.args(["-na", "Google Chrome", "--args", &debug, &data_dir]);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", "chrome", &debug, &data_dir]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = {
        let mut c = std::process::Command::new("google-chrome");
        c.args([&debug, &data_dir]);
        c
    };

    let status = cmd
        .status()
        .map_err(|e| format!("Failed to spawn Chrome: {}. Is Google Chrome installed?", e))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Chrome launch command exited with status {}", status))
    }
}

/// Open `url` as a freshly created page and select it. A newly launched
/// Chrome instance may expose only extension targets (no real page), so we
/// create the page over CDP rather than relying on the launch command line.
async fn open_page(client: &mut CdpClient, url: &str) -> Result<(), String> {
    let page = client
        .browser
        .new_page(url)
        .await
        .map_err(|e| format!("Connected but failed to open page {}: {}", url, e))?;
    client.selected_page = Some(page);
    Ok(())
}

/// Launch (or reuse) a managed debug Chrome on `port` and connect to it.
///
/// Returns `(client, reused)`: `reused == true` means a debug browser was
/// already answering on the port and was attached to without relaunching —
/// this is what preserves an existing logged-in session. Otherwise Chrome is
/// spawned against the stable profile and we poll until the debug port answers.
pub async fn launch_and_connect(
    port: u16,
    profile: &str,
    url: &str,
) -> Result<(CdpClient, bool), String> {
    // Reuse path: a debug browser is already up on this port. Leave its
    // existing tabs untouched (that's the logged-in session we want); only
    // open the target page if it has no usable page selected.
    if let Ok(mut client) = CdpClient::connect(port).await {
        if client.selected_page.is_none() {
            open_page(&mut client, url).await?;
        }
        return Ok((client, true));
    }

    let dir = profile_dir(profile)?;
    spawn_chrome(port, &dir)?;

    // Chrome needs a moment to open the debug port; retry until it answers,
    // then open the target page over CDP.
    let mut last_err = String::new();
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        match CdpClient::connect(port).await {
            Ok(mut client) => {
                open_page(&mut client, url).await?;
                return Ok((client, false));
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!(
        "Launched Chrome but could not connect on port {} within ~15s. Last error: {}",
        port, last_err
    ))
}

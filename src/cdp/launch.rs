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
use std::process::{Child, Command, Stdio};
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

/// Locate the Chrome executable for a direct (headless) launch, where the OS
/// app-launcher (`open` / `start`) can't be used to pass `--headless`.
fn chrome_binary() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        const STD: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        if Path::new(STD).exists() {
            return Some(PathBuf::from(STD));
        }
        let user = home_dir()?.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
        user.exists().then_some(user)
    }
    #[cfg(target_os = "windows")]
    {
        for p in [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ] {
            if Path::new(p).exists() {
                return Some(PathBuf::from(p));
            }
        }
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Some(PathBuf::from("google-chrome"))
    }
}

/// Spawn Chrome as a new instance bound to `port`, using `dir` as its
/// user-data-dir. Returns the [`Child`] only when we launched the binary
/// directly (headless) so the caller can kill it; the windowed `open`/`start`
/// path detaches and returns `None` (left running on purpose).
///
/// No URL is passed on the command line: `open -na ... <url>` is unreliable
/// (the URL is often routed to an already-running Chrome rather than this new
/// instance). We open the target page over CDP after connecting instead.
fn spawn_chrome(port: u16, dir: &Path, headless: bool) -> Result<Option<Child>, String> {
    let debug = format!("--remote-debugging-port={}", port);
    let data_dir = format!("--user-data-dir={}", dir.display());

    if headless {
        let bin = chrome_binary().ok_or(
            "Cannot find the Google Chrome binary for a headless launch. \
             Install Chrome at the standard location.",
        )?;
        let child = Command::new(bin)
            .args([
                "--headless=new",
                "--disable-gpu",
                "--no-first-run",
                "--no-default-browser-check",
                &debug,
                &data_dir,
                "about:blank",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to spawn headless Chrome: {}", e))?;
        return Ok(Some(child));
    }

    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.args(["-na", "Google Chrome", "--args", &debug, &data_dir]);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", "chrome", &debug, &data_dir]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = {
        let mut c = Command::new("google-chrome");
        c.args([&debug, &data_dir]);
        c
    };

    let status = cmd
        .status()
        .map_err(|e| format!("Failed to spawn Chrome: {}. Is Google Chrome installed?", e))?;
    if status.success() {
        Ok(None)
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
    headless: bool,
    ephemeral: bool,
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

    // Ephemeral (CI) uses a throwaway temp profile for a reproducible,
    // contention-free run; otherwise the stable managed profile persists logins.
    let (dir, mut tempdir): (PathBuf, Option<tempfile::TempDir>) = if ephemeral {
        let td = tempfile::Builder::new()
            .prefix("ndt-cdp-")
            .tempdir()
            .map_err(|e| format!("Failed to create ephemeral profile dir: {}", e))?;
        (td.path().to_path_buf(), Some(td))
    } else {
        (profile_dir(profile)?, None)
    };

    let mut child = spawn_chrome(port, &dir, headless)?;

    // Chrome needs a moment to open the debug port; retry until it answers,
    // then open the target page over CDP.
    let mut last_err = String::new();
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        match CdpClient::connect(port).await {
            Ok(mut client) => {
                open_page(&mut client, url).await?;
                client.chrome_child = child.take();
                client.profile_tempdir = tempdir.take();
                return Ok((client, false));
            }
            Err(e) => last_err = e,
        }
    }
    // Never connected — don't leak the process or temp dir we just created.
    if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    drop(tempdir);
    Err(format!(
        "Launched Chrome but could not connect on port {} within ~15s. Last error: {}",
        port, last_err
    ))
}

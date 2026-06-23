//! Chrome DevTools Protocol (CDP) client for browser automation.
//!
//! Connects to Chrome/Electron apps via their remote debugging port
//! using the chromiumoxide crate.

pub mod auto_connect;
pub mod dom_discovery;
pub mod launch;
pub mod tools;

use chromiumoxide::browser::Browser;
use chromiumoxide::page::Page;
use futures_util::StreamExt;
use rmcp::model::{CallToolResult, Content};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

pub const DOM_UID_PREFIX: &str = "d";

/// Shared, optionally-connected CDP client owned by the MCP server.
pub type SharedCdp = Arc<RwLock<Option<CdpClient>>>;

/// CDP client state, owned by the MCP server.
pub struct CdpClient {
    pub browser: Browser,
    pub selected_page: Option<Page>,
    pub handler_handle: JoinHandle<()>,
    pub last_dom_snapshot: Option<SnapshotMap>,
    pub last_page_list: Vec<Page>,
    /// Monotonic counter bumped on every page-lifecycle event that could
    /// invalidate the `backendNodeId` space (navigate, reload, select/new/close
    /// page). Stamped onto each [`SnapshotMap`] at creation time so lookups
    /// can detect stale snapshots even when the page URL hasn't changed
    /// (same-URL reload, SPA pushState/replaceState, switching to another tab
    /// with an identical URL).
    pub generation: u64,
    /// The Chrome process when *we* spawned it directly (headless/ephemeral
    /// launches). Killed on [`Self::disconnect`]. `None` for `cdp_connect` and
    /// for the persistent `open -na` launch (which detaches and is left running
    /// on purpose so its logged-in session survives).
    pub chrome_child: Option<std::process::Child>,
    /// Temp profile dir for an ephemeral launch; removed when this field drops
    /// (on disconnect), after the Chrome process is killed.
    pub profile_tempdir: Option<tempfile::TempDir>,
}

impl CdpClient {
    /// Connect to a Chrome/Electron instance via its remote debugging port.
    ///
    /// Resolves the WebSocket URL from `http://127.0.0.1:{port}`, spawns the
    /// chromiumoxide handler loop, and auto-selects the first non-extension page.
    pub async fn connect(port: u16) -> Result<Self, String> {
        let url = format!("http://127.0.0.1:{}", port);
        let (mut browser, mut handler) = Browser::connect(&url)
            .await
            .map_err(|e| format!(
                "Cannot connect to CDP on port {port}: {e}. Check: (1) a browser is running \
                 with --remote-debugging-port={port} and a non-default --user-data-dir \
                 (Chrome 136+ requires this); (2) no other process is holding the port without \
                 debugging enabled; (3) or use cdp_launch to start a managed debug browser \
                 automatically.",
            ))?;

        let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });

        // Discover pre-existing targets (pages opened before we connected).
        // Chrome 136+ with chromiumoxide: fetch_targets() queues discovery but
        // Page objects are NOT guaranteed to be ready when it returns. We must
        // poll until at least one real page appears or we time out.
        let selected_page = poll_for_page(&mut browser, std::time::Duration::from_secs(10)).await?;

        Ok(Self {
            browser,
            selected_page,
            handler_handle,
            last_dom_snapshot: None,
            last_page_list: Vec::new(),
            generation: 0,
            chrome_child: None,
            profile_tempdir: None,
        })
    }

    /// Connect to a Chromium instance using an already-resolved WebSocket URL.
    ///
    /// Skips chromiumoxide's HTTP-discovery step (which would `GET /json/version`
    /// and fail on browsers that reject that endpoint, e.g. Chrome 144+ on the
    /// default profile when remote debugging is enabled only via the
    /// `chrome://inspect/#remote-debugging` toggle).
    ///
    /// The URL is expected to be `ws://127.0.0.1:<port><path>`, exactly as
    /// written in `<userDataDir>/DevToolsActivePort`. Use
    /// [`crate::cdp::auto_connect::connect_default_chrome`] to wire that up.
    pub async fn connect_ws(ws_url: &str) -> Result<Self, String> {
        let (mut browser, mut handler) = Browser::connect(ws_url)
            .await
            .map_err(|e| format!(
                "Cannot connect to CDP at {ws_url}: {e}. Check that the browser is running \
                 and that its remote debugging endpoint is reachable."
            ))?;

        let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });

        let selected_page = poll_for_page(&mut browser, std::time::Duration::from_secs(10)).await?;

        Ok(Self {
            browser,
            selected_page,
            handler_handle,
            last_dom_snapshot: None,
            last_page_list: Vec::new(),
            generation: 0,
            chrome_child: None,
            profile_tempdir: None,
        })
    }

    /// Disconnect from the browser by aborting the handler task. If we spawned
    /// the Chrome process ourselves (headless/ephemeral), kill it; the temp
    /// profile dir is then removed when `profile_tempdir` drops.
    pub fn disconnect(mut self) {
        self.handler_handle.abort();
        if let Some(mut child) = self.chrome_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // self.profile_tempdir drops here, removing the temp dir.
    }

    /// Mark the current `backendNodeId` space as invalidated.
    ///
    /// Bumps [`Self::generation`] and clears the DOM snapshot cache. Call
    /// after any navigation, reload, or page switch that invalidates
    /// element UIDs.
    pub fn invalidate_snapshots(&mut self) {
        self.last_dom_snapshot = None;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Get the selected page, or return a tool error.
    pub fn require_page(&self) -> Result<Page, CallToolResult> {
        self.selected_page.clone().ok_or_else(|| {
            cdp_error("No page selected. Use cdp_list_pages and cdp_select_page first.")
        })
    }
}

/// Check out the selected [`Page`] and the current generation under a brief
/// read lock, releasing the lock *before returning*.
///
/// This is the entry point for the project's concurrency rule: **never hold
/// the CDP lock across a `page.execute().await`**. Callers get an owned `Page`
/// clone (chromiumoxide pages are cheap `Arc` handles) plus the generation
/// stamp, then do all async CDP work lock-free. Mutations are written back via
/// [`commit_cdp`].
///
/// Returns a ready-to-return tool error if there is no connection or no
/// selected page.
pub async fn checkout_page(client: &SharedCdp) -> Result<(Page, u64), CallToolResult> {
    let guard = client.read().await;
    let c = guard
        .as_ref()
        .ok_or_else(|| cdp_error("No CDP connection. Use cdp_connect first."))?;
    let page = c.require_page()?;
    Ok((page, c.generation))
}

/// Re-acquire the write lock *briefly* to commit mutations back onto the
/// `CdpClient` after lock-free async work has completed.
///
/// If the client was disconnected while the lock was released, `f` is not
/// called (the mutation is silently dropped — there is nothing left to mutate).
pub async fn commit_cdp<F>(client: &SharedCdp, f: F)
where
    F: FnOnce(&mut CdpClient),
{
    if let Some(c) = client.write().await.as_mut() {
        f(c);
    }
}

/// Convenience helper to get the URL of a page, returning an empty string on failure.
pub async fn page_url(page: &Page) -> String {
    page.url().await.ok().flatten().unwrap_or_default()
}

/// Return true if the URL belongs to a Chrome extension.
pub(crate) fn is_extension_url(url: &str) -> bool {
    url.starts_with("chrome-extension://")
}

/// Find the first non-extension page from a list of pages.
async fn first_non_extension_page(pages: &[Page]) -> Option<Page> {
    for page in pages {
        let url = page_url(page).await;
        if !is_extension_url(&url) {
            return Some(page.clone());
        }
    }
    None
}

/// Discover pre-existing targets and wait for at least one page to appear.
///
/// `fetch_targets()` sends `Target.getTargets` and triggers `AttachToTarget`
/// for each discovered target. The attach is asynchronous — the handler must
/// process the responses before `pages()` can see them. We call `fetch_targets`
/// once, then poll `pages()` until a non-extension page appears or we time out.
async fn poll_for_page(
    browser: &mut Browser,
    timeout: std::time::Duration,
) -> Result<Option<Page>, String> {
    // Kick off target discovery once. This triggers AttachToTarget for each
    // existing target, which the handler processes asynchronously.
    let _ = browser.fetch_targets().await;

    let interval = std::time::Duration::from_millis(100);
    let start = std::time::Instant::now();

    loop {
        let pages = browser
            .pages()
            .await
            .map_err(|e| format!("Failed to list pages: {}", e))?;

        if let Some(page) = first_non_extension_page(&pages).await {
            return Ok(Some(page));
        }

        if start.elapsed() >= timeout {
            return Ok(None);
        }

        tokio::time::sleep(interval).await;
    }
}

/// Shorthand for building a CDP tool error result.
pub fn cdp_error(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.into())])
}

/// Maps snapshot UIDs to CDP node identifiers for click/eval resolution.
///
/// Stale-snapshot detection uses two signals:
/// - `generation`, bumped on every page-lifecycle event the client drives
///   (navigate, reload, page switch) — catches same-URL reloads and SPA
///   navigations that don't change the URL.
/// - `page_url`, compared against the live page URL at lookup time —
///   catches out-of-band navigations (user clicks a link, JS `location.href`)
///   that happen between our tool calls.
///
/// Either signal mismatching is enough to reject the snapshot as stale.
pub struct SnapshotMap {
    pub uid_to_node: HashMap<String, SnapshotNode>,
    /// Rich candidate metadata keyed by d-prefixed UID. This is used by
    /// bounded expansion tools without requiring a fresh page-wide dump.
    pub uid_to_candidate: HashMap<String, dom_discovery::DomCandidate>,
    /// Reverse map: backendNodeId → list of snapshot UIDs.
    /// Skips entries where backendNodeId is 0 (no DOM backing).
    pub backend_to_uids: HashMap<i64, Vec<String>>,
    /// Snapshot order, matching the order UIDs were assigned in the response.
    pub ordered_uids: Vec<String>,
    /// URL of the page at the moment this snapshot was taken.
    pub page_url: String,
    /// Value of [`CdpClient::generation`] at the moment this snapshot was taken.
    pub generation: u64,
}

pub struct SnapshotNode {
    pub backend_node_id: i64,
    pub role: String,
    pub name: String,
}

/// Resolve a `d<N>`-prefixed UID to its SnapshotNode from the DOM map.
///
/// Errors when the prefix isn't `d`, the snapshot is missing or stale
/// (generation bumped or the live page URL changed out-of-band), or the
/// UID isn't present.
pub fn resolve_uid_from_maps<'a>(
    uid: &str,
    dom_snapshot: Option<&'a SnapshotMap>,
    current_generation: u64,
    current_url: &str,
) -> Result<&'a SnapshotNode, String> {
    if !uid.starts_with(DOM_UID_PREFIX) {
        return Err(format!(
            "Unknown UID prefix in '{}'. Expected 'd<N>' (DOM).",
            uid
        ));
    }

    let snapshot = dom_snapshot.ok_or(
        "No DOM snapshot available. Call cdp_take_dom_snapshot or cdp_find_elements first.",
    )?;

    if current_generation != snapshot.generation || current_url != snapshot.page_url {
        return Err(
            "Snapshot is stale — page has navigated since last snapshot. \
             Call cdp_take_dom_snapshot or cdp_find_elements again."
                .to_string(),
        );
    }

    snapshot.uid_to_node.get(uid).ok_or_else(|| {
        format!(
            "uid={} not found in DOM snapshot. Take a fresh snapshot.",
            uid
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.com/";

    fn make_dom_map(generation: u64, uid: &str, backend_node_id: i64) -> SnapshotMap {
        let mut map = SnapshotMap {
            uid_to_node: HashMap::new(),
            uid_to_candidate: HashMap::new(),
            backend_to_uids: HashMap::new(),
            ordered_uids: vec![uid.to_string()],
            page_url: URL.to_string(),
            generation,
        };
        map.uid_to_node.insert(
            uid.to_string(),
            SnapshotNode {
                backend_node_id,
                role: "button".to_string(),
                name: "Submit".to_string(),
            },
        );
        map
    }

    #[test]
    fn resolve_uid_dom_prefix() {
        let dom_map = make_dom_map(3, "d5", 99);

        let result = resolve_uid_from_maps("d5", Some(&dom_map), 3, URL);
        assert!(result.is_ok());
        let node = result.unwrap();
        assert_eq!(node.backend_node_id, 99);
    }

    #[test]
    fn resolve_uid_unknown_prefix_fails() {
        for uid in ["x1", "a1"] {
            match resolve_uid_from_maps(uid, None, 0, URL) {
                Err(msg) => assert!(
                    msg.contains("Unknown UID prefix"),
                    "uid={} got: {}",
                    uid,
                    msg
                ),
                Ok(_) => panic!("expected unknown-prefix error for uid={}", uid),
            }
        }
    }

    fn expect_stale(result: Result<&SnapshotNode, String>) {
        match result {
            Err(msg) => assert!(msg.contains("stale"), "expected stale error, got: {}", msg),
            Ok(_) => panic!("expected stale-snapshot error, got Ok"),
        }
    }

    #[test]
    fn resolve_uid_stale_generation_fails() {
        let dom_map = make_dom_map(1, "d1", 1);

        expect_stale(resolve_uid_from_maps("d1", Some(&dom_map), 2, URL));
    }

    /// Same-URL reload bumps the generation, so a snapshot taken before
    /// the reload must be rejected even though `page.url()` hasn't changed.
    #[test]
    fn same_url_reload_invalidates_snapshot() {
        let dom_map = make_dom_map(0, "d1", 42);

        expect_stale(resolve_uid_from_maps("d1", Some(&dom_map), 1, URL));
    }

    /// An out-of-band navigation (user clicks a link, `location.href = ...`)
    /// changes the live URL without bumping our generation. The snapshot
    /// must still be rejected.
    #[test]
    fn out_of_band_url_change_invalidates_snapshot() {
        let dom_map = make_dom_map(0, "d1", 42);

        expect_stale(resolve_uid_from_maps(
            "d1",
            Some(&dom_map),
            0,
            "https://example.com/different",
        ));
    }

    /// A snapshot looked up at its stamped generation and URL succeeds;
    /// bumping the generation causes the same snapshot to be rejected.
    #[test]
    fn snapshot_taken_before_navigation_is_stale_after_bump() {
        let dom_map = make_dom_map(0, "d1", 42);

        assert!(resolve_uid_from_maps("d1", Some(&dom_map), 0, URL).is_ok());

        expect_stale(resolve_uid_from_maps("d1", Some(&dom_map), 1, URL));
    }
}

//! CDP page management tools: list, select, navigate, new, close, handle_dialog.

use crate::cdp::{cdp_error, is_extension_url, page_url, CdpClient};
use chromiumoxide::cdp::browser_protocol::page::{
    GetNavigationHistoryParams, HandleJavaScriptDialogParams, NavigateParams,
    NavigateToHistoryEntryParams, ReloadParams,
};
use rmcp::model::{CallToolResult, Content};
use std::sync::Arc;
use tokio::sync::RwLock;

pub async fn cdp_list_pages(cdp_client: Arc<RwLock<Option<CdpClient>>>) -> CallToolResult {
    // Hold the lock only across the single `browser.pages()` RPC, then release
    // it before the per-page URL fetches below (which are N more round trips).
    let (pages, selected_target_id) = {
        let guard = cdp_client.read().await;
        let client = match guard.as_ref() {
            Some(c) => c,
            None => return cdp_error("No CDP connection. Use cdp_connect first."),
        };
        let pages = match client.browser.pages().await {
            Ok(p) => p,
            Err(e) => return cdp_error(format!("Failed to list pages: {}", e)),
        };
        let selected_target_id = client.selected_page.as_ref().map(|p| p.target_id().clone());
        (pages, selected_target_id)
    };

    // Filter out chrome-extension:// pages, collecting URLs to avoid double fetch.
    let mut filtered: Vec<chromiumoxide::page::Page> = Vec::new();
    let mut urls: Vec<String> = Vec::new();
    for page in pages {
        let url = page_url(&page).await;
        if !is_extension_url(&url) {
            filtered.push(page);
            urls.push(url);
        }
    }

    let total = filtered.len();
    let mut output = format!("Pages ({} total):\n", total);
    for (i, page) in filtered.iter().enumerate() {
        let marker = if selected_target_id
            .as_ref()
            .is_some_and(|id| id == page.target_id())
        {
            " *"
        } else {
            ""
        };
        output.push_str(&format!("  [{}]{} {}\n", i, marker, urls[i]));
    }

    crate::cdp::commit_cdp(&cdp_client, |c| c.last_page_list = filtered).await;

    CallToolResult::success(vec![Content::text(output.trim_end().to_string())])
}

pub async fn cdp_select_page(
    page_idx: usize,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    // Resolve the target page handle under a brief read lock, then release it
    // before the `bring_to_front` / URL round trips.
    let (page, same_page) = {
        let guard = cdp_client.read().await;
        let client = match guard.as_ref() {
            Some(c) => c,
            None => return cdp_error("No CDP connection. Use cdp_connect first."),
        };

        if client.last_page_list.is_empty() {
            return cdp_error("No page list available. Call cdp_list_pages first.");
        }

        if page_idx >= client.last_page_list.len() {
            return cdp_error(format!(
                "Page index {} is out of range (0..{}). Call cdp_list_pages to refresh.",
                page_idx,
                client.last_page_list.len()
            ));
        }

        let page = client.last_page_list[page_idx].clone();
        let same_page = client
            .selected_page
            .as_ref()
            .is_some_and(|sel| sel.target_id() == page.target_id());
        (page, same_page)
    };

    if let Err(e) = page.bring_to_front().await {
        return cdp_error(format!("Failed to bring page {} to front: {}", page_idx, e));
    }

    let url = page_url(&page).await;
    crate::cdp::commit_cdp(&cdp_client, |c| {
        c.selected_page = Some(page);
        if !same_page {
            c.invalidate_snapshots();
        }
    })
    .await;

    CallToolResult::success(vec![Content::text(format!(
        "Selected page [{}]: {}",
        page_idx, url
    ))])
}

pub async fn cdp_handle_dialog(
    action: String,
    prompt_text: Option<String>,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    let (page, _generation) = match crate::cdp::checkout_page(&cdp_client).await {
        Ok(v) => v,
        Err(e) => return e,
    };

    let accept = match action.as_str() {
        "accept" => true,
        "dismiss" => false,
        _ => {
            return cdp_error(format!(
                "Invalid action '{}'. Use 'accept' or 'dismiss'.",
                action
            ))
        }
    };

    let detail = if let Some(text) = &prompt_text {
        format!(" with text '{}'", text)
    } else {
        String::new()
    };

    let mut params = HandleJavaScriptDialogParams::new(accept);
    params.prompt_text = prompt_text;

    match page.execute(params).await {
        Ok(_) => CallToolResult::success(vec![Content::text(format!(
            "Dialog {}ed{}",
            action, detail
        ))]),
        Err(e) => cdp_error(format!("Failed to handle dialog: {}", e)),
    }
}

const DEFAULT_NAV_TIMEOUT_MS: u64 = 10_000;

pub async fn cdp_navigate(
    url: Option<String>,
    nav_type: Option<String>,
    timeout_ms: Option<u64>,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    // Check out the page lock-free: `Page.navigate` can block for the full
    // timeout (10s default), and holding the lock across it would serialize
    // every other CDP tool for that whole window.
    let (page, _generation) = match crate::cdp::checkout_page(&cdp_client).await {
        Ok(v) => v,
        Err(e) => return e,
    };

    let action = nav_type.as_deref().unwrap_or("url");

    match action {
        "url" => {
            let target_url = match &url {
                Some(u) => u.clone(),
                None => return cdp_error("'url' parameter is required when type is 'url'."),
            };
            // Use a timeout so slow-loading pages don't block indefinitely.
            // The CDP Page.navigate command waits for the frame to commit,
            // which can be slow on heavy pages. If it times out, the navigation
            // likely still succeeded — return success with a note.
            let nav_future = page.execute(NavigateParams::new(&target_url));
            let nav_timeout =
                std::time::Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_NAV_TIMEOUT_MS));

            match tokio::time::timeout(nav_timeout, nav_future).await {
                Ok(Ok(resp)) => {
                    if let Some(error_text) = &resp.result.error_text {
                        return cdp_error(format!(
                            "Navigation to {} failed: {}",
                            target_url, error_text
                        ));
                    }
                    if resp.result.is_download == Some(true) {
                        return cdp_error(format!(
                            "Navigation to {} triggered a download instead of loading a page.",
                            target_url
                        ));
                    }
                    crate::cdp::commit_cdp(&cdp_client, |c| c.invalidate_snapshots()).await;
                    CallToolResult::success(vec![Content::text(format!(
                        "Navigated to {}",
                        target_url
                    ))])
                }
                Ok(Err(e)) => cdp_error(format!("Navigation failed: {}", e)),
                Err(_) => {
                    // Timed out waiting for load event — navigation was sent,
                    // page is likely still loading or already loaded.
                    crate::cdp::commit_cdp(&cdp_client, |c| c.invalidate_snapshots()).await;
                    CallToolResult::success(vec![Content::text(format!(
                        "Navigated to {} (page may still be loading)",
                        target_url
                    ))])
                }
            }
        }
        "reload" => match page.execute(ReloadParams::default()).await {
            Ok(_) => {
                crate::cdp::commit_cdp(&cdp_client, |c| c.invalidate_snapshots()).await;
                CallToolResult::success(vec![Content::text("Page reloaded")])
            }
            Err(e) => cdp_error(format!("Reload failed: {}", e)),
        },
        "back" | "forward" => {
            let history = match page.execute(GetNavigationHistoryParams::default()).await {
                Ok(r) => r.result,
                Err(e) => return cdp_error(format!("Failed to get navigation history: {}", e)),
            };

            let target_idx = if action == "back" {
                history.current_index - 1
            } else {
                history.current_index + 1
            };

            if target_idx < 0 || target_idx as usize >= history.entries.len() {
                return cdp_error(format!("No {} history entry available.", action));
            }

            let entry = &history.entries[target_idx as usize];
            let entry_id = entry.id;
            let entry_url = entry.url.clone();

            match page
                .execute(NavigateToHistoryEntryParams::new(entry_id))
                .await
            {
                Ok(_) => {
                    crate::cdp::commit_cdp(&cdp_client, |c| c.invalidate_snapshots()).await;
                    CallToolResult::success(vec![Content::text(format!(
                        "Navigated {}: {}",
                        action, entry_url
                    ))])
                }
                Err(e) => cdp_error(format!("Navigation {} failed: {}", action, e)),
            }
        }
        _ => cdp_error(format!(
            "Invalid navigation type '{}'. Use 'url', 'back', 'forward', or 'reload'.",
            action
        )),
    }
}

pub async fn cdp_new_page(
    url: String,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    // `browser` isn't cloneable, so hold a read lock only across the single
    // `new_page` RPC, then release before the URL fetch + commit.
    let page = {
        let guard = cdp_client.read().await;
        let client = match guard.as_ref() {
            Some(c) => c,
            None => return cdp_error("No CDP connection. Use cdp_connect first."),
        };
        match client.browser.new_page(&url).await {
            Ok(p) => p,
            Err(e) => return cdp_error(format!("Failed to create new page: {}", e)),
        }
    };

    let page_url = page_url(&page).await;
    crate::cdp::commit_cdp(&cdp_client, |c| {
        c.selected_page = Some(page);
        c.invalidate_snapshots();
    })
    .await;

    CallToolResult::success(vec![Content::text(format!(
        "Created and selected new page: {}",
        page_url
    ))])
}

pub async fn cdp_close_page(
    page_idx: usize,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    // Deliberately holds the write lock for the whole operation: the awaits
    // here are single bounded RPCs (url/close/bring_to_front), and the
    // `last_page_list` removal + replacement-selection must stay atomic —
    // splitting it would race the index against a concurrent list/select.
    let mut guard = cdp_client.write().await;
    let client = match guard.as_mut() {
        Some(c) => c,
        None => return cdp_error("No CDP connection. Use cdp_connect first."),
    };

    if client.last_page_list.is_empty() {
        return cdp_error("No page list available. Call cdp_list_pages first.");
    }

    if client.last_page_list.len() <= 1 {
        return cdp_error("Cannot close the last open page.");
    }

    if page_idx >= client.last_page_list.len() {
        return cdp_error(format!(
            "Page index {} is out of range (0..{}). Call cdp_list_pages to refresh.",
            page_idx,
            client.last_page_list.len()
        ));
    }

    let page_to_close = client.last_page_list[page_idx].clone();
    let url = page_to_close.url().await.ok().flatten().unwrap_or_default();

    let is_selected = client
        .selected_page
        .as_ref()
        .is_some_and(|selected| selected.target_id() == page_to_close.target_id());

    if let Err(e) = page_to_close.close().await {
        return cdp_error(format!("Failed to close page [{}]: {}", page_idx, e));
    }

    // Remove from cached list only after successful close.
    client.last_page_list.remove(page_idx);

    // Select the adjacent tab (same index, or previous if we closed the last one).
    // NOTE: CDP doesn't expose tab-strip order — last_page_list comes from
    // browser.pages() which iterates an unordered HashMap. This is a best-effort
    // heuristic; the selected page may not match the browser's visually active tab.
    if is_selected {
        // Invalidate up front: even if there is no replacement page, the
        // snapshots we hold refer to a closed target and must be dropped.
        client.invalidate_snapshots();

        let new_idx = if page_idx < client.last_page_list.len() {
            page_idx
        } else {
            client.last_page_list.len().saturating_sub(1)
        };
        if let Some(replacement) = client.last_page_list.get(new_idx) {
            if replacement.bring_to_front().await.is_ok() {
                client.selected_page = Some(replacement.clone());
            } else {
                client.selected_page = None;
            }
        } else {
            client.selected_page = None;
        }
    }

    CallToolResult::success(vec![Content::text(format!(
        "Closed page [{}]: {}",
        page_idx, url
    ))])
}

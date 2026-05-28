//! Image-source resolution seam for `find_image`.
//!
//! Filled out across steps 1-4:
//! - Step 1 introduces `Caches`, a thin holder that pairs the screenshot
//!   and image caches so the algorithm doesn't need to thread two `Arc`s.
//! - Steps 2-4 add `ImageSource`, `RawImageBytes`, `SourceMeta`, `Resolved`,
//!   `Miss`, `SlotInput`, plus `fetch`/`decode`/`resolve_slot`.

use crate::tools::image_cache::ImageCache;
use crate::tools::screenshot_cache::ScreenshotCache;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Bundle of caches the `find_image` pipeline reads from.
///
/// The two caches use different lock kinds at call sites — screenshot
/// goes through `peek()` under a read lock (no LRU bump), image goes
/// through `get()` under a write lock (LRU bump). Keep that distinction
/// when adding consumers.
#[derive(Clone)]
pub(super) struct Caches {
    pub(super) screenshot: Arc<RwLock<ScreenshotCache>>,
    pub(super) image: Arc<RwLock<ImageCache>>,
}


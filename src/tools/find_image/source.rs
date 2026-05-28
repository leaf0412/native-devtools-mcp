//! Image-source resolution seam for `find_image`.
//!
//! Filled out across steps 1-4:
//! - Step 1 introduces `Caches`, a thin holder that pairs the screenshot
//!   and image caches so the algorithm doesn't need to thread two `Arc`s.
//! - Step 2 adds `ImageSource`, `RawImageBytes`, `SourceMeta`, `Resolved`,
//!   `Miss` and routes the screenshot slot through `fetch`/`decode`.
//! - Steps 3-4 add `SlotInput::resolve_slot` and route template + mask.

use crate::tools::find_image::transform::{decode_base64_to_gray, decode_png_to_gray};
use crate::tools::image_cache::ImageCache;
use crate::tools::screenshot_cache::{ScreenshotCache, ScreenshotMetadata};
use image::GrayImage;
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

/// Where an image for the matching pipeline can come from.
///
/// `Screenshot` and `Image` carry the cache id; the corresponding cache
/// is read under the lock kind documented on [`Caches`].
#[allow(dead_code)] // Image variant lights up in step 3
pub(super) enum ImageSource {
    Base64(String),
    InlinePng(Vec<u8>),
    Screenshot(String),
    Image(String),
}

/// Raw bytes a source produced, still encoded — `decode()` turns them
/// into a `GrayImage` on the blocking thread.
pub(super) enum RawImageBytes {
    Png(Vec<u8>),
    Base64(String),
}

impl RawImageBytes {
    /// Decode to grayscale. Pure CPU — call from inside `spawn_blocking`.
    pub(super) fn decode(&self) -> Result<GrayImage, String> {
        match self {
            RawImageBytes::Png(bytes) => decode_png_to_gray(bytes),
            RawImageBytes::Base64(b64) => decode_base64_to_gray(b64),
        }
    }
}

/// Side-band metadata a source may carry. Only the screenshot cache
/// produces meaningful values today; everything else uses `default()`.
#[derive(Default)]
pub(super) struct SourceMeta {
    pub(super) screenshot: Option<ScreenshotMetadata>,
}

/// Successful outcome of `ImageSource::fetch`.
pub(super) struct Resolved {
    pub(super) bytes: RawImageBytes,
    pub(super) meta: SourceMeta,
}

/// Cache-miss outcome. `Error` is a hard stop; `FallbackWithWarning`
/// asks the caller to warn and retry with the embedded source.
///
/// Step 2 only constructs `Error` (for the screenshot slot, which the
/// algorithm intentionally converts back into a warning + no-data).
/// `FallbackWithWarning` lights up in steps 3-4 via `SlotInput`.
#[allow(dead_code)]
pub(super) enum Miss {
    Error(String),
    FallbackWithWarning(String, ImageSource),
}

impl ImageSource {
    /// Resolve a source to raw bytes, taking the right lock for the
    /// underlying cache. Does NOT decode — that runs in `decode()` on
    /// the blocking thread.
    pub(super) async fn fetch(&self, caches: &Caches) -> Result<Resolved, Miss> {
        match self {
            ImageSource::Base64(b64) => Ok(Resolved {
                bytes: RawImageBytes::Base64(b64.clone()),
                meta: SourceMeta::default(),
            }),
            ImageSource::InlinePng(bytes) => Ok(Resolved {
                bytes: RawImageBytes::Png(bytes.clone()),
                meta: SourceMeta::default(),
            }),
            ImageSource::Screenshot(id) => {
                // READ lock + peek(): no LRU bump on screenshot cache.
                let guard = caches.screenshot.read().await;
                match guard.peek(id) {
                    Some(cached) => Ok(Resolved {
                        bytes: RawImageBytes::Png(cached.png_data.clone()),
                        meta: SourceMeta {
                            screenshot: Some(cached.metadata.clone()),
                        },
                    }),
                    None => Err(Miss::Error(format!(
                        "Screenshot ID '{}' not found in cache",
                        id
                    ))),
                }
            }
            ImageSource::Image(id) => {
                // WRITE lock + get(): bumps LRU on the image cache.
                let mut guard = caches.image.write().await;
                match guard.get(id) {
                    Some(cached) => Ok(Resolved {
                        bytes: RawImageBytes::Png(cached.png_data.clone()),
                        meta: SourceMeta::default(),
                    }),
                    None => Err(Miss::Error(format!(
                        "Image ID '{}' not found in image cache",
                        id
                    ))),
                }
            }
        }
    }
}


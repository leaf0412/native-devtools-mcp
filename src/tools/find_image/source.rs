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
#[allow(dead_code)] // InlinePng is a deliberate seam point, not yet exercised
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
/// asks the caller to warn and use the embedded fallback source.
#[allow(dead_code)] // FallbackWithWarning is reserved for callers that
                    // want fetch itself to bundle the fallback source;
                    // current callers route fallback through resolve_slot.
pub(super) enum Miss {
    Error(String),
    FallbackWithWarning(String, ImageSource),
}

/// Slot wiring for `resolve_slot`: a primary source (typically an
/// `Image(id)`) plus an optional base64 fallback the caller wants
/// used when the primary id misses the cache.
pub(super) struct SlotInput {
    pub(super) primary: Option<ImageSource>,
    pub(super) fallback_b64: Option<ImageSource>,
}

/// Names a slot for the warn / hard-error message templates.
///
/// Literals chosen to match the pre-seam strings byte-for-byte:
///   warn:  "{Label} ID '{id}' not found in cache, using base64 fallback"
///   error: "{Label} ID '{id}' not found in image cache"
#[derive(Clone, Copy)]
#[allow(dead_code)] // Mask variant lights up in step 4
pub(super) enum SlotKind {
    Template,
    Mask,
}

impl SlotKind {
    fn label(self) -> &'static str {
        match self {
            SlotKind::Template => "Template",
            SlotKind::Mask => "Mask",
        }
    }
}

/// Resolve a slot's bytes per the standard cache-miss policy:
///   - no primary, no fallback → `Ok(None, None)`
///   - primary resolves → use it
///   - primary misses + fallback present → warn + use fallback
///   - primary misses + no fallback → hard error (caller surfaces it)
///
/// Returns the resolved bytes (if any) and a warning string the caller
/// must merge into its rolling warning. The hard-error path returns
/// `Err(msg)` for the caller to wrap in `CallToolResult::error`.
pub(super) async fn resolve_slot(
    slot: SlotInput,
    kind: SlotKind,
    caches: &Caches,
) -> Result<(Option<Resolved>, Option<String>), String> {
    let Some(primary) = slot.primary else {
        // No id: just take the fallback if any (no warning, no error).
        return match slot.fallback_b64 {
            Some(src) => match src.fetch(caches).await {
                Ok(resolved) => Ok((Some(resolved), None)),
                Err(Miss::Error(e)) => Err(e),
                Err(Miss::FallbackWithWarning(_, _)) => unreachable!(
                    "Base64/InlinePng fetch never produces FallbackWithWarning"
                ),
            },
            None => Ok((None, None)),
        };
    };

    match primary.fetch(caches).await {
        Ok(resolved) => Ok((Some(resolved), None)),
        Err(Miss::Error(_raw)) => {
            // Re-derive the slot-flavored miss message; `_raw` carried
            // the generic "Image ID ... not found in image cache" form,
            // but the public-facing literals are slot-specific.
            let id = primary.id_for_log();
            match slot.fallback_b64 {
                Some(fb) => {
                    let warning = format!(
                        "{} ID '{}' not found in cache, using base64 fallback",
                        kind.label(),
                        id,
                    );
                    match fb.fetch(caches).await {
                        Ok(resolved) => Ok((Some(resolved), Some(warning))),
                        Err(Miss::Error(e)) => Err(e),
                        Err(Miss::FallbackWithWarning(_, _)) => unreachable!(
                            "Base64/InlinePng fetch never produces FallbackWithWarning"
                        ),
                    }
                }
                None => Err(format!(
                    "{} ID '{}' not found in image cache",
                    kind.label(),
                    id,
                )),
            }
        }
        Err(Miss::FallbackWithWarning(_, _)) => unreachable!(
            "ImageSource::fetch never produces FallbackWithWarning on its own"
        ),
    }
}

impl ImageSource {
    /// Best-effort id string for log/warning text.
    fn id_for_log(&self) -> &str {
        match self {
            ImageSource::Screenshot(id) | ImageSource::Image(id) => id.as_str(),
            ImageSource::Base64(_) | ImageSource::InlinePng(_) => "<inline>",
        }
    }
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


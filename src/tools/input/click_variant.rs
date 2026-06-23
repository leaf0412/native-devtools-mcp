//! Coordinate-variant selection and error messaging for the `click` tool.
//!
//! The `click` schema exposes all variant fields as optional top-level
//! properties (Anthropic's tool-use API rejects top-level `oneOf`), so the
//! "exactly one variant" contract is enforced here at runtime.

use super::click::ClickParams;

/// Identifier for the one concrete coordinate variant resolved for a click.
///
/// The schema exposes all variant fields as optional top-level properties
/// (required because Anthropic's tool-use API rejects top-level `oneOf`).
/// [`select_click_variant`] enforces the "exactly one variant" contract at
/// runtime from the submitted params.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClickVariant {
    /// Screen-space (x, y).
    Screen,
    /// Window-relative (window_x, window_y, window_id).
    WindowRelative,
    /// Screenshot image pixels + origin/scale metadata (preferred).
    ScreenshotPixels,
    /// Legacy screenshot pixels (screenshot_x, screenshot_y, screenshot_window_id).
    ScreenshotPixelsLegacy,
}

impl ClickVariant {
    fn title(self) -> &'static str {
        match self {
            ClickVariant::Screen => "screen",
            ClickVariant::WindowRelative => "window-relative",
            ClickVariant::ScreenshotPixels => "screenshot-pixels",
            ClickVariant::ScreenshotPixelsLegacy => "screenshot-pixels-legacy",
        }
    }
}

/// Select exactly one coordinate variant from the submitted params. Returns
/// the matched variant or a descriptive error message naming what went wrong.
///
/// A valid call has every required field of exactly one variant set and no
/// field from any other variant set. The schema cannot enforce this (top-level
/// `oneOf` is rejected by Anthropic's tool-use API), so this runtime check is
/// the single source of truth: a payload mixing `x`/`y` with screenshot fields
/// fails fast with a clear message instead of silently picking a branch.
///
/// Pure function — no I/O.
pub(super) fn select_click_variant(params: &ClickParams) -> Result<ClickVariant, String> {
    // Fields of every variant, in their declared order.
    const SCREEN_FIELDS: &[&str] = &["x", "y"];
    const WINDOW_FIELDS: &[&str] = &["window_x", "window_y", "window_id"];
    const SCREENSHOT_PIXELS_FIELDS: &[&str] = &[
        "screenshot_x",
        "screenshot_y",
        "screenshot_origin_x",
        "screenshot_origin_y",
        "screenshot_scale",
    ];
    const LEGACY_FIELDS: &[&str] = &["screenshot_x", "screenshot_y", "screenshot_window_id"];

    let screen_present = [params.x.is_some(), params.y.is_some()];
    let window_present = [
        params.window_x.is_some(),
        params.window_y.is_some(),
        params.window_id.is_some(),
    ];
    let pixels_present = [
        params.screenshot_x.is_some(),
        params.screenshot_y.is_some(),
        params.screenshot_origin_x.is_some(),
        params.screenshot_origin_y.is_some(),
        params.screenshot_scale.is_some(),
    ];
    let legacy_present = [
        params.screenshot_x.is_some(),
        params.screenshot_y.is_some(),
        params.screenshot_window_id.is_some(),
    ];

    // A variant "has activity" if any of its unique fields are present; this
    // determines which variants the caller was trying to use so we can
    // reject mixes.
    let screen_active = params.x.is_some() || params.y.is_some();
    let window_active =
        params.window_x.is_some() || params.window_y.is_some() || params.window_id.is_some();
    // The two screenshot variants overlap on (screenshot_x, screenshot_y),
    // so disambiguate by their unique fields.
    let pixels_unique_active = params.screenshot_origin_x.is_some()
        || params.screenshot_origin_y.is_some()
        || params.screenshot_scale.is_some();
    let legacy_unique_active = params.screenshot_window_id.is_some();
    let any_screenshot_coord = params.screenshot_x.is_some() || params.screenshot_y.is_some();

    let screenshot_active = pixels_unique_active || legacy_unique_active || any_screenshot_coord;

    let active_count = [screen_active, window_active, screenshot_active]
        .iter()
        .filter(|b| **b)
        .count();

    if active_count == 0 {
        return Err(describe_click_coord_error(params));
    }

    // Multiple families active: a true mixed payload. Describe exactly which
    // families the caller is mixing so they know *what* to remove, not just
    // which variant they were closest to. `describe_click_coord_error`'s
    // "closest variant / missing: ..." shape is misleading here because it
    // can report `missing: (none)` when one of the mixed variants is
    // already complete.
    if active_count > 1 {
        let mut active: Vec<&str> = Vec::new();
        if screen_active {
            active.push("screen (x, y)");
        }
        if window_active {
            active.push("window-relative (window_x, window_y, window_id)");
        }
        if screenshot_active {
            active
                .push("screenshot (screenshot_x/y + origin/scale, or legacy screenshot_window_id)");
        }
        return Err(format!(
            "click received fields from multiple coordinate variants: {}. \
             Send exactly one variant. Prefer screenshot-pixels after take_screenshot: \
             screenshot_x, screenshot_y, screenshot_origin_x, screenshot_origin_y, \
             screenshot_scale.",
            active.join(" + ")
        ));
    }

    // Exactly one family is active — now require its fields to be complete.
    if screen_active {
        if screen_present.iter().all(|p| *p) {
            return Ok(ClickVariant::Screen);
        }
        return Err(format_missing_fields_error(
            ClickVariant::Screen,
            SCREEN_FIELDS,
            &screen_present,
        ));
    }
    if window_active {
        if window_present.iter().all(|p| *p) {
            return Ok(ClickVariant::WindowRelative);
        }
        return Err(format_missing_fields_error(
            ClickVariant::WindowRelative,
            WINDOW_FIELDS,
            &window_present,
        ));
    }
    // Screenshot family: pick the variant the caller clearly intended.
    if pixels_unique_active && legacy_unique_active {
        return Err(
            "click received screenshot_origin_*/screenshot_scale (screenshot-pixels variant) \
             together with screenshot_window_id (screenshot-pixels-legacy variant). \
             Send exactly one. Prefer screenshot-pixels: screenshot_x, screenshot_y, \
             screenshot_origin_x, screenshot_origin_y, screenshot_scale."
                .to_string(),
        );
    }
    if pixels_unique_active {
        if pixels_present.iter().all(|p| *p) {
            return Ok(ClickVariant::ScreenshotPixels);
        }
        return Err(format_missing_fields_error(
            ClickVariant::ScreenshotPixels,
            SCREENSHOT_PIXELS_FIELDS,
            &pixels_present,
        ));
    }
    if legacy_unique_active {
        if legacy_present.iter().all(|p| *p) {
            return Ok(ClickVariant::ScreenshotPixelsLegacy);
        }
        return Err(format_missing_fields_error(
            ClickVariant::ScreenshotPixelsLegacy,
            LEGACY_FIELDS,
            &legacy_present,
        ));
    }
    // Only screenshot_x and/or screenshot_y provided — ambiguous between the
    // two screenshot variants. Default to the preferred one for the error.
    Err(format_missing_fields_error(
        ClickVariant::ScreenshotPixels,
        SCREENSHOT_PIXELS_FIELDS,
        &pixels_present,
    ))
}

fn format_missing_fields_error(variant: ClickVariant, fields: &[&str], present: &[bool]) -> String {
    debug_assert_eq!(fields.len(), present.len());
    let missing: Vec<&str> = fields
        .iter()
        .zip(present.iter())
        .filter_map(|(n, p)| if *p { None } else { Some(*n) })
        .collect();
    format!(
        "click variant '{}' is missing required fields: {}. \
         Send every field of exactly one variant, and no fields from other variants.",
        variant.title(),
        missing.join(", ")
    )
}

/// Build a user-facing error message explaining why the provided click params
/// don't match any supported coordinate variant. Names the fields that were
/// provided and suggests the closest variant based on heuristic overlap.
///
/// Pure function — used by `click` when no variant validates successfully.
pub(super) fn describe_click_coord_error(params: &ClickParams) -> String {
    // (variant_name, [(field_name, is_provided), ...])
    // Order matters: the first listed variant wins ties, which is why
    // "screenshot-pixels" is first — it's the preferred variant and a
    // natural target to steer callers toward.
    let variants: [(&str, &[(&str, bool)]); 4] = [
        (
            "screenshot-pixels",
            &[
                ("screenshot_x", params.screenshot_x.is_some()),
                ("screenshot_y", params.screenshot_y.is_some()),
                ("screenshot_origin_x", params.screenshot_origin_x.is_some()),
                ("screenshot_origin_y", params.screenshot_origin_y.is_some()),
                ("screenshot_scale", params.screenshot_scale.is_some()),
            ],
        ),
        (
            "screen",
            &[("x", params.x.is_some()), ("y", params.y.is_some())],
        ),
        (
            "window-relative",
            &[
                ("window_x", params.window_x.is_some()),
                ("window_y", params.window_y.is_some()),
                ("window_id", params.window_id.is_some()),
            ],
        ),
        (
            "screenshot-pixels-legacy",
            &[
                ("screenshot_x", params.screenshot_x.is_some()),
                ("screenshot_y", params.screenshot_y.is_some()),
                (
                    "screenshot_window_id",
                    params.screenshot_window_id.is_some(),
                ),
            ],
        ),
    ];

    // Every field in the union, preserving source order for a stable message.
    let all_fields: &[(&str, bool)] = &[
        ("x", params.x.is_some()),
        ("y", params.y.is_some()),
        ("window_x", params.window_x.is_some()),
        ("window_y", params.window_y.is_some()),
        ("window_id", params.window_id.is_some()),
        ("screenshot_x", params.screenshot_x.is_some()),
        ("screenshot_y", params.screenshot_y.is_some()),
        ("screenshot_origin_x", params.screenshot_origin_x.is_some()),
        ("screenshot_origin_y", params.screenshot_origin_y.is_some()),
        ("screenshot_scale", params.screenshot_scale.is_some()),
        (
            "screenshot_window_id",
            params.screenshot_window_id.is_some(),
        ),
    ];

    let provided: Vec<&str> = all_fields
        .iter()
        .filter_map(|(n, p)| if *p { Some(*n) } else { None })
        .collect();

    // Closest variant = highest count of provided fields.
    // Ties are broken in favor of earlier variants (so "screenshot-pixels"
    // wins ties as the preferred form). std's `max_by_key` returns the last
    // maximal element, so we invert with `min_by_key` on a (negative_score,
    // index) tuple to pick the earliest best.
    let (closest_variant, fields) = variants
        .iter()
        .enumerate()
        .min_by_key(|(idx, (_, fields))| {
            let score = fields.iter().filter(|(_, p)| *p).count() as isize;
            (-score, *idx)
        })
        .map(|(_, v)| *v)
        .expect("variants table is non-empty");

    let missing: Vec<&str> = fields
        .iter()
        .filter_map(|(n, p)| if *p { None } else { Some(*n) })
        .collect();

    let provided_str = if provided.is_empty() {
        "(no coordinate fields)".to_string()
    } else {
        provided.join(", ")
    };
    let missing_str = if missing.is_empty() {
        "(none)".to_string()
    } else {
        missing.join(", ")
    };

    format!(
        "click requires exactly one complete coordinate variant. \
         Provided fields: {provided_str}. \
         Closest variant: '{closest_variant}' — missing: {missing_str}.\n\
         Supported variants:\n\
         - screenshot-pixels (PREFERRED after take_screenshot): \
           screenshot_x, screenshot_y, screenshot_origin_x, screenshot_origin_y, screenshot_scale\n\
         - screen: x, y\n\
         - window-relative: window_x, window_y, window_id\n\
         - screenshot-pixels-legacy (deprecated): \
           screenshot_x, screenshot_y, screenshot_window_id"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_click_params() -> ClickParams {
        ClickParams {
            x: None,
            y: None,
            window_x: None,
            window_y: None,
            window_id: None,
            screenshot_x: None,
            screenshot_y: None,
            screenshot_origin_x: None,
            screenshot_origin_y: None,
            screenshot_scale: None,
            screenshot_window_id: None,
            button: None,
            click_count: 1,
            include_snapshot: false,
            app_name: None,
            background: false,
        }
    }

    #[test]
    fn test_click_error_reports_provided_fields_for_partial_screenshot_pixels() {
        // Caller sent only screenshot_x/y — no origin/scale, no window_id.
        // Closest variant should be screenshot-pixels (highest overlap).
        let mut p = empty_click_params();
        p.screenshot_x = Some(10.0);
        p.screenshot_y = Some(20.0);

        let msg = describe_click_coord_error(&p);
        assert!(msg.contains("screenshot_x"), "msg: {msg}");
        assert!(msg.contains("screenshot_y"), "msg: {msg}");
        assert!(
            msg.contains("'screenshot-pixels'"),
            "closest variant should be screenshot-pixels: {msg}"
        );
        // Missing fields are named.
        assert!(msg.contains("screenshot_origin_x"), "msg: {msg}");
        assert!(msg.contains("screenshot_scale"), "msg: {msg}");
    }

    #[test]
    fn test_click_error_reports_closest_screen_variant_when_only_x_set() {
        let mut p = empty_click_params();
        p.x = Some(100.0);

        let msg = describe_click_coord_error(&p);
        assert!(msg.contains("'screen'"), "msg: {msg}");
        assert!(msg.contains("missing: y"), "msg: {msg}");
    }

    #[test]
    fn test_click_error_reports_closest_window_relative_variant() {
        let mut p = empty_click_params();
        p.window_x = Some(10.0);
        p.window_y = Some(20.0);

        let msg = describe_click_coord_error(&p);
        assert!(msg.contains("'window-relative'"), "msg: {msg}");
        assert!(msg.contains("window_id"), "msg: {msg}");
    }

    #[test]
    fn test_click_error_for_empty_params_names_no_provided_fields() {
        let p = empty_click_params();
        let msg = describe_click_coord_error(&p);
        assert!(msg.contains("(no coordinate fields)"), "msg: {msg}");
    }

    // MARK: - select_click_variant (oneOf runtime enforcement)

    #[test]
    fn test_select_variant_picks_screen_for_pure_x_y() {
        let mut p = empty_click_params();
        p.x = Some(100.0);
        p.y = Some(200.0);
        assert_eq!(select_click_variant(&p), Ok(ClickVariant::Screen));
    }

    #[test]
    fn test_select_variant_picks_screenshot_pixels_for_full_payload() {
        let mut p = empty_click_params();
        p.screenshot_x = Some(10.0);
        p.screenshot_y = Some(20.0);
        p.screenshot_origin_x = Some(100.0);
        p.screenshot_origin_y = Some(200.0);
        p.screenshot_scale = Some(2.0);
        assert_eq!(select_click_variant(&p), Ok(ClickVariant::ScreenshotPixels));
    }

    #[test]
    fn test_select_variant_picks_window_relative() {
        let mut p = empty_click_params();
        p.window_x = Some(10.0);
        p.window_y = Some(20.0);
        p.window_id = Some(42);
        assert_eq!(select_click_variant(&p), Ok(ClickVariant::WindowRelative));
    }

    #[test]
    fn test_select_variant_picks_legacy_screenshot() {
        let mut p = empty_click_params();
        p.screenshot_x = Some(10.0);
        p.screenshot_y = Some(20.0);
        p.screenshot_window_id = Some(42);
        assert_eq!(
            select_click_variant(&p),
            Ok(ClickVariant::ScreenshotPixelsLegacy)
        );
    }

    #[test]
    fn test_select_variant_rejects_mixed_screen_and_screenshot_pixels() {
        // This was the silent-pick bug: the old flat resolver would pick
        // `screen` as the first complete branch and click at (x,y) even
        // though the caller also supplied full screenshot-pixels fields.
        let mut p = empty_click_params();
        p.x = Some(100.0);
        p.y = Some(200.0);
        p.screenshot_x = Some(10.0);
        p.screenshot_y = Some(20.0);
        p.screenshot_origin_x = Some(100.0);
        p.screenshot_origin_y = Some(200.0);
        p.screenshot_scale = Some(2.0);

        let err = select_click_variant(&p).unwrap_err();
        // The message must explicitly flag the mix (naming both families),
        // not just report a "closest variant" with missing: (none).
        assert!(
            err.contains("multiple coordinate variants"),
            "err should flag the mix, got: {err}"
        );
        assert!(err.contains("screen"), "err: {err}");
        assert!(err.contains("screenshot"), "err: {err}");
        assert!(!err.contains("missing: (none)"), "err: {err}");
    }

    #[test]
    fn test_select_variant_rejects_mixed_screen_and_window_relative() {
        let mut p = empty_click_params();
        p.x = Some(100.0);
        p.y = Some(200.0);
        p.window_x = Some(10.0);
        p.window_y = Some(20.0);
        p.window_id = Some(42);

        let err = select_click_variant(&p).unwrap_err();
        assert!(
            err.contains("multiple coordinate variants"),
            "err should flag the mix, got: {err}"
        );
        assert!(err.contains("screen"), "err: {err}");
        assert!(err.contains("window-relative"), "err: {err}");
    }

    #[test]
    fn test_select_variant_rejects_mixed_screenshot_pixels_and_legacy() {
        // Supplying both the origin+scale set AND screenshot_window_id is
        // ambiguous and must be rejected.
        let mut p = empty_click_params();
        p.screenshot_x = Some(10.0);
        p.screenshot_y = Some(20.0);
        p.screenshot_origin_x = Some(100.0);
        p.screenshot_origin_y = Some(200.0);
        p.screenshot_scale = Some(2.0);
        p.screenshot_window_id = Some(7);

        let err = select_click_variant(&p).unwrap_err();
        assert!(err.contains("screenshot-pixels"), "err: {err}");
    }

    #[test]
    fn test_select_variant_rejects_partial_screen() {
        // `x` without `y` is invalid.
        let mut p = empty_click_params();
        p.x = Some(100.0);

        let err = select_click_variant(&p).unwrap_err();
        assert!(
            err.contains("'screen'") || err.contains("screen"),
            "err: {err}"
        );
        assert!(err.contains("y"), "err: {err}");
    }

    #[test]
    fn test_select_variant_rejects_empty_params() {
        let p = empty_click_params();
        assert!(select_click_variant(&p).is_err());
    }
}

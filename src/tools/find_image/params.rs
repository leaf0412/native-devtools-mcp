//! Parameter structs and mode defaults for the `find_image` tool.
//!
//! This file owns: request/response types, the `ModeDefaults` policy table,
//! and pure validators for rotations and scale ranges.

use serde::{Deserialize, Serialize};

/// Parameters for the find_image tool.
#[derive(Debug, Deserialize)]
pub struct FindImageParams {
    /// Screenshot ID from a previous take_screenshot call.
    pub screenshot_id: Option<String>,

    /// Base64-encoded screenshot image (used if no screenshot_id).
    pub screenshot_image_base64: Option<String>,

    /// Image ID from a previous load_image call (preferred over template_image_base64).
    pub template_id: Option<String>,

    /// Base64-encoded template image to find (used if no template_id).
    pub template_image_base64: Option<String>,

    /// Image ID from a previous load_image call for the mask.
    pub mask_id: Option<String>,

    /// Base64-encoded mask image (optional; white=match, black=ignore).
    pub mask_image_base64: Option<String>,

    /// Mode: "fast" (default) or "accurate".
    #[serde(default = "default_mode")]
    pub mode: String,

    /// Minimum match score threshold (default depends on mode).
    pub threshold: Option<f64>,

    /// Maximum number of results to return (default depends on mode).
    pub max_results: Option<usize>,

    /// Scale search range: {min, max, step}.
    pub scales: Option<ScaleRange>,

    /// Rotations to try in degrees. Only 0, 90, 180, 270 are supported.
    pub rotations: Option<Vec<f64>>,

    /// Search region within the screenshot: {x, y, w, h}.
    pub search_region: Option<SearchRegion>,

    /// Stride for matching (default: 2 in fast mode, 1 in accurate mode).
    pub stride: Option<u32>,

    /// Return screen coordinates (requires screenshot metadata).
    #[serde(default = "default_return_screen_coords")]
    pub return_screen_coords: bool,
}

fn default_mode() -> String {
    "fast".to_string()
}

fn default_return_screen_coords() -> bool {
    true
}

/// Scale search range configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScaleRange {
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

impl Default for ScaleRange {
    fn default() -> Self {
        Self {
            min: 0.8,
            max: 1.2,
            step: 0.1,
        }
    }
}

/// Search region within the screenshot.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchRegion {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// A single match result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchResult {
    /// Match confidence score (0.0 to 1.0).
    pub score: f64,
    /// Bounding box in screenshot pixels.
    pub bbox: BoundingBox,
    /// Center point in screenshot pixels.
    pub center: Point,
    /// Scale at which the match was found.
    pub scale: f64,
    /// Rotation at which the match was found (degrees).
    pub rotation: f64,
    /// Screen X coordinate (if metadata available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_x: Option<f64>,
    /// Screen Y coordinate (if metadata available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_y: Option<f64>,
}

/// Bounding box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Point coordinate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Response from find_image tool.
#[derive(Debug, Serialize, Deserialize)]
pub struct FindImageResponse {
    pub matches: Vec<MatchResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Mode-specific defaults.
pub(super) struct ModeDefaults {
    pub(super) threshold: f64,
    pub(super) max_results: usize,
    pub(super) scales: ScaleRange,
    pub(super) stride: u32,
}

impl ModeDefaults {
    fn fast() -> Self {
        Self {
            threshold: 0.75,
            max_results: 3,
            scales: ScaleRange {
                min: 0.8,
                max: 1.2,
                step: 0.1,
            },
            stride: 2,
        }
    }

    fn accurate() -> Self {
        Self {
            threshold: 0.75,
            max_results: 5,
            scales: ScaleRange {
                min: 0.5,
                max: 2.0,
                step: 0.05,
            },
            stride: 1,
        }
    }

    pub(super) fn for_mode(mode: &str) -> Self {
        match mode {
            "accurate" => Self::accurate(),
            _ => Self::fast(),
        }
    }
}

/// Normalize a rotation angle to one of the supported values (0, 90, 180, 270).
/// Returns None if the angle is not within ±1° tolerance of a supported value.
pub(super) fn normalize_rotation(r: f64) -> Option<f64> {
    let normalized = ((r % 360.0) + 360.0) % 360.0;
    // ±1° tolerance (inclusive)
    if normalized <= 1.0 || normalized >= 359.0 {
        Some(0.0)
    } else if (normalized - 90.0).abs() <= 1.0 {
        Some(90.0)
    } else if (normalized - 180.0).abs() <= 1.0 {
        Some(180.0)
    } else if (normalized - 270.0).abs() <= 1.0 {
        Some(270.0)
    } else {
        None
    }
}

/// Validate scale range parameters.
/// Returns Ok(()) if valid, Err(message) if invalid.
pub(super) fn validate_scale_range(scales: &ScaleRange) -> Result<(), String> {
    if scales.step <= 0.0 {
        return Err("scales.step must be positive (got 0 or negative)".to_string());
    }
    if scales.min <= 0.0 {
        return Err(format!("scales.min must be positive (got {})", scales.min));
    }
    if scales.max <= 0.0 {
        return Err(format!("scales.max must be positive (got {})", scales.max));
    }
    if scales.min > scales.max {
        return Err(format!(
            "scales.min ({}) must not exceed scales.max ({})",
            scales.min, scales.max
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rotation_normalization_wrapping() {
        // Values that wrap around 360
        assert_eq!(normalize_rotation(360.0), Some(0.0));
        assert_eq!(normalize_rotation(450.0), Some(90.0));
        assert_eq!(normalize_rotation(540.0), Some(180.0));
        assert_eq!(normalize_rotation(-90.0), Some(270.0));
        assert_eq!(normalize_rotation(-270.0), Some(90.0));
    }

    #[test]
    fn test_scale_validation_step_must_be_positive() {
        // Zero step
        let result = validate_scale_range(&ScaleRange {
            min: 0.8,
            max: 1.2,
            step: 0.0,
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("step"));

        // Negative step
        let result = validate_scale_range(&ScaleRange {
            min: 0.8,
            max: 1.2,
            step: -0.1,
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("step"));
    }

    #[test]
    fn test_scale_validation_min_must_be_positive() {
        // Zero min
        let result = validate_scale_range(&ScaleRange {
            min: 0.0,
            max: 1.2,
            step: 0.1,
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("min"));

        // Negative min
        let result = validate_scale_range(&ScaleRange {
            min: -0.5,
            max: 1.2,
            step: 0.1,
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("min"));
    }

    #[test]
    fn test_scale_validation_max_must_be_positive() {
        // Zero max
        let result = validate_scale_range(&ScaleRange {
            min: 0.5,
            max: 0.0,
            step: 0.1,
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max"));

        // Negative max
        let result = validate_scale_range(&ScaleRange {
            min: 0.5,
            max: -1.0,
            step: 0.1,
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max"));
    }

    #[test]
    fn test_scale_validation_min_not_exceed_max() {
        let result = validate_scale_range(&ScaleRange {
            min: 2.0,
            max: 0.5,
            step: 0.1,
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must not exceed"));
    }
}

//! Non-maximum suppression for collapsing overlapping `find_image` detections.

use crate::tools::find_image::params::{BoundingBox, MatchResult};

/// Non-Maximum Suppression to remove overlapping detections.
pub(super) fn non_maximum_suppression(
    mut matches: Vec<MatchResult>,
    iou_threshold: f64,
    max_results: usize,
) -> Vec<MatchResult> {
    // Sort by score descending
    matches.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut keep = Vec::new();

    while !matches.is_empty() && keep.len() < max_results {
        let best = matches.remove(0);

        // Remove all matches that overlap too much with the best
        matches.retain(|m| compute_iou(&best.bbox, &m.bbox) < iou_threshold);

        keep.push(best);
    }

    keep
}

/// Compute Intersection over Union of two bounding boxes.
fn compute_iou(a: &BoundingBox, b: &BoundingBox) -> f64 {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.w).min(b.x + b.w);
    let y2 = (a.y + a.h).min(b.y + b.h);

    if x2 <= x1 || y2 <= y1 {
        return 0.0;
    }

    let intersection = (x2 - x1) as f64 * (y2 - y1) as f64;
    let area_a = a.w as f64 * a.h as f64;
    let area_b = b.w as f64 * b.h as f64;
    let union = area_a + area_b - intersection;

    if union < f64::EPSILON {
        return 0.0;
    }

    intersection / union
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::find_image::params::Point;

    #[test]
    fn test_nms() {
        let matches = vec![
            MatchResult {
                score: 0.9,
                bbox: BoundingBox {
                    x: 0,
                    y: 0,
                    w: 10,
                    h: 10,
                },
                center: Point { x: 5.0, y: 5.0 },
                scale: 1.0,
                rotation: 0.0,
                screen_x: None,
                screen_y: None,
            },
            MatchResult {
                score: 0.85,
                bbox: BoundingBox {
                    x: 2,
                    y: 2,
                    w: 10,
                    h: 10,
                },
                center: Point { x: 7.0, y: 7.0 },
                scale: 1.0,
                rotation: 0.0,
                screen_x: None,
                screen_y: None,
            },
            MatchResult {
                score: 0.8,
                bbox: BoundingBox {
                    x: 50,
                    y: 50,
                    w: 10,
                    h: 10,
                },
                center: Point { x: 55.0, y: 55.0 },
                scale: 1.0,
                rotation: 0.0,
                screen_x: None,
                screen_y: None,
            },
        ];

        let result = non_maximum_suppression(matches, 0.3, 5);
        // First two overlap significantly, third doesn't
        assert_eq!(result.len(), 2);
        assert!((result[0].score - 0.9).abs() < f64::EPSILON);
        assert!((result[1].score - 0.8).abs() < f64::EPSILON);
    }
}

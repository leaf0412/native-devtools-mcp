//! AX tree traversal helpers — walk + hit-test.
//!
//! `walk_ax_tree` is the core depth-first visitor used by `find_text`,
//! `list_element_names`, `hit_test_tree`, and (in later steps)
//! `collect_ax_tree_recursive`. `hit_test_tree` + `HitResult` belong here
//! because the geometry decision is taken inside the visitor; the
//! consumer (`find::element_at_point`) just unwraps `HitResult`.

use super::attr;
use super::ffi::AXUIElementRef;
use super::{MAX_DEPTH, MAX_ELEMENTS};
use core_graphics::geometry::{CGPoint, CGSize};

/// Recursively walk the AX element tree and call `visitor` on each element.
///
/// `depth` limits recursion to prevent runaway traversal of deep trees.
///
/// # Safety
/// `element` must be a live, retained `AXUIElementRef`.
pub(super) unsafe fn walk_ax_tree(
    element: AXUIElementRef,
    element_count: &mut usize,
    depth: u32,
    visitor: &mut dyn FnMut(AXUIElementRef),
) {
    // Guard against excessively deep or large trees
    if depth > MAX_DEPTH || *element_count >= MAX_ELEMENTS {
        return;
    }

    *element_count += 1;
    visitor(element);

    // .ok().flatten() collapses Err(Ffi)/Err(Decode) into the legacy
    // "no children, stop walking" path — same shape as the prior
    // get_ax_children wrapper. See ax::attr module docs.
    if let Some(children) = attr::array(element, "AXChildren").ok().flatten() {
        for i in 0..children.len() {
            let child = *children.get_unchecked(i) as AXUIElementRef;
            // Retain the child for the duration of our walk since CFArray
            // only gives a get-rule ref.
            core_foundation::base::CFRetain(child as core_foundation::base::CFTypeRef);
            walk_ax_tree(child, element_count, depth + 1, visitor);
            core_foundation::base::CFRelease(child as core_foundation::base::CFTypeRef);
        }
    }
}

/// Result of a hit-test tree walk — captures all attributes at visit time
/// since AX element references from the walk are borrowed, not owned.
pub(super) struct HitResult {
    pub(super) name: Option<String>,
    pub(super) role: Option<String>,
    pub(super) subrole: Option<String>,
    pub(super) label: Option<String>,
    pub(super) value: Option<String>,
    pub(super) position: CGPoint,
    pub(super) size: CGSize,
    pub(super) area: f64,
}

/// Full tree-walk hit-test: find the smallest-area AX element whose bounds
/// contain (x, y). Walks the entire tree (no spatial pruning) because
/// Electron/Chromium apps may have intermediate containers with inaccurate
/// bounds that don't encompass their children.
///
/// # Safety
/// `root` must be a live, retained `AXUIElementRef`.
pub(super) unsafe fn hit_test_tree(root: AXUIElementRef, x: f64, y: f64) -> Option<HitResult> {
    let mut best: Option<HitResult> = None;
    let mut element_count: usize = 0;

    walk_ax_tree(root, &mut element_count, 0, &mut |element| {
        let pos = match attr::point(element, "AXPosition").ok().flatten() {
            Some(p) => p,
            None => return,
        };
        let size = match attr::size(element, "AXSize").ok().flatten() {
            Some(s) => s,
            None => return,
        };
        if size.width > 0.0
            && size.height > 0.0
            && x >= pos.x
            && x <= pos.x + size.width
            && y >= pos.y
            && y <= pos.y + size.height
        {
            let area = size.width * size.height;
            let is_better = match &best {
                Some(current) => area < current.area,
                None => true,
            };
            if is_better {
                best = Some(HitResult {
                    name: attr::string(element, "AXTitle").ok().flatten(),
                    role: attr::string(element, "AXRole").ok().flatten(),
                    subrole: attr::string(element, "AXSubrole").ok().flatten(),
                    label: attr::string(element, "AXDescription").ok().flatten(),
                    value: attr::string(element, "AXValue").ok().flatten(),
                    position: pos,
                    size,
                    area,
                });
            }
        }
    });

    best
}

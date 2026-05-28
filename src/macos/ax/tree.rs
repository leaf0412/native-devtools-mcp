//! AX tree traversal helpers — walk, hit-test, snapshot, ancestor chain.
//!
//! `walk_ax_tree` is the core depth-first visitor used by `find_text`,
//! `list_element_names`, `hit_test_tree`, and `collect_ax_tree_recursive`.
//! Snapshot collection and bbox/ancestor probes also live here because they
//! are all "navigate the tree, read attributes" — the natural sibling of
//! the walker.

use super::attr;
use super::ffi::{AXUIElementCreateApplication, AXUIElementRef};
use super::AXRef;
use super::{MAX_DEPTH, MAX_ELEMENTS};
use crate::tools::ax_snapshot::{map_ax_role, AXSnapshotNode};
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

/// Read position + size from an AX element and return a `Rect`. Returns
/// `None` when either attribute is unreadable. The raw pointer must refer to
/// a live, retained `AXUIElement`.
///
/// # Safety
/// `element` must be a live, retained `AXUIElementRef`.
pub(crate) unsafe fn element_bbox(
    element: AXUIElementRef,
) -> Option<crate::tools::ax_snapshot::Rect> {
    let pos = attr::point(element, "AXPosition").ok().flatten()?;
    let size = attr::size(element, "AXSize").ok().flatten()?;
    Some(crate::tools::ax_snapshot::Rect {
        x: pos.x,
        y: pos.y,
        w: size.width,
        h: size.height,
    })
}

/// Number of parent links to traverse when hunting for an enclosing role.
/// Deep-enough to absorb the intermediate cells/groups native sidebars wrap
/// their rows in, shallow enough to bail cleanly on a pathological cycle.
const AX_ANCESTOR_WALK_LIMIT: u32 = 32;

/// Walk up the `AXParent` chain starting from `start` (inclusive) and
/// return a leaf-to-root vector of `(AXRef, Option<role>)` pairs.
///
/// The walk stops when `AXParent` becomes unreadable or when
/// `AX_ANCESTOR_WALK_LIMIT` is reached — either way a bounded vector is
/// returned rather than an error. Downstream logic (`ax_select`) inspects
/// the chain to pick out the enclosing `AXRow` and `AXOutline`/`AXTable`;
/// splitting the walk from the decision lets the decision logic be unit
/// tested without live `AXUIElement` handles.
pub(crate) fn ancestor_role_chain(start: &AXRef) -> Vec<(AXRef, Option<String>)> {
    let mut chain: Vec<(AXRef, Option<String>)> = Vec::new();
    let mut current = start.clone();
    for _ in 0..AX_ANCESTOR_WALK_LIMIT {
        let role = attr::string(current.as_raw(), "AXRole").ok().flatten();
        chain.push((current.clone(), role));
        // attr::element collapses Err(Ffi)/Err(Decode) into None alongside
        // the legitimately-absent root case via .ok().flatten() — same
        // shape as the prior ax_parent wrapper, now inlined.
        match attr::element(current.as_raw(), "AXParent").ok().flatten() {
            Some(p) => current = p,
            None => break,
        }
    }
    chain
}

/// Recursively walk the AX element tree and collect [`AXSnapshotNode`]
/// entries plus a `HashMap<uid, AXRef>` of retained handles.
///
/// UIDs are assigned sequentially via `next_uid` (starts at 1). Traversal
/// order is depth-first, matching `walk_ax_tree`.
///
/// # Safety
/// `element` must be a live, retained `AXUIElementRef`.
pub(crate) unsafe fn collect_ax_tree_recursive(
    element: AXUIElementRef,
    element_count: &mut usize,
    depth: u32,
    next_uid: &mut u32,
    nodes: &mut Vec<AXSnapshotNode>,
    refs: &mut std::collections::HashMap<u32, AXRef>,
) {
    if depth > MAX_DEPTH || *element_count >= MAX_ELEMENTS {
        return;
    }

    *element_count += 1;

    let uid = *next_uid;
    *next_uid += 1;

    // Preserved degraded path: AXRole unreadable -> "unknown" role label,
    // matching prior behavior. .ok().flatten() collapses Err(Ffi)/Err(Decode)
    // into None alongside legitimately-absent values; see ax::attr docs.
    let role = attr::string(element, "AXRole")
        .ok()
        .flatten()
        .as_deref()
        .map(map_ax_role)
        .unwrap_or_else(|| "unknown".to_string());

    let name = attr::string(element, "AXTitle").ok().flatten();
    let value = attr::string(element, "AXValue").ok().flatten();
    // Preserved degraded path: AXFocused/AXEnabled unreadable -> false.
    // Matches prior behavior so JSON snapshots are byte-identical.
    let focused = attr::boolean(element, "AXFocused").ok().flatten().unwrap_or(false);
    let disabled = attr::boolean(element, "AXEnabled")
        .ok()
        .flatten()
        .map(|enabled| !enabled)
        .unwrap_or(false);
    let expanded = attr::boolean(element, "AXExpanded").ok().flatten();
    let selected = attr::boolean(element, "AXSelected").ok().flatten();
    let bbox = element_bbox(element);

    nodes.push(AXSnapshotNode {
        uid,
        role,
        name,
        value,
        focused,
        disabled,
        expanded,
        selected,
        depth,
        bbox,
    });

    // Retain under get-rule: the current `element` was obtained either from
    // `AXUIElementCreateApplication` (owned, passed to us) or from the
    // CFArray iteration below (which retains around recursion). Either way,
    // `from_get` is the correct rule here since the caller still holds the
    // owning reference for the duration of this visitor.
    refs.insert(uid, AXRef::from_get(element));

    if let Some(children) = attr::array(element, "AXChildren").ok().flatten() {
        for i in 0..children.len() {
            let child = *children.get_unchecked(i) as AXUIElementRef;
            core_foundation::base::CFRetain(child as core_foundation::base::CFTypeRef);
            collect_ax_tree_recursive(child, element_count, depth + 1, next_uid, nodes, refs);
            core_foundation::base::CFRelease(child as core_foundation::base::CFTypeRef);
        }
    }
}

/// Walk an application's AX tree and return flat nodes + a
/// `HashMap<uid, AXRef>`.
///
/// The `AXRef` map is the source of truth for `AxSession` — each entry is a
/// retained handle that stays live until the session swaps in a new snapshot.
pub fn collect_ax_tree_indexed(
    app_name: Option<&str>,
) -> Result<(Vec<AXSnapshotNode>, std::collections::HashMap<u32, AXRef>), String> {
    let pid = match app_name {
        Some(name) => super::pid_for_app_name(name)?,
        None => super::frontmost_pid()?,
    };

    let app_element = unsafe { AXUIElementCreateApplication(pid) };
    if app_element.is_null() {
        return Err(format!("Failed to create AXUIElement for pid {}", pid));
    }

    let mut nodes = Vec::new();
    let mut element_count: usize = 0;
    let mut next_uid: u32 = 1;
    let mut refs = std::collections::HashMap::new();

    unsafe {
        collect_ax_tree_recursive(
            app_element,
            &mut element_count,
            0,
            &mut next_uid,
            &mut nodes,
            &mut refs,
        );
        core_foundation::base::CFRelease(app_element as core_foundation::base::CFTypeRef);
    }

    Ok((nodes, refs))
}

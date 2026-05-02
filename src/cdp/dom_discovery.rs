//! DOM-native discovery for CDP-connected pages.
//!
//! Walks the live DOM via Runtime.evaluate to find interactive elements,
//! extract semantic labels, and assign d<N> prefixed UIDs.

use super::{SnapshotMap, SnapshotNode};
use std::collections::HashMap;

/// Viewport-relative bounds for a DOM candidate.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct DomRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A candidate element extracted from the live DOM.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct DomCandidate {
    #[serde(rename = "backendNodeId")]
    pub backend_node_id: i64,
    pub role: String,
    pub label: String,
    pub tag: String,
    pub disabled: bool,
    #[serde(rename = "parentRole")]
    pub parent_role: String,
    #[serde(rename = "parentName")]
    pub parent_name: String,
    #[serde(rename = "accessibleName", default)]
    pub accessible_name: String,
    #[serde(rename = "visibleText", default)]
    pub visible_text: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub placeholder: String,
    #[serde(default)]
    pub title: String,
    #[serde(rename = "altText", default)]
    pub alt_text: String,
    #[serde(rename = "testId", default)]
    pub test_id: String,
    #[serde(rename = "matchedOn", default)]
    pub matched_on: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(rename = "viewportRect", default)]
    pub viewport_rect: Option<DomRect>,
    #[serde(rename = "inViewport", default)]
    pub in_viewport: bool,
}

/// Build a SnapshotMap from DOM candidates, assigning d<N> prefixed UIDs.
///
/// `page_url` and `generation` are stamped onto the resulting
/// [`SnapshotMap`] so stale snapshots are detected at lookup time.
pub fn build_dom_snapshot(
    candidates: &[DomCandidate],
    page_url: String,
    generation: u64,
) -> SnapshotMap {
    let mut uid_to_node = HashMap::new();
    let mut uid_to_candidate = HashMap::new();
    let mut backend_to_uids: HashMap<i64, Vec<String>> = HashMap::new();
    let mut ordered_uids = Vec::with_capacity(candidates.len());

    for (i, candidate) in candidates.iter().enumerate() {
        let uid_key = format!("d{}", i + 1);
        ordered_uids.push(uid_key.clone());

        uid_to_node.insert(
            uid_key.clone(),
            SnapshotNode {
                backend_node_id: candidate.backend_node_id,
                role: candidate.role.clone(),
                name: snapshot_display_name(candidate),
            },
        );
        uid_to_candidate.insert(uid_key.clone(), candidate.clone());

        if candidate.backend_node_id != 0 {
            backend_to_uids
                .entry(candidate.backend_node_id)
                .or_default()
                .push(uid_key);
        }
    }

    SnapshotMap {
        uid_to_node,
        uid_to_candidate,
        backend_to_uids,
        ordered_uids,
        page_url,
        generation,
    }
}

fn snapshot_display_name(candidate: &DomCandidate) -> String {
    let matched_visible_text = candidate
        .matched_on
        .iter()
        .any(|field| field == "visible_text");
    let stale_accessible_name = candidate
        .warnings
        .iter()
        .any(|warning| warning == "accessible_name_visible_text_mismatch");

    if !candidate.visible_text.is_empty()
        && (matched_visible_text || stale_accessible_name || candidate.label.is_empty())
    {
        return candidate.visible_text.clone();
    }

    [
        &candidate.label,
        &candidate.accessible_name,
        &candidate.value,
        &candidate.placeholder,
        &candidate.title,
        &candidate.alt_text,
        &candidate.test_id,
        &candidate.visible_text,
    ]
    .into_iter()
    .find(|value| !value.is_empty())
    .cloned()
    .unwrap_or_default()
}

/// Build the JavaScript expression that walks the DOM and returns interactive candidates.
///
/// The JS walker:
/// 1. Collects interactive elements (buttons, links, inputs, textareas, selects,
///    contenteditable, elements with interactive ARIA roles, tabindex >= 0)
/// 2. Filters invisible elements (display:none, visibility:hidden, aria-hidden, zero-size, inert)
/// 3. Walks open shadow roots and same-origin iframes
/// 4. Returns matched elements and a parallel metadata array,
///    plus an `inventory` summarizing all interactive elements by role
///
/// Returns `{ elements: Element[], metadata: [...], inventory: [...] }` as a non-by-value object.
/// The caller must use `Runtime.callFunctionOn` to extract `metadata` (a JSON array of
/// `DomCandidate` objects) in one bulk call, then `DOM.describeNode` per element to
/// resolve `backendNodeId`.
pub fn dom_walker_js(query: &str, role_filter: Option<&str>, max_results: u32) -> String {
    // Use serde_json::to_string for proper JS string encoding (handles all edge cases)
    let query_json = serde_json::to_string(query).unwrap();
    let role_json = role_filter
        .map(|r| serde_json::to_string(r).unwrap())
        .unwrap_or_else(|| "null".to_string());

    format!(
        r##"(() => {{
const QUERY = {query_json};
const ROLE_FILTER = {role_json};
const MAX = {max_results};

const INTERACTIVE_TAGS = new Set(["BUTTON", "A", "INPUT", "TEXTAREA", "SELECT", "SUMMARY"]);
const INTERACTIVE_ROLES = new Set([
    "button", "checkbox", "combobox", "link", "menuitem", "menuitemcheckbox",
    "menuitemradio", "option", "radio", "searchbox", "slider", "spinbutton",
    "switch", "tab", "textbox", "treeitem"
]);

function isVisible(el) {{
    if (el.closest("[aria-hidden='true']") || el.closest("[inert]")) return false;
    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden") return false;
    if (el.offsetWidth === 0 && el.offsetHeight === 0) return false;
    return true;
}}

function normalizeText(value) {{
    return (value || "").replace(/\s+/g, " ").trim();
}}

function truncate(value, max) {{
    const text = normalizeText(value);
    return text.length > max ? text.substring(0, max) : text;
}}

function labelledByText(el) {{
    const labelledBy = el.getAttribute("aria-labelledby");
    if (!labelledBy) return "";
    return labelledBy.split(/\s+/).map(id => {{
        const root = el.getRootNode();
        const ref_ = root && root.getElementById ? root.getElementById(id) : null;
        return ref_ ? ref_.textContent : "";
    }}).filter(Boolean).join(" ");
}}

function labelElementText(el) {{
    if (!el.labels || !el.labels.length) return "";
    return Array.from(el.labels).map(l => l.textContent).join(" ");
}}

function formValue(el) {{
    if (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT") {{
        return el.value || "";
    }}
    return "";
}}

function explicitAccessibleName(el) {{
    const ariaLabel = el.getAttribute("aria-label");
    if (ariaLabel) return ariaLabel;

    const labelledBy = labelledByText(el);
    if (labelledBy) return labelledBy;

    const labelText = labelElementText(el);
    if (labelText) return labelText;

    const ph = el.getAttribute("placeholder") || el.getAttribute("data-placeholder");
    if (ph) return ph;

    if (el.tagName === "INPUT" && ["submit", "button", "reset"].includes(el.type)) {{
        const value = formValue(el);
        if (value) return value;
    }}

    const title = el.getAttribute("title");
    if (title) return title;

    const alt = el.getAttribute("alt");
    if (alt) return alt;

    return "";
}}

function getLabel(el) {{
    const ariaLabel = el.getAttribute("aria-label");
    if (ariaLabel) return ariaLabel.trim();

    const labelledBy = labelledByText(el);
    if (labelledBy) return normalizeText(labelledBy);

    const labelText = labelElementText(el);
    if (labelText) return normalizeText(labelText);

    const ph = el.getAttribute("placeholder") || el.getAttribute("data-placeholder");
    if (ph) return ph.trim();

    if (el.tagName === "INPUT" && ["submit", "button", "reset"].includes(el.type)) {{
        if (el.value) return el.value.trim();
    }}

    const title = el.getAttribute("title");
    if (title) return title.trim();

    const alt = el.getAttribute("alt");
    if (alt) return alt.trim();

    // Fallback: collect the element's own text. Prefer direct text nodes
    // (what this element itself says) over any descendant text. Only walk
    // into children — with pruning — if the element has no direct text of
    // its own. This prevents a header button wrapping avatar + name +
    // badges from collapsing all descendant textContent into a composite
    // label like "Note to Self 1 week Verified".
    const own = ownTextNodes(el);
    if (own) return own.substring(0, 200);

    const nested = directOwnText(el).trim().substring(0, 200);
    if (nested) return nested;

    // Last resort: tag name, so we never concatenate descendant text.
    return el.tagName.toLowerCase();
}}

function getVisibleText(el) {{
    // innerText follows rendered text more closely than textContent. Fall
    // back to the old pruned text helpers for elements/browsers that do not
    // expose useful innerText.
    const inner = truncate(el.innerText || "", 300);
    if (inner) return inner;
    const own = ownTextNodes(el);
    if (own) return truncate(own, 300);
    return truncate(directOwnText(el), 300);
}}

function hasOwnLabel(el) {{
    return el.hasAttribute("aria-label")
        || el.hasAttribute("aria-labelledby")
        || el.hasAttribute("title")
        || el.hasAttribute("alt")
        || el.hasAttribute("role")
        || el.hasAttribute("data-testid");
}}

// Returns the concatenation of this element's *direct* child text nodes
// (nodeType === 3), normalised. Ignores descendant elements entirely. This
// is the preferred label source: it captures what the element itself says
// without picking up styled-span badges or avatar text.
function ownTextNodes(el) {{
    let out = "";
    for (const child of el.childNodes) {{
        if (child.nodeType === Node.TEXT_NODE) {{
            out += child.nodeValue;
        }}
    }}
    return out.replace(/\s+/g, " ").trim();
}}

function directOwnText(el) {{
    let out = "";
    for (const child of el.childNodes) {{
        if (child.nodeType === Node.TEXT_NODE) {{
            out += child.nodeValue;
        }} else if (child.nodeType === Node.ELEMENT_NODE) {{
            // Prune subtrees that own their own label or are interactive on
            // their own — their text belongs to them, not the ancestor.
            if (hasOwnLabel(child)) continue;
            if (isInteractive(child)) continue;
            out += " " + directOwnText(child);
        }}
    }}
    return out.replace(/\s+/g, " ").trim();
}}

function getRole(el) {{
    const ariaRole = el.getAttribute("role");
    if (ariaRole && INTERACTIVE_ROLES.has(ariaRole)) return ariaRole;

    const tag = el.tagName;
    if (tag === "BUTTON" || (tag === "INPUT" && ["submit", "button", "reset"].includes(el.type))) return "button";
    if (tag === "A" && el.hasAttribute("href")) return "link";
    if (tag === "INPUT") {{
        const t = el.type || "text";
        if (t === "checkbox") return "checkbox";
        if (t === "radio") return "radio";
        if (t === "search") return "searchbox";
        if (t === "range") return "slider";
        if (t === "number") return "spinbutton";
        return "textbox";
    }}
    if (tag === "TEXTAREA") return "textbox";
    if (tag === "SELECT") return "combobox";
    if (tag === "SUMMARY") return "button";
    if (el.isContentEditable) return "textbox";
    if (ariaRole) return ariaRole;
    return "generic";
}}

function getParentContext(el) {{
    let parent = el.parentElement;
    while (parent) {{
        const role = parent.getAttribute("role");
        if (role) {{
            const name = parent.getAttribute("aria-label") || parent.textContent?.trim().substring(0, 50) || "";
            return {{ role, name }};
        }}
        const tag = parent.tagName;
        if (["NAV", "MAIN", "ASIDE", "HEADER", "FOOTER", "SECTION", "FORM", "DIALOG"].includes(tag)) {{
            const name = parent.getAttribute("aria-label") || "";
            return {{ role: tag.toLowerCase(), name }};
        }}
        parent = parent.parentElement;
    }}
    return {{ role: "", name: "" }};
}}

function viewportRect(el) {{
    const rect = el.getBoundingClientRect();
    return {{
        x: Math.round(rect.x * 10) / 10,
        y: Math.round(rect.y * 10) / 10,
        width: Math.round(rect.width * 10) / 10,
        height: Math.round(rect.height * 10) / 10,
    }};
}}

function intersectsViewport(rect) {{
    return rect.width > 0
        && rect.height > 0
        && rect.x < window.innerWidth
        && rect.y < window.innerHeight
        && rect.x + rect.width > 0
        && rect.y + rect.height > 0;
}}

function matchFields(fields) {{
    if (!queryLower) return [];
    const matched = [];
    for (const [name, value] of Object.entries(fields)) {{
        if (value && value.toLowerCase().includes(queryLower)) {{
            matched.push(name);
        }}
    }}
    return matched;
}}

function meaningfullyDifferent(a, b) {{
    const left = normalizeText(a).toLowerCase();
    const right = normalizeText(b).toLowerCase();
    if (!left || !right || left === right) return false;
    return !left.includes(right) && !right.includes(left);
}}

function isInteractive(el) {{
    if (INTERACTIVE_TAGS.has(el.tagName)) return true;
    if (el.isContentEditable && (!el.parentElement || !el.parentElement.isContentEditable)) return true;
    const role = el.getAttribute("role");
    if (role && INTERACTIVE_ROLES.has(role)) return true;
    const tabindex = el.getAttribute("tabindex");
    if (tabindex !== null && parseInt(tabindex, 10) >= 0) return true;
    return false;
}}

function walk(root, results) {{
    const elements = root.querySelectorAll("*");
    for (const el of elements) {{
        if (isInteractive(el)) {{
            results.push(el);
        }}
        if (el.shadowRoot) {{
            walk(el.shadowRoot, results);
        }}
    }}
    // Same-origin iframes
    if (root === document) {{
        for (const iframe of document.querySelectorAll("iframe")) {{
            try {{
                if (iframe.contentDocument) walk(iframe.contentDocument, results);
            }} catch(e) {{}}
        }}
    }}
}}

const allElements = [];
walk(document, allElements);

const queryLower = QUERY.toLowerCase();
const matched = [];
const roleCounts = {{}};

for (const el of allElements) {{
    const label = getLabel(el);
    const role = getRole(el);

    // Inventory counts all interactive elements regardless of visibility or text match
    if (!roleCounts[role]) roleCounts[role] = {{ count: 0, labels: [] }};
    roleCounts[role].count++;
    const inventoryLabel = label || (roleCounts[role].labels.length < 3 ? getVisibleText(el) : "");
    if (roleCounts[role].labels.length < 3 && inventoryLabel) {{
        roleCounts[role].labels.push(inventoryLabel.substring(0, 80));
    }}

    // Summary-only calls ask for MAX=0 and only consume inventory. Avoid
    // computing expensive per-candidate metadata that would be discarded.
    if (MAX === 0) continue;

    const accessibleName = truncate(explicitAccessibleName(el), 200);
    const visibleText = getVisibleText(el);
    const value = truncate(formValue(el), 200);
    const placeholder = truncate(el.getAttribute("placeholder") || el.getAttribute("data-placeholder") || "", 200);
    const title = truncate(el.getAttribute("title") || "", 200);
    const altText = truncate(el.getAttribute("alt") || "", 200);
    const testId = truncate(el.getAttribute("data-testid") || el.getAttribute("data-test") || el.getAttribute("data-cy") || "", 200);

    // Match filter: cheap checks first, expensive isVisible last.
    if (ROLE_FILTER && role !== ROLE_FILTER) continue;

    const parent = getParentContext(el);
    const matchedOn = matchFields({{
        label,
        accessible_name: accessibleName,
        visible_text: visibleText,
        value,
        placeholder,
        title,
        alt_text: altText,
        test_id: testId,
    }});
    if (queryLower && matchedOn.length === 0) continue;
    if (!isVisible(el)) continue;

    const tag = el.tagName.toLowerCase();
    const disabled = el.disabled === true || el.getAttribute("aria-disabled") === "true";
    const rect = viewportRect(el);
    const inViewport = intersectsViewport(rect);
    const warnings = [];
    if (meaningfullyDifferent(accessibleName, visibleText)) {{
        warnings.push("accessible_name_visible_text_mismatch");
    }}
    if (!inViewport) {{
        warnings.push("outside_viewport");
    }}

    // Store metadata in a parallel array (avoids mutating live DOM elements)
    matched.push({{
        el,
        metadata: {{
            backendNodeId: 0,
            role,
            label: label.substring(0, 200),
            tag,
            disabled,
            parentRole: parent.role,
            parentName: truncate(parent.name, 100),
            accessibleName,
            visibleText,
            value,
            placeholder,
            title,
            altText,
            testId,
            matchedOn,
            warnings,
            viewportRect: rect,
            inViewport,
        }},
    }});
}}

matched.sort((a, b) => {{
    if (a.metadata.inViewport !== b.metadata.inViewport) {{
        return a.metadata.inViewport ? -1 : 1;
    }}
    if (a.metadata.viewportRect.y !== b.metadata.viewportRect.y) {{
        return a.metadata.viewportRect.y - b.metadata.viewportRect.y;
    }}
    return a.metadata.viewportRect.x - b.metadata.viewportRect.x;
}});

const selected = matched.slice(0, MAX);
const matchedElements = selected.map(item => item.el);
const metadataArray = selected.map(item => item.metadata);

const inventory = Object.entries(roleCounts).map(([role, data]) => ({{
    role,
    count: data.count,
    sample_labels: data.labels,
}}));

return {{ elements: matchedElements, metadata: metadataArray, inventory }};
}})()"##
    )
}

/// Format DOM candidates as indented text (similar to AX snapshot format).
///
/// Includes parent context (`role` / optional `name`) when the walker
/// captured one, so an LLM can disambiguate, e.g., a sidebar list row from
/// a chat-header button that happen to carry the same label text.
pub fn format_dom_snapshot(candidates: &[DomCandidate]) -> String {
    let mut lines = Vec::with_capacity(candidates.len());
    for (i, node) in candidates.iter().enumerate() {
        let mut parts = vec![format!("uid=d{} {}", i + 1, node.role)];
        if !node.label.is_empty() {
            parts.push(format!("\"{}\"", node.label));
        }
        if !node.accessible_name.is_empty() && node.accessible_name != node.label {
            parts.push(format!("accessible_name=\"{}\"", node.accessible_name));
        }
        if !node.visible_text.is_empty() && node.visible_text != node.label {
            parts.push(format!("visible_text=\"{}\"", node.visible_text));
        }
        if !node.value.is_empty() && node.value != node.label {
            parts.push(format!("value=\"{}\"", node.value));
        }
        if !node.placeholder.is_empty() && node.placeholder != node.label {
            parts.push(format!("placeholder=\"{}\"", node.placeholder));
        }
        if !node.title.is_empty() && node.title != node.label {
            parts.push(format!("title=\"{}\"", node.title));
        }
        if !node.alt_text.is_empty() && node.alt_text != node.label {
            parts.push(format!("alt=\"{}\"", node.alt_text));
        }
        if !node.test_id.is_empty() {
            parts.push(format!("test_id=\"{}\"", node.test_id));
        }
        parts.push(format!("tag={}", node.tag));
        if node.disabled {
            parts.push("disabled".to_string());
        }
        if let Some(rect) = &node.viewport_rect {
            parts.push(format!(
                "rect=({:.0},{:.0} {:.0}x{:.0})",
                rect.x, rect.y, rect.width, rect.height
            ));
            if !node.in_viewport {
                parts.push("offscreen".to_string());
            }
        }
        if !node.matched_on.is_empty() {
            parts.push(format!("matched_on={}", node.matched_on.join(",")));
        }
        if !node.warnings.is_empty() {
            parts.push(format!("warnings={}", node.warnings.join(",")));
        }
        if !node.parent_role.is_empty() {
            if node.parent_name.is_empty() {
                parts.push(format!("(in {})", node.parent_role));
            } else {
                parts.push(format!(
                    "(in {} \"{}\")",
                    node.parent_role, node.parent_name
                ));
            }
        }
        lines.push(parts.join(" "));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_dom_snapshot_map_basic() {
        let candidates = vec![
            DomCandidate {
                backend_node_id: 10,
                role: "button".to_string(),
                label: "Submit".to_string(),
                tag: "button".to_string(),
                disabled: false,
                parent_role: "form".to_string(),
                parent_name: "Login".to_string(),
                ..Default::default()
            },
            DomCandidate {
                backend_node_id: 20,
                role: "textbox".to_string(),
                label: "Email".to_string(),
                tag: "input".to_string(),
                disabled: false,
                parent_role: "form".to_string(),
                parent_name: "Login".to_string(),
                ..Default::default()
            },
        ];

        let map = build_dom_snapshot(&candidates, "about:blank".to_string(), 0);
        assert_eq!(map.uid_to_node.len(), 2);
        assert!(map.uid_to_node.contains_key("d1"));
        assert!(map.uid_to_node.contains_key("d2"));
        assert_eq!(map.uid_to_node["d1"].backend_node_id, 10);
        assert_eq!(map.uid_to_node["d1"].role, "button");
        assert_eq!(map.uid_to_node["d1"].name, "Submit");
    }

    #[test]
    fn build_dom_snapshot_reverse_map() {
        let candidates = vec![DomCandidate {
            backend_node_id: 42,
            role: "button".to_string(),
            label: "Ok".to_string(),
            tag: "button".to_string(),
            disabled: false,
            parent_role: "dialog".to_string(),
            parent_name: "Confirm".to_string(),
            ..Default::default()
        }];

        let map = build_dom_snapshot(&candidates, "about:blank".to_string(), 0);
        assert_eq!(map.backend_to_uids[&42], vec!["d1"]);
    }

    #[test]
    fn build_dom_snapshot_uses_visible_display_name_for_stale_accessibility_label() {
        let candidates = vec![DomCandidate {
            backend_node_id: 42,
            role: "button".to_string(),
            label: "Chat with Ljuba Isakovic, 0 new messages".to_string(),
            tag: "button".to_string(),
            disabled: false,
            parent_role: "list".to_string(),
            parent_name: "Chats".to_string(),
            visible_text: "Note to Self Tue Photo".to_string(),
            matched_on: vec!["visible_text".to_string()],
            warnings: vec!["accessible_name_visible_text_mismatch".to_string()],
            ..Default::default()
        }];

        let map = build_dom_snapshot(&candidates, "about:blank".to_string(), 0);

        assert_eq!(map.uid_to_node["d1"].name, "Note to Self Tue Photo");
    }

    #[test]
    fn dom_walker_js_is_valid_javascript() {
        let js = dom_walker_js("Search", None, 10);
        // Basic sanity: contains the query and returns elements + inventory
        assert!(js.contains("Search"));
        assert!(js.contains("elements"));
        assert!(js.contains("inventory"));
    }

    #[test]
    fn dom_walker_js_encodes_special_chars() {
        // Verify serde_json encoding handles edge cases
        let js = dom_walker_js("test\"with\\quotes", Some("button"), 5);
        assert!(js.contains(r#"test\"with\\quotes"#));
        assert!(js.contains(r#""button""#));
    }

    #[test]
    fn dom_walker_js_keeps_parent_name_out_of_match_fields() {
        let js = dom_walker_js("Note to Self", None, 10);
        assert!(
            !js.contains("parent_name: parent.name"),
            "parent context should be rendered as metadata, not treated as a primary query hit"
        );
        assert!(
            js.contains("parentName: truncate(parent.name, 100)"),
            "parent context should still be returned for disambiguation"
        );
    }

    #[test]
    fn dom_walker_js_short_circuits_candidate_metadata_when_max_zero() {
        let js = dom_walker_js("", None, 0);
        assert!(
            js.contains("if (MAX === 0) continue;"),
            "summary-only calls should skip per-candidate metadata work"
        );
    }

    #[test]
    fn format_dom_snapshot_basic() {
        let candidates = vec![
            DomCandidate {
                backend_node_id: 10,
                role: "button".to_string(),
                label: "Submit".to_string(),
                tag: "button".to_string(),
                disabled: false,
                parent_role: "form".to_string(),
                parent_name: "Login".to_string(),
                ..Default::default()
            },
            DomCandidate {
                backend_node_id: 20,
                role: "textbox".to_string(),
                label: "".to_string(),
                tag: "input".to_string(),
                disabled: true,
                parent_role: "".to_string(),
                parent_name: "".to_string(),
                ..Default::default()
            },
        ];

        let result = format_dom_snapshot(&candidates);
        assert_eq!(
            result,
            "uid=d1 button \"Submit\" tag=button (in form \"Login\")\nuid=d2 textbox tag=input disabled"
        );
    }

    #[test]
    fn format_dom_snapshot_parent_role_only() {
        // parent_role set, parent_name empty → emit only the role.
        let candidates = vec![DomCandidate {
            backend_node_id: 1,
            role: "button".to_string(),
            label: "Send".to_string(),
            tag: "button".to_string(),
            disabled: false,
            parent_role: "nav".to_string(),
            parent_name: "".to_string(),
            ..Default::default()
        }];

        let result = format_dom_snapshot(&candidates);
        assert_eq!(result, "uid=d1 button \"Send\" tag=button (in nav)");
    }

    #[test]
    fn format_dom_snapshot_disambiguates_sidebar_vs_header() {
        // Regression: the clickweave agent once confused a sidebar row and a
        // chat-header button that both surfaced the label "Note to Self" in
        // the snapshot. With parent context rendered, the two lines are
        // visibly distinct.
        let candidates = vec![
            DomCandidate {
                backend_node_id: 100,
                role: "button".to_string(),
                label: "Note to Self".to_string(),
                tag: "li".to_string(),
                disabled: false,
                parent_role: "list".to_string(),
                parent_name: "Chats".to_string(),
                ..Default::default()
            },
            DomCandidate {
                backend_node_id: 200,
                role: "button".to_string(),
                label: "Note to Self".to_string(),
                tag: "button".to_string(),
                disabled: false,
                parent_role: "header".to_string(),
                parent_name: "".to_string(),
                ..Default::default()
            },
        ];

        let result = format_dom_snapshot(&candidates);
        assert_eq!(
            result,
            "uid=d1 button \"Note to Self\" tag=li (in list \"Chats\")\n\
             uid=d2 button \"Note to Self\" tag=button (in header)"
        );
    }

    #[test]
    fn dom_walker_js_uses_direct_text_fallback() {
        // Sanity: the emitted JS includes the direct-text helpers and no
        // longer relies on bare `el.textContent` for the label fallback.
        let js = dom_walker_js("anything", None, 1);
        assert!(
            js.contains("directOwnText"),
            "expected directOwnText helper in walker JS"
        );
        assert!(
            js.contains("hasOwnLabel"),
            "expected hasOwnLabel helper in walker JS"
        );
        // The old fallback concatenated all descendants via `el.textContent`.
        // Guard against regressing to that.
        assert!(
            !js.contains("el.textContent || \"\""),
            "walker JS must not fall back to raw el.textContent for label"
        );
    }

    #[test]
    fn dom_walker_js_prefers_element_own_text_over_descendants() {
        // Primary defence: the walker should look at an element's direct
        // text nodes first (ownTextNodes) and only descend into children
        // when the element has no direct text of its own. Without this,
        // header buttons like Signal's "Note to Self" chat header collapse
        // their sibling badge spans ("1 week", "Verified") into a composite
        // label.
        let js = dom_walker_js("anything", None, 1);
        assert!(
            js.contains("ownTextNodes"),
            "expected ownTextNodes helper (direct text nodes) in walker JS"
        );
        // The helper must filter for TEXT_NODE only — no ELEMENT_NODE branch.
        let helper_start = js
            .find("function ownTextNodes")
            .expect("ownTextNodes should be defined");
        // The next function definition marks the end of ownTextNodes.
        let helper_rest = &js[helper_start..];
        let after_header = helper_rest.find('{').expect("ownTextNodes has no body") + 1;
        let helper_end = helper_rest[after_header..]
            .find("function ")
            .expect("ownTextNodes body not followed by another function")
            + after_header;
        let helper_body = &helper_rest[..helper_end];
        assert!(
            helper_body.contains("Node.TEXT_NODE"),
            "ownTextNodes must inspect TEXT_NODE children"
        );
        assert!(
            !helper_body.contains("ELEMENT_NODE"),
            "ownTextNodes must NOT descend into element children"
        );
        // getLabel must try ownTextNodes before directOwnText so the direct
        // path wins whenever the element has any direct text at all.
        let get_label_start = js.find("function getLabel").expect("getLabel must exist");
        let get_label_body = &js[get_label_start..];
        let own_idx = get_label_body
            .find("ownTextNodes(el)")
            .expect("getLabel must call ownTextNodes");
        let nested_idx = get_label_body
            .find("directOwnText(el)")
            .expect("getLabel must call directOwnText as fallback");
        assert!(
            own_idx < nested_idx,
            "getLabel must call ownTextNodes before directOwnText"
        );
    }

    #[test]
    fn dom_walker_js_hasownlabel_covers_role_and_testid() {
        // Belt-and-braces: role= and data-testid on a descendant are strong
        // signals it's a semantic unit of its own; pruning them makes the
        // recursive fallback safer if we ever fall back to it (e.g. buttons
        // whose text actually lives inside a wrapper span).
        let js = dom_walker_js("anything", None, 1);
        let start = js
            .find("function hasOwnLabel")
            .expect("hasOwnLabel must exist");
        let rest = &js[start..];
        let after_header = rest.find('{').expect("hasOwnLabel has no body") + 1;
        let end = rest[after_header..]
            .find("function ")
            .expect("hasOwnLabel body not followed by another function")
            + after_header;
        let body = &rest[..end];
        assert!(body.contains("\"role\""), "hasOwnLabel should prune role=");
        assert!(
            body.contains("\"data-testid\""),
            "hasOwnLabel should prune data-testid="
        );
    }

    #[test]
    fn format_dom_snapshot_header_button_wraps_badges() {
        // Structural regression: in Signal's web-based chat header, the
        // title button wraps "Note to Self" plus unlabelled badge spans
        // ("1 week", "Verified"). With the new direct-text-first logic,
        // the DOM walker reports the button's direct text only, so
        // format_dom_snapshot emits a clean label — not a composite.
        //
        // This test validates the downstream rendering given candidates
        // that already reflect the new walker output: label is just
        // "Note to Self", and the header context is preserved for
        // disambiguation against sidebar rows.
        let candidates = vec![DomCandidate {
            backend_node_id: 321,
            role: "button".to_string(),
            label: "Note to Self".to_string(),
            tag: "button".to_string(),
            disabled: false,
            parent_role: "header".to_string(),
            parent_name: "".to_string(),
            ..Default::default()
        }];

        let result = format_dom_snapshot(&candidates);
        assert_eq!(
            result,
            "uid=d1 button \"Note to Self\" tag=button (in header)"
        );
        assert!(
            !result.contains("1 week"),
            "badge text must not leak into the header button label"
        );
        assert!(
            !result.contains("Verified"),
            "verified badge must not leak into the header button label"
        );
    }
}

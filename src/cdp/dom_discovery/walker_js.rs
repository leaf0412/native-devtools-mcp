//! JavaScript walker template for DOM-native discovery.
//!
//! Builds the embedded JS expression executed via Runtime.evaluate to
//! collect interactive DOM candidates. The template string is verbatim and
//! sensitive to formatting.

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

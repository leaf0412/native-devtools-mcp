//! CDP keyboard tools: press_key, type_text plus key-mapping helpers.

use crate::cdp::{cdp_error, CdpClient};
use chromiumoxide::cdp::browser_protocol::input::{DispatchKeyEventParams, DispatchKeyEventType};
use rmcp::model::{CallToolResult, Content};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Map a key name to its CDP key identifier, code, and Windows virtual key code.
fn key_definition(key: &str) -> Option<(&'static str, &'static str, i64)> {
    Some(match key {
        "Enter" => ("Enter", "Enter", 13),
        "Tab" => ("Tab", "Tab", 9),
        "Escape" => ("Escape", "Escape", 27),
        "Backspace" => ("Backspace", "Backspace", 8),
        "Delete" => ("Delete", "Delete", 46),
        "ArrowUp" => ("ArrowUp", "ArrowUp", 38),
        "ArrowDown" => ("ArrowDown", "ArrowDown", 40),
        "ArrowLeft" => ("ArrowLeft", "ArrowLeft", 37),
        "ArrowRight" => ("ArrowRight", "ArrowRight", 39),
        "Home" => ("Home", "Home", 36),
        "End" => ("End", "End", 35),
        "PageUp" => ("PageUp", "PageUp", 33),
        "PageDown" => ("PageDown", "PageDown", 34),
        "Space" | " " => (" ", "Space", 32),
        "F1" => ("F1", "F1", 112),
        "F2" => ("F2", "F2", 113),
        "F3" => ("F3", "F3", 114),
        "F4" => ("F4", "F4", 115),
        "F5" => ("F5", "F5", 116),
        "F6" => ("F6", "F6", 117),
        "F7" => ("F7", "F7", 118),
        "F8" => ("F8", "F8", 119),
        "F9" => ("F9", "F9", 120),
        "F10" => ("F10", "F10", 121),
        "F11" => ("F11", "F11", 122),
        "F12" => ("F12", "F12", 123),
        _ if key.len() == 1 => return None,
        _ => return None,
    })
}

/// Map a single character to its DOM `code` and Windows virtual key code.
fn char_key_code(ch: char) -> (&'static str, i64) {
    match ch {
        'a'..='z' | 'A'..='Z' => {
            let upper = ch.to_ascii_uppercase();
            let code = match upper {
                'A' => "KeyA",
                'B' => "KeyB",
                'C' => "KeyC",
                'D' => "KeyD",
                'E' => "KeyE",
                'F' => "KeyF",
                'G' => "KeyG",
                'H' => "KeyH",
                'I' => "KeyI",
                'J' => "KeyJ",
                'K' => "KeyK",
                'L' => "KeyL",
                'M' => "KeyM",
                'N' => "KeyN",
                'O' => "KeyO",
                'P' => "KeyP",
                'Q' => "KeyQ",
                'R' => "KeyR",
                'S' => "KeyS",
                'T' => "KeyT",
                'U' => "KeyU",
                'V' => "KeyV",
                'W' => "KeyW",
                'X' => "KeyX",
                'Y' => "KeyY",
                'Z' => "KeyZ",
                _ => unreachable!(),
            };
            (code, upper as i64)
        }
        '0' => ("Digit0", 0x30),
        '1' => ("Digit1", 0x31),
        '2' => ("Digit2", 0x32),
        '3' => ("Digit3", 0x33),
        '4' => ("Digit4", 0x34),
        '5' => ("Digit5", 0x35),
        '6' => ("Digit6", 0x36),
        '7' => ("Digit7", 0x37),
        '8' => ("Digit8", 0x38),
        '9' => ("Digit9", 0x39),
        '-' => ("Minus", 0xBD),
        '=' | '+' => ("Equal", 0xBB),
        '[' => ("BracketLeft", 0xDB),
        ']' => ("BracketRight", 0xDD),
        '\\' => ("Backslash", 0xDC),
        ';' => ("Semicolon", 0xBA),
        '\'' => ("Quote", 0xDE),
        ',' => ("Comma", 0xBC),
        '.' => ("Period", 0xBE),
        '/' => ("Slash", 0xBF),
        '`' => ("Backquote", 0xC0),
        _ => ("Unidentified", 0),
    }
}

const MODIFIER_ALT: i64 = 1;
const MODIFIER_CONTROL: i64 = 2;
const MODIFIER_META: i64 = 4;
const MODIFIER_SHIFT: i64 = 8;

/// Map a modifier name to its CDP bitmask.
fn modifier_bit(name: &str) -> Option<i64> {
    match name {
        "Alt" => Some(MODIFIER_ALT),
        "Control" => Some(MODIFIER_CONTROL),
        "Meta" => Some(MODIFIER_META),
        "Shift" => Some(MODIFIER_SHIFT),
        _ => None,
    }
}

/// Parsed key combination: modifier bitmask + main key name.
#[derive(Debug)]
struct ParsedKeyCombo {
    modifiers: i64,
    modifier_names: Vec<String>,
    main_key: String,
}

/// Parse a key combo string like "Control+Shift+A" or "Control++".
/// Returns Err with the unknown modifier name on failure.
fn parse_key_combo(key: &str) -> Result<ParsedKeyCombo, String> {
    let parts: Vec<&str> = key.split('+').collect();

    let (modifier_parts, main_key) = if key.ends_with("++") {
        (&parts[..parts.len() - 2], "+")
    } else if parts.len() > 1 {
        (&parts[..parts.len() - 1], *parts.last().unwrap_or(&""))
    } else {
        (&[][..], parts[0])
    };

    let mut modifiers: i64 = 0;
    let mut modifier_names = Vec::new();
    for &m in modifier_parts {
        match modifier_bit(m) {
            Some(bit) => {
                modifiers |= bit;
                modifier_names.push(m.to_string());
            }
            None => return Err(m.to_string()),
        }
    }

    Ok(ParsedKeyCombo {
        modifiers,
        modifier_names,
        main_key: main_key.to_string(),
    })
}

/// Dispatch a named key press (RawKeyDown + KeyUp) with optional modifiers.
async fn dispatch_named_key(
    page: &chromiumoxide::page::Page,
    key_val: &str,
    code: &str,
    vk: i64,
    modifiers: i64,
) -> Result<(), String> {
    let mut down = DispatchKeyEventParams::new(DispatchKeyEventType::RawKeyDown);
    down.key = Some(key_val.to_string());
    down.code = Some(code.to_string());
    down.windows_virtual_key_code = Some(vk);
    down.modifiers = Some(modifiers);
    page.execute(down)
        .await
        .map_err(|e| format!("Failed to press key {}: {}", key_val, e))?;

    let mut up = DispatchKeyEventParams::new(DispatchKeyEventType::KeyUp);
    up.key = Some(key_val.to_string());
    up.code = Some(code.to_string());
    up.windows_virtual_key_code = Some(vk);
    up.modifiers = Some(modifiers);
    page.execute(up)
        .await
        .map_err(|e| format!("Failed to release key {}: {}", key_val, e))?;

    Ok(())
}

/// Dispatch a single character key press (RawKeyDown + Char + KeyUp).
async fn dispatch_char(
    page: &chromiumoxide::page::Page,
    ch: char,
    modifiers: i64,
) -> Result<(), String> {
    let (code, vk) = char_key_code(ch);

    let mut down = DispatchKeyEventParams::new(DispatchKeyEventType::RawKeyDown);
    down.key = Some(ch.to_string());
    down.code = Some(code.to_string());
    down.windows_virtual_key_code = Some(vk);
    down.modifiers = Some(modifiers);
    page.execute(down)
        .await
        .map_err(|e| format!("Failed to press key {}: {}", ch, e))?;

    // Only send Char event if no modifiers (otherwise it's a shortcut).
    if modifiers == 0 || modifiers == MODIFIER_SHIFT {
        let mut char_event = DispatchKeyEventParams::new(DispatchKeyEventType::Char);
        char_event.text = Some(ch.to_string());
        char_event.modifiers = Some(modifiers);
        let _ = page.execute(char_event).await;
    }

    let mut up = DispatchKeyEventParams::new(DispatchKeyEventType::KeyUp);
    up.key = Some(ch.to_string());
    up.code = Some(code.to_string());
    up.windows_virtual_key_code = Some(vk);
    up.modifiers = Some(modifiers);
    page.execute(up)
        .await
        .map_err(|e| format!("Failed to release key {}: {}", ch, e))?;

    Ok(())
}

pub async fn cdp_press_key(
    key: String,
    include_snapshot: bool,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    let guard = cdp_client.read().await;
    let client = match guard.as_ref() {
        Some(c) => c,
        None => return cdp_error("No CDP connection. Use cdp_connect first."),
    };

    let page = match client.require_page() {
        Ok(p) => p,
        Err(e) => return e,
    };

    drop(guard);

    let combo = match parse_key_combo(&key) {
        Ok(c) => c,
        Err(unknown) => {
            return cdp_error(format!(
                "Unknown modifier '{}'. Use Control, Shift, Alt, or Meta.",
                unknown
            ))
        }
    };
    let modifiers = combo.modifiers;
    let main_key = &combo.main_key;

    // Dispatch modifier key-downs.
    for m in &combo.modifier_names {
        let mut params = DispatchKeyEventParams::new(DispatchKeyEventType::KeyDown);
        params.key = Some(m.clone());
        params.modifiers = Some(modifiers);
        if let Err(e) = page.execute(params).await {
            return cdp_error(format!("Failed to press modifier {}: {}", m, e));
        }
    }

    // Dispatch main key.
    if let Some((key_val, code, vk)) = key_definition(main_key) {
        if let Err(e) = dispatch_named_key(&page, key_val, code, vk, modifiers).await {
            return cdp_error(e);
        }
    } else if main_key.len() == 1 {
        let ch = main_key.chars().next().unwrap_or(' ');
        if let Err(e) = dispatch_char(&page, ch, modifiers).await {
            return cdp_error(e);
        }
    } else {
        return cdp_error(format!(
            "Unknown key '{}'. Use key names like Enter, Tab, ArrowUp, or single characters.",
            main_key
        ));
    }

    // Release modifier keys in reverse order.
    for m in combo.modifier_names.iter().rev() {
        let mut params = DispatchKeyEventParams::new(DispatchKeyEventType::KeyUp);
        params.key = Some(m.clone());
        let _ = page.execute(params).await;
    }

    let result = CallToolResult::success(vec![Content::text(format!("Pressed key: {}", key))]);
    super::finish_after_action(result, include_snapshot, cdp_client).await
}

pub async fn cdp_type_text(
    text: String,
    submit_key: Option<String>,
    cdp_client: Arc<RwLock<Option<CdpClient>>>,
) -> CallToolResult {
    let guard = cdp_client.read().await;
    let client = match guard.as_ref() {
        Some(c) => c,
        None => return cdp_error("No CDP connection. Use cdp_connect first."),
    };

    let page = match client.require_page() {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Validate submit_key before typing so we fail fast on bad input.
    let submit_def = if let Some(ref sk) = submit_key {
        match key_definition(sk) {
            Some(def) => Some(def),
            None => {
                return cdp_error(format!(
                    "Unknown submit key '{}'. Use key names like Enter, Tab, Escape.",
                    sk
                ))
            }
        }
    } else {
        None
    };

    drop(guard);

    for ch in text.chars() {
        if let Err(e) = dispatch_char(&page, ch, 0).await {
            return cdp_error(e);
        }
    }

    if let Some((key_val, code, vk)) = submit_def {
        if let Err(e) = dispatch_named_key(&page, key_val, code, vk, 0).await {
            return cdp_error(e);
        }
    }

    let suffix = submit_key
        .as_ref()
        .map(|k| format!(" + {}", k))
        .unwrap_or_default();
    super::invalidate_snapshot_cache(cdp_client).await;
    CallToolResult::success(vec![Content::text(format!(
        "Typed text \"{}{}\"",
        text, suffix
    ))])
}

#[cfg(test)]
mod tests {
    use super::*;

    // MARK: - key_definition tests

    #[test]
    fn observed_fill_status_accepts_select_value_or_visible_text() {
        let observed = "us\nUnited States";
        assert_eq!(
            super::super::observed_fill_status("select_value", observed, "us"),
            "observed_text=true"
        );
        assert_eq!(
            super::super::observed_fill_status("select_value", observed, "United States"),
            "observed_text=true"
        );
    }

    #[test]
    fn key_definition_returns_enter() {
        let (key, code, vk) = key_definition("Enter").unwrap();
        assert_eq!(key, "Enter");
        assert_eq!(code, "Enter");
        assert_eq!(vk, 13);
    }

    #[test]
    fn key_definition_returns_tab() {
        let (_, _, vk) = key_definition("Tab").unwrap();
        assert_eq!(vk, 9);
    }

    #[test]
    fn key_definition_returns_arrow_keys() {
        assert_eq!(key_definition("ArrowUp").unwrap().2, 38);
        assert_eq!(key_definition("ArrowDown").unwrap().2, 40);
        assert_eq!(key_definition("ArrowLeft").unwrap().2, 37);
        assert_eq!(key_definition("ArrowRight").unwrap().2, 39);
    }

    #[test]
    fn key_definition_returns_space() {
        let (key, code, vk) = key_definition("Space").unwrap();
        assert_eq!(key, " ");
        assert_eq!(code, "Space");
        assert_eq!(vk, 32);
        // Also works with literal space.
        assert!(key_definition(" ").is_some());
    }

    #[test]
    fn key_definition_returns_f_keys() {
        assert_eq!(key_definition("F1").unwrap().2, 112);
        assert_eq!(key_definition("F12").unwrap().2, 123);
    }

    #[test]
    fn key_definition_returns_none_for_single_char() {
        assert!(key_definition("a").is_none());
        assert!(key_definition("Z").is_none());
        assert!(key_definition("1").is_none());
    }

    #[test]
    fn key_definition_returns_none_for_unknown() {
        assert!(key_definition("FooBar").is_none());
        assert!(key_definition("").is_none());
    }

    // MARK: - modifier_bit tests

    #[test]
    fn modifier_bit_returns_correct_values() {
        assert_eq!(modifier_bit("Alt"), Some(1));
        assert_eq!(modifier_bit("Control"), Some(2));
        assert_eq!(modifier_bit("Meta"), Some(4));
        assert_eq!(modifier_bit("Shift"), Some(8));
    }

    #[test]
    fn modifier_bit_returns_none_for_unknown() {
        assert_eq!(modifier_bit("Ctrl"), None);
        assert_eq!(modifier_bit("alt"), None);
        assert_eq!(modifier_bit(""), None);
    }

    // MARK: - parse_key_combo tests

    #[test]
    fn parse_single_key() {
        let combo = parse_key_combo("Enter").unwrap();
        assert_eq!(combo.main_key, "Enter");
        assert_eq!(combo.modifiers, 0);
        assert!(combo.modifier_names.is_empty());
    }

    #[test]
    fn parse_single_character() {
        let combo = parse_key_combo("a").unwrap();
        assert_eq!(combo.main_key, "a");
        assert_eq!(combo.modifiers, 0);
    }

    #[test]
    fn parse_control_a() {
        let combo = parse_key_combo("Control+A").unwrap();
        assert_eq!(combo.main_key, "A");
        assert_eq!(combo.modifiers, MODIFIER_CONTROL);
        assert_eq!(combo.modifier_names, vec!["Control"]);
    }

    #[test]
    fn parse_control_shift_r() {
        let combo = parse_key_combo("Control+Shift+R").unwrap();
        assert_eq!(combo.main_key, "R");
        assert_eq!(combo.modifiers, MODIFIER_CONTROL | MODIFIER_SHIFT);
        assert_eq!(combo.modifier_names, vec!["Control", "Shift"]);
    }

    #[test]
    fn parse_control_plus_key() {
        // "Control++" means Control + the '+' key.
        let combo = parse_key_combo("Control++").unwrap();
        assert_eq!(combo.main_key, "+");
        assert_eq!(combo.modifiers, MODIFIER_CONTROL);
    }

    #[test]
    fn parse_all_modifiers() {
        let combo = parse_key_combo("Alt+Control+Meta+Shift+x").unwrap();
        assert_eq!(combo.main_key, "x");
        assert_eq!(
            combo.modifiers,
            MODIFIER_ALT | MODIFIER_CONTROL | MODIFIER_META | MODIFIER_SHIFT
        );
        assert_eq!(combo.modifier_names.len(), 4);
    }

    #[test]
    fn parse_unknown_modifier_returns_error() {
        let err = parse_key_combo("Ctrl+A").unwrap_err();
        assert_eq!(err, "Ctrl");
    }

    #[test]
    fn parse_meta_enter() {
        let combo = parse_key_combo("Meta+Enter").unwrap();
        assert_eq!(combo.main_key, "Enter");
        assert_eq!(combo.modifiers, MODIFIER_META);
    }

    // MARK: - char_key_code tests

    #[test]
    fn char_key_code_letters() {
        assert_eq!(char_key_code('a'), ("KeyA", 65));
        assert_eq!(char_key_code('Z'), ("KeyZ", 90));
    }

    #[test]
    fn char_key_code_digits() {
        assert_eq!(char_key_code('0'), ("Digit0", 0x30));
        assert_eq!(char_key_code('9'), ("Digit9", 0x39));
    }

    #[test]
    fn char_key_code_punctuation() {
        assert_eq!(char_key_code('+'), ("Equal", 0xBB));
        assert_eq!(char_key_code('-'), ("Minus", 0xBD));
        assert_eq!(char_key_code('/'), ("Slash", 0xBF));
        assert_eq!(char_key_code('.'), ("Period", 0xBE));
        assert_eq!(char_key_code(','), ("Comma", 0xBC));
        assert_eq!(char_key_code(';'), ("Semicolon", 0xBA));
    }

    #[test]
    fn char_key_code_unknown() {
        assert_eq!(char_key_code('€'), ("Unidentified", 0));
    }
}

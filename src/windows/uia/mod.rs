//! Windows UI Automation (UIA) helpers.
//!
//! Uses the Windows Accessibility tree for:
//! - Text search: find UI elements by name (faster than OCR for standard controls)
//! - Snapshot: collect the full UIA tree as a flat list of snapshot nodes

mod find_text;
mod element_at_point;
mod tree;

pub use find_text::*;
pub use element_at_point::*;
pub use tree::*;

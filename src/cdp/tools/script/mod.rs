//! CDP script and snapshot tools: evaluate_script, find_elements,
//! take_dom_snapshot, wait_for, wait_for_page_change.

mod dom;
mod evaluate;
mod scope;
mod summary;
mod wait;

pub use evaluate::cdp_evaluate_script;
pub use summary::{
    cdp_find_elements, cdp_get_element_context, cdp_summarize_page, cdp_take_dom_snapshot,
};
pub use wait::{cdp_wait_for, cdp_wait_for_page_change};

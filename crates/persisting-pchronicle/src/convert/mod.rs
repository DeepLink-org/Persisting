//! Hub conversions: every format converts **only** through [`StorylineDocument`].
//!
//! ```text
//! events.lance (in-memory EventRecord) ──┐
//! agenticmd ─────────────────────────────┼──► storyline ──► …
//! openai_msg ────────────────────────────┤
//! atif ──────────────────────────────────┤
//! actf ──────────────────────────────────┘
//! ```
//!
//! Canonical Event and Storyline Lance are accessed through typed storage APIs;
//! string parsing and encoding stay on the four peripheral document formats.

mod actf;
mod atif;
mod events;

pub(crate) use actf::{actf_to_storylines, storylines_to_actf};
pub(crate) use atif::{atif_collection_to_storylines, storylines_to_atif};
#[cfg(all(test, feature = "lance-store"))]
pub(crate) use atif::{atif_to_storyline, storyline_to_atif};
#[cfg(feature = "lance-store")]
pub(crate) use events::event_storyline_key;
pub use events::{events_to_storyline, project_event_records, storyline_to_events};

pub(crate) fn message_text(message: &serde_json::Value) -> Option<String> {
    match message {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(parts) => {
            let texts: Vec<_> = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join(""))
            }
        }
        _ => None,
    }
}

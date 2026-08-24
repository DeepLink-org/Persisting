//! Hub conversions: every format converts **only** through [`StorylineDocument`].
//!
//! ```text
//! events.lance (in-memory EventRecord) ──┐
//! agenticmd ─────────────────────────────┤
//! openai_msg ────────────────────────────┼──► storyline ──► …
//! actf ──────────────────────────────────┘
//! ```
//!
//! Canonical Event and Storyline Lance are accessed through typed storage APIs.
//! Physical format codecs and Event ↔ Storyline conversion live in `formats/`.
//! This module remains the public conversion hub.

#[cfg(feature = "lance-store")]
pub(crate) use crate::formats::actf::actf_to_storylines;
#[cfg(test)]
pub(crate) use crate::formats::atif::atif_collection_to_storylines;
#[cfg(test)]
pub(crate) use crate::formats::atif::{atif_to_storyline, storyline_to_atif, storylines_to_atif};
#[cfg(feature = "lance-store")]
pub(crate) use crate::formats::events::event_storyline_key;
pub use crate::formats::events::{events_to_storyline, project_event_records, storyline_to_events};

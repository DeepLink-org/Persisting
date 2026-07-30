//! Hub conversions: every format converts **only** through [`StorylineDocument`].
//!
//! ```text
//! events.lance (in-memory EventRecord) ──┐
//! agenticmd ─────────────────────────────┼──► storyline ──► …
//! openai_msg ────────────────────────────┤
//! atif ──────────────────────────────────┘
//! ```
//!
//! [`ChronicleFormat::Events`] has **no string wire form**: `into_storyline` /
//! `from_storyline` / `convert` return an error. Use [`events_to_storyline`] /
//! [`storyline_to_events`] after loading Lance rows into [`EventsDocument`].

mod agenticmd;
mod atif;
mod events;
mod openai_msg;

pub use agenticmd::{agenticmd_to_storyline, storyline_to_agenticmd};
pub use atif::{atif_to_storyline, storyline_to_atif};
pub use events::{events_to_storyline, storyline_to_events};
pub use openai_msg::{openai_msg_to_storyline, storyline_to_openai_msg};

use crate::atif::AtifTrajectory;
use crate::format::ChronicleFormat;
use crate::formats::events::events_lance_only_error;
use crate::formats::storyline::StorylineDocument;
use crate::formats::{
    parse_agenticmd_document, parse_openai_msg_document, parse_storyline_document,
};
use crate::Result;

/// Parse a supported **string** document into the storyline hub.
///
/// [`ChronicleFormat::Events`] always errors — load Lance separately, then call
/// [`events_to_storyline`].
pub fn into_storyline(format: ChronicleFormat, input: &str) -> Result<StorylineDocument> {
    match format {
        ChronicleFormat::Storyline => parse_storyline_document(input),
        ChronicleFormat::Atif => {
            let traj = AtifTrajectory::from_json_str(input)?;
            atif_to_storyline(&traj)
        }
        ChronicleFormat::Events => Err(events_lance_only_error()),
        ChronicleFormat::Agenticmd => {
            let doc = parse_agenticmd_document(input)?;
            agenticmd_to_storyline(&doc)
        }
        ChronicleFormat::OpenaiMsg => {
            let doc = parse_openai_msg_document(input)?;
            openai_msg_to_storyline(&doc)
        }
    }
}

/// Emit a peripheral/hub format as a string.
///
/// [`ChronicleFormat::Events`] always errors — use [`storyline_to_events`] then
/// write Lance via Capture, or [`export_events_jsonl`](crate::export_events_jsonl) for debug dumps.
pub fn from_storyline(format: ChronicleFormat, story: &StorylineDocument) -> Result<String> {
    match format {
        ChronicleFormat::Storyline => story.to_json_string_pretty(),
        ChronicleFormat::Atif => Ok(serde_json::to_string_pretty(&storyline_to_atif(story)?)?),
        ChronicleFormat::Events => Err(events_lance_only_error()),
        ChronicleFormat::Agenticmd => {
            let doc = storyline_to_agenticmd(story)?;
            crate::formats::encode_agenticmd_document(&doc)
        }
        ChronicleFormat::OpenaiMsg => {
            let doc = storyline_to_openai_msg(story)?;
            Ok(serde_json::to_string_pretty(&doc)?)
        }
    }
}

/// Convert between two **string** formats via the storyline hub.
///
/// Any leg involving [`ChronicleFormat::Events`] fails (Lance-only).
pub fn convert(from: ChronicleFormat, to: ChronicleFormat, input: &str) -> Result<String> {
    if from == to {
        if from.is_lance_only() {
            return Err(events_lance_only_error());
        }
        return Ok(input.to_string());
    }
    let story = into_storyline(from, input)?;
    from_storyline(to, &story)
}

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

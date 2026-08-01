//! Named storage formats supported by pChronicle.
//!
//! [`ChronicleFormat::Storyline`] is the **hub** interchange format.
//! Peripheral formats convert only to/from storyline — never pairwise.
//!
//! [`ChronicleFormat::Events`] is **Lance-only** (`events.lance`); it is not a
//! JSON/JSONL string format for convert APIs.

use crate::{Error, Result};
use std::fmt;
use std::str::FromStr;

/// First-class trajectory storage formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChronicleFormat {
    /// ATIF-enhanced Run→Storyline→Turn hub (`storyline`).
    Storyline,
    /// Capture canonical event log — **Lance dataset only** (`events`).
    Events,
    /// Capture TLV markdown dialogue view (`agenticmd`).
    Agenticmd,
    /// dlcapt OpenAI-messages step table (`openai_msg`).
    OpenaiMsg,
    /// Harbor ATIF JSON interchange (`atif`).
    Atif,
}

impl ChronicleFormat {
    pub const ALL: &[ChronicleFormat] = &[
        Self::Storyline,
        Self::Events,
        Self::Agenticmd,
        Self::OpenaiMsg,
        Self::Atif,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Storyline => "storyline",
            Self::Events => "events",
            Self::Agenticmd => "agenticmd",
            Self::OpenaiMsg => "openai_msg",
            Self::Atif => "atif",
        }
    }

    pub fn is_hub(self) -> bool {
        matches!(self, Self::Storyline)
    }

    /// `events` has no string wire form (Lance-only).
    pub fn is_lance_only(self) -> bool {
        matches!(self, Self::Events)
    }

    pub fn origin(self) -> &'static str {
        match self {
            Self::Storyline => "pchronicle (hub)",
            Self::Events => "persisting-gateway (Lance)",
            Self::Agenticmd => "persisting-gateway",
            Self::OpenaiMsg => "dlcapt",
            Self::Atif => "Harbor ATIF",
        }
    }

    pub fn primary_artifact(self) -> &'static str {
        match self {
            Self::Storyline => "storyline.json",
            Self::Events => "events.lance",
            Self::Agenticmd => "*.md",
            Self::OpenaiMsg => "session_steps.json",
            Self::Atif => "*.atif.json / *.atif.jsonl",
        }
    }
}

impl fmt::Display for ChronicleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ChronicleFormat {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "storyline" => Ok(Self::Storyline),
            "events" => Ok(Self::Events),
            "agenticmd" => Ok(Self::Agenticmd),
            "openai_msg" => Ok(Self::OpenaiMsg),
            "atif" => Ok(Self::Atif),
            other => Err(Error::Other(format!(
                "unknown chronicle format '{other}'; expected storyline|events|agenticmd|openai_msg|atif"
            ))),
        }
    }
}

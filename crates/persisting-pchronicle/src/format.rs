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

/// On-disk document formats understood by pChronicle.
///
/// This enum describes physical representations. It does not imply that all
/// formats support the same read or write operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocumentFormat {
    /// Canonical append-only event facts stored in Lance.
    CanonicalEvent,
    /// Storyline runs, steps, tool calls, and objects stored in Lance.
    Storyline,
    /// Human-readable Storyline Markdown.
    AgenticMd,
    /// ATIF JSON, JSONL, or NDJSON.
    Atif,
    /// OpenAI message corpus JSON.
    OpenaiMsg,
    /// ACTF JSON.
    Actf,
}

impl DocumentFormat {
    pub const ALL: &[Self] = &[
        Self::CanonicalEvent,
        Self::Storyline,
        Self::AgenticMd,
        Self::Atif,
        Self::OpenaiMsg,
        Self::Actf,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CanonicalEvent => "canonical-event",
            Self::Storyline => "storyline",
            Self::AgenticMd => "agenticmd",
            Self::Atif => "atif",
            Self::OpenaiMsg => "openai-msg",
            Self::Actf => "actf",
        }
    }
}

impl fmt::Display for DocumentFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DocumentFormat {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "canonical-event" => Ok(Self::CanonicalEvent),
            "storyline" => Ok(Self::Storyline),
            "agenticmd" => Ok(Self::AgenticMd),
            "atif" => Ok(Self::Atif),
            "openai-msg" => Ok(Self::OpenaiMsg),
            "actf" => Ok(Self::Actf),
            other => Err(Error::Other(format!(
                "unknown document format '{other}'; expected canonical-event|storyline|agenticmd|atif|openai-msg|actf"
            ))),
        }
    }
}

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
    /// ACTF v1.0 benchmark task/attempt trajectory document (`actf`).
    Actf,
}

impl ChronicleFormat {
    pub const ALL: &[ChronicleFormat] = &[
        Self::Storyline,
        Self::Events,
        Self::Agenticmd,
        Self::OpenaiMsg,
        Self::Atif,
        Self::Actf,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Storyline => "storyline",
            Self::Events => "events",
            Self::Agenticmd => "agenticmd",
            Self::OpenaiMsg => "openai_msg",
            Self::Atif => "atif",
            Self::Actf => "actf",
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
            Self::Actf => "ACTF v1.0",
        }
    }

    pub fn primary_artifact(self) -> &'static str {
        match self {
            Self::Storyline => "storyline.json",
            Self::Events => "events.lance",
            Self::Agenticmd => "*.md",
            Self::OpenaiMsg => "session_steps.json",
            Self::Atif => "*.atif.json / *.atif.jsonl",
            Self::Actf => "*.actf.json",
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
            "actf" => Ok(Self::Actf),
            other => Err(Error::Other(format!(
                "unknown chronicle format '{other}'; expected storyline|events|agenticmd|openai_msg|atif|actf"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DocumentFormat;
    use std::str::FromStr;

    #[test]
    fn document_format_names_are_canonical_only() {
        let cases = [
            ("canonical-event", DocumentFormat::CanonicalEvent),
            ("storyline", DocumentFormat::Storyline),
            ("agenticmd", DocumentFormat::AgenticMd),
            ("atif", DocumentFormat::Atif),
            ("openai-msg", DocumentFormat::OpenaiMsg),
            ("actf", DocumentFormat::Actf),
        ];

        for (name, expected) in cases {
            assert_eq!(DocumentFormat::from_str(name).unwrap(), expected);
            assert_eq!(expected.to_string(), name);
        }

        for alias in ["events", "lance", "md", "openai_msg", "session_steps"] {
            assert!(DocumentFormat::from_str(alias).is_err(), "accepted {alias}");
        }
    }
}

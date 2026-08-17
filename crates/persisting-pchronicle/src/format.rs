//! Named physical document formats supported by pChronicle.

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

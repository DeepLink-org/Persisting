//! pChronicle 支持的具名物理文档格式。

use crate::{InputIssue, InputResult};
use std::fmt;
use std::str::FromStr;

/// pChronicle 能够打开的磁盘文档格式。
///
/// 枚举只描述物理表示，不暗示所有格式支持相同的读写操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocumentFormat {
    /// Lance 中 append-only 的 Canonical Event 事实。
    CanonicalEvent,
    /// 严格版本化的 Storyline JSON wire。
    Storyline,
    /// Lance 中的 Storyline runs、steps、tool calls 和 objects。
    StorylineLance,
    /// 人类可读的 Storyline Markdown。
    AgenticMd,
    /// ATIF JSON、JSONL 或 NDJSON。
    Atif,
    /// OpenAI message corpus JSON。
    OpenaiMsg,
    /// ACTF JSON。
    Actf,
}

impl DocumentFormat {
    pub const ALL: &[Self] = &[
        Self::CanonicalEvent,
        Self::Storyline,
        Self::StorylineLance,
        Self::AgenticMd,
        Self::Atif,
        Self::OpenaiMsg,
        Self::Actf,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CanonicalEvent => "canonical-event",
            Self::Storyline => "storyline",
            Self::StorylineLance => "storyline-lance",
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
    type Err = InputIssue;

    fn from_str(input: &str) -> InputResult<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "canonical-event" => Ok(Self::CanonicalEvent),
            "storyline" => Ok(Self::Storyline),
            "storyline-lance" => Ok(Self::StorylineLance),
            "agenticmd" => Ok(Self::AgenticMd),
            "atif" => Ok(Self::Atif),
            "openai-msg" => Ok(Self::OpenaiMsg),
            "actf" => Ok(Self::Actf),
            other => Err(InputIssue::invalid(format!(
                "unknown document format '{other}'; expected canonical-event|storyline|storyline-lance|agenticmd|atif|openai-msg|actf"
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
            ("storyline-lance", DocumentFormat::StorylineLance),
            ("agenticmd", DocumentFormat::AgenticMd),
            ("atif", DocumentFormat::Atif),
            ("openai-msg", DocumentFormat::OpenaiMsg),
            ("actf", DocumentFormat::Actf),
        ];

        for (name, expected) in cases {
            assert_eq!(DocumentFormat::from_str(name).unwrap(), expected);
            assert_eq!(expected.to_string(), name);
        }

        for alias in [
            "events",
            "lance",
            "md",
            "openai_msg",
            "session_steps",
            "openclaw",
            "openclaw-events",
        ] {
            assert!(DocumentFormat::from_str(alias).is_err(), "accepted {alias}");
        }
    }
}

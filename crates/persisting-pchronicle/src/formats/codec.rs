//! Document-codec contract for JSON / JSONL / Markdown trajectory files.
//!
//! Storage backends (Canonical Event Lance, Storyline Lance) are not codecs
//! and must not implement this trait.

use std::io::{BufRead, Write};
use std::path::Path;

use crate::format::DocumentFormat;
use crate::formats::storyline::StorylineDocument;
use crate::{InputIssue, InputResult};

/// Provenance passed into a codec decode.
#[derive(Debug, Clone)]
pub struct DocumentSource {
    pub relative_path: String,
}

impl DocumentSource {
    pub fn new(relative_path: impl Into<String>) -> Self {
        Self {
            relative_path: relative_path.into(),
        }
    }
}

/// Limits and provenance for one decode invocation.
#[derive(Debug, Clone, Copy)]
pub struct DecodeContext<'a> {
    pub source: &'a DocumentSource,
    /// Callers should already bound the reader to this limit.
    pub max_file_bytes: u64,
    pub max_record_bytes: usize,
}

impl<'a> DecodeContext<'a> {
    pub fn new(source: &'a DocumentSource) -> Self {
        Self {
            source,
            max_file_bytes: u64::MAX,
            max_record_bytes: usize::MAX,
        }
    }

    #[must_use]
    #[cfg(any(test, feature = "lance-store"))]
    pub fn with_limits(self, max_file_bytes: u64, max_record_bytes: usize) -> Self {
        Self {
            source: self.source,
            max_file_bytes,
            max_record_bytes,
        }
    }
}

/// Counters returned by a successful decode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecodeReport {
    pub documents: usize,
    pub peak_record_bytes: usize,
}

#[cfg_attr(not(any(test, feature = "lance-store")), allow(dead_code))]
pub(crate) fn is_candidate_path(path: &Path, extensions: &[&str]) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name.ends_with(".meta.json") {
        return false;
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|wanted| extension.eq_ignore_ascii_case(wanted))
        })
}

pub(crate) fn emit_stories(
    stories: Vec<StorylineDocument>,
    emit: &mut dyn FnMut(StorylineDocument) -> InputResult<()>,
) -> InputResult<DecodeReport> {
    let documents = stories.len();
    for story in stories {
        emit(story)?;
    }
    Ok(DecodeReport {
        documents,
        peak_record_bytes: 0,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatCapabilities {
    pub decode: bool,
    pub encode: bool,
    pub direct_query: bool,
    pub streaming_input: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProbeConfidence {
    None,
    PathHint,
    ContentFingerprint,
}

/// File-oriented trajectory codec. One physical format owns identity, probe,
/// decode, and optional encode.
pub trait TrajectoryFormat: Send + Sync {
    fn id(&self) -> DocumentFormat;

    #[cfg_attr(not(any(test, feature = "lance-store")), allow(dead_code))]
    fn extensions(&self) -> &'static [&'static str];

    fn capabilities(&self) -> FormatCapabilities;

    #[cfg_attr(not(any(test, feature = "lance-store")), allow(dead_code))]
    fn is_candidate(&self, path: &Path) -> bool {
        is_candidate_path(path, self.extensions())
    }

    fn probe(&self, path: Option<&Path>, content: &[u8]) -> InputResult<ProbeConfidence>;

    fn decode(
        &self,
        reader: &mut dyn BufRead,
        ctx: &DecodeContext<'_>,
        emit: &mut dyn FnMut(StorylineDocument) -> InputResult<()>,
    ) -> InputResult<DecodeReport>;

    fn encode(&self, stories: &[StorylineDocument], output: &mut dyn Write) -> InputResult<()> {
        let _ = (stories, output);
        Err(InputIssue::unsupported(format!(
            "{} is decode-only",
            self.id()
        )))
    }
}

pub fn decode_all(
    format: &dyn TrajectoryFormat,
    reader: &mut dyn BufRead,
    source: &DocumentSource,
) -> InputResult<Vec<StorylineDocument>> {
    decode_all_with(format, reader, &DecodeContext::new(source)).map(|(stories, _)| stories)
}

pub fn decode_all_with(
    format: &dyn TrajectoryFormat,
    reader: &mut dyn BufRead,
    ctx: &DecodeContext<'_>,
) -> InputResult<(Vec<StorylineDocument>, DecodeReport)> {
    let mut stories = Vec::new();
    let report = format.decode(reader, ctx, &mut |story| {
        stories.push(story);
        Ok(())
    })?;
    Ok((stories, report))
}

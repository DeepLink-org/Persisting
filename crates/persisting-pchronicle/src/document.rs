//! Unified read/query entrypoint for pChronicle's physical document formats.

#[cfg(feature = "lance-store")]
use std::path::Path;

#[cfg(feature = "lance-store")]
use datafusion::prelude::SessionContext;

pub use crate::agenticmd::{
    agenticmd_block_count, agenticmd_structural_issues, count_agenticmd_role, encode_agenticmd,
    index_agenticmd_path, list_agenticmd_paths, parse_agenticmd,
    rewrite_agenticmd_storyline_metadata, upsert_agenticmd_turn, write_agenticmd_storyline,
    AgenticmdFileIndex,
};
pub use crate::convert::{
    actf_to_storyline, actf_to_storylines, atif_to_storyline, events_to_storyline,
    openai_msg_to_storyline, project_event_records, storyline_to_actf, storyline_to_atif,
    storyline_to_events, storyline_to_openai_msg, storylines_to_actf,
};
pub use crate::error::{classify_error, Error, ErrorCode, Result};
pub use crate::format::DocumentFormat;
pub use crate::formats::{
    detect_format, events_lance_only_message, export_events_json_pretty, export_events_jsonl,
    is_lossless_openai_storyline, parse_actf_document, parse_openai_msg_corpus_value,
    parse_openai_msg_document, parse_storyline_document, recover_openai_msg_files,
};
pub use crate::interop::{events_to_har, events_to_otlp_json, otlp_json_to_events};

#[cfg(feature = "lance-store")]
use crate::formats::StorylineDocument;

/// Static filter pushdown guarantee exposed by a document source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "lance-store")]
pub enum FilterPushdown {
    Unsupported,
    Inexact,
    Exact,
    /// The guarantee depends on the concrete expression and table columns.
    ExpressionDependent,
}

/// Logical tables registered by a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "lance-store")]
pub enum QueryTables {
    Events,
    Storyline,
}

/// Truthful optimization capabilities for one opened provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "lance-store")]
pub struct QueryCapabilities {
    pub projection_pushdown: bool,
    pub filter_pushdown: FilterPushdown,
    pub limit_pushdown: bool,
    pub scalar_indexes: bool,
    pub streaming_decode: bool,
    pub late_content_materialization: bool,
    pub snapshot_consistent: bool,
}

/// Maximum Storyline rows retained by the convenience materialization API.
#[cfg(feature = "lance-store")]
pub const DEFAULT_DOCUMENT_MATERIALIZE_ROWS: usize = 10_000;
/// Maximum serialized Storyline bytes retained by the convenience materialization API.
#[cfg(feature = "lance-store")]
pub const DEFAULT_DOCUMENT_MATERIALIZE_BYTES: usize = 64 * 1024 * 1024;

/// One opened physical document source. Provider variants remain private.
#[derive(Debug)]
#[cfg(feature = "lance-store")]
pub struct DocumentSource {
    pub(crate) inner: crate::store::DocumentSourceImpl,
}

/// Open one of the six physical pChronicle document formats.
#[cfg(feature = "lance-store")]
pub async fn open_document(format: DocumentFormat, path: &Path) -> Result<DocumentSource> {
    Ok(DocumentSource {
        inner: crate::store::open_document_source(format, path).await?,
    })
}

#[cfg(feature = "lance-store")]
impl DocumentSource {
    pub fn format(&self) -> DocumentFormat {
        self.inner.format()
    }

    pub fn capabilities(&self) -> QueryCapabilities {
        self.inner.capabilities()
    }

    /// Materialize all Storylines, failing closed when the aggregate budget is exceeded.
    pub async fn project_storylines(&self) -> Result<Vec<StorylineDocument>> {
        self.inner.project_storylines().await
    }

    /// Visit Storylines one at a time without retaining the complete source.
    pub async fn for_each_storyline<F>(&self, on_storyline: F) -> Result<()>
    where
        F: FnMut(StorylineDocument) -> Result<()>,
    {
        self.inner.for_each_storyline(on_storyline).await
    }

    pub fn register_datafusion(&self, context: &SessionContext) -> Result<QueryTables> {
        self.inner.register_datafusion(context)
    }
}

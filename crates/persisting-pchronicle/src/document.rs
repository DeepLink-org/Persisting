//! Unified read/query entrypoint for pChronicle's physical document formats.

use std::path::Path;

use datafusion::prelude::SessionContext;

use crate::{DocumentFormat, Result, StorylineDocument};

/// Static filter pushdown guarantee exposed by a document source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterPushdown {
    Unsupported,
    Inexact,
    Exact,
    /// The guarantee depends on the concrete expression and table columns.
    ExpressionDependent,
}

/// Logical tables registered by a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryTables {
    Events,
    Storyline,
}

/// Truthful optimization capabilities for one opened provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
pub const DEFAULT_DOCUMENT_MATERIALIZE_ROWS: usize = 10_000;
/// Maximum serialized Storyline bytes retained by the convenience materialization API.
pub const DEFAULT_DOCUMENT_MATERIALIZE_BYTES: usize = 64 * 1024 * 1024;

/// One opened physical document source. Provider variants remain private.
#[derive(Debug)]
pub struct DocumentSource {
    pub(crate) inner: crate::store::DocumentSourceImpl,
}

/// Open one of the six physical pChronicle document formats.
pub async fn open_document(format: DocumentFormat, path: &Path) -> Result<DocumentSource> {
    Ok(DocumentSource {
        inner: crate::store::open_document_source(format, path).await?,
    })
}

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

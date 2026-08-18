//! Private provider variants behind the public `DocumentSource` API.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use datafusion::arrow::array::{Array, StringArray};
use datafusion::prelude::SessionContext;
use futures::TryStreamExt;

use crate::agenticmd::parse_agenticmd;
use crate::convert::{actf_to_storylines, project_event_records};
use crate::document::{
    FilterPushdown, QueryCapabilities, QueryTables, DEFAULT_DOCUMENT_MATERIALIZE_BYTES,
    DEFAULT_DOCUMENT_MATERIALIZE_ROWS,
};
use crate::error::{Error, Result};
use crate::format::DocumentFormat;
use crate::formats::actf::ActfDocument;
use crate::formats::{parse_openai_msg_corpus_value, StorylineDocument};

use super::{
    AgenticMdDataSource, FileTrajectoryDataSource, LocalQueryManifest, RawEventDataSource,
    StorylineDataSource, DEFAULT_MAX_EVENT_FALLBACK_BYTES, DEFAULT_MAX_EVENT_FALLBACK_ROWS,
};

pub(crate) trait QueryDocumentSource {
    fn format(&self) -> DocumentFormat;
    fn tables(&self) -> QueryTables;
    fn capabilities(&self) -> QueryCapabilities;
    fn register(&self, context: &SessionContext) -> Result<()>;
}

#[derive(Debug)]
pub(crate) enum DocumentSourceImpl {
    Events {
        path: PathBuf,
        source: RawEventDataSource,
    },
    Storyline {
        path: PathBuf,
        source: Box<StorylineDataSource>,
    },
    AgenticMd {
        path: PathBuf,
        story: Box<StorylineDocument>,
        source: AgenticMdDataSource,
    },
    Files {
        format: DocumentFormat,
        path: PathBuf,
        manifest: LocalQueryManifest,
        source: FileTrajectoryDataSource,
    },
}

pub(crate) async fn open_document_source(
    format: DocumentFormat,
    path: &Path,
) -> Result<DocumentSourceImpl> {
    let path = path.to_path_buf();
    match format {
        DocumentFormat::CanonicalEvent => Ok(DocumentSourceImpl::Events {
            source: RawEventDataSource::open(&path).await.map_err(other)?,
            path,
        }),
        DocumentFormat::Storyline => {
            let source = StorylineDataSource::open(&path).await.map_err(other)?;
            Ok(DocumentSourceImpl::Storyline {
                path,
                source: Box::new(source),
            })
        }
        DocumentFormat::AgenticMd => {
            let input = std::fs::read_to_string(&path).map_err(Error::from)?;
            let story = parse_agenticmd(&input).map_err(|error| with_path(error.into(), &path))?;
            let source = AgenticMdDataSource::new(&story).map_err(other)?;
            Ok(DocumentSourceImpl::AgenticMd {
                path,
                story: Box::new(story),
                source,
            })
        }
        DocumentFormat::Atif | DocumentFormat::OpenaiMsg | DocumentFormat::Actf => {
            let manifest = LocalQueryManifest::for_format(&path, format).map_err(other)?;
            let source =
                FileTrajectoryDataSource::from_manifest(manifest.clone()).map_err(other)?;
            debug_assert_eq!(source.format(), format);
            Ok(DocumentSourceImpl::Files {
                format,
                path,
                manifest,
                source,
            })
        }
    }
}

impl DocumentSourceImpl {
    pub(crate) fn format(&self) -> DocumentFormat {
        QueryDocumentSource::format(self)
    }

    pub(crate) fn capabilities(&self) -> QueryCapabilities {
        QueryDocumentSource::capabilities(self)
    }

    pub(crate) fn register_datafusion(&self, context: &SessionContext) -> Result<QueryTables> {
        QueryDocumentSource::register(self, context)?;
        Ok(QueryDocumentSource::tables(self))
    }

    pub(crate) async fn project_storylines(&self) -> Result<Vec<StorylineDocument>> {
        let mut stories = Vec::new();
        let mut retained_rows = 0usize;
        let mut retained_bytes = 0usize;
        self.for_each_storyline(|story| {
            retained_rows = retained_rows
                .checked_add(story_rows(&story))
                .ok_or_else(|| budget_error(self, "row count overflow"))?;
            if retained_rows > DEFAULT_DOCUMENT_MATERIALIZE_ROWS {
                return Err(budget_error(
                    self,
                    &format!(
                        "materialized rows {retained_rows} exceed {DEFAULT_DOCUMENT_MATERIALIZE_ROWS}"
                    ),
                ));
            }
            retained_bytes = retained_bytes
                .checked_add(serde_json::to_vec(&story)?.len())
                .ok_or_else(|| budget_error(self, "byte count overflow"))?;
            if retained_bytes > DEFAULT_DOCUMENT_MATERIALIZE_BYTES {
                return Err(budget_error(
                    self,
                    &format!(
                        "materialized bytes {retained_bytes} exceed {DEFAULT_DOCUMENT_MATERIALIZE_BYTES}"
                    ),
                ));
            }
            stories.push(story);
            Ok(())
        })
        .await?;
        Ok(stories)
    }

    pub(crate) async fn for_each_storyline<F>(&self, mut on_storyline: F) -> Result<()>
    where
        F: FnMut(StorylineDocument) -> Result<()>,
    {
        match self {
            Self::AgenticMd { story, .. } => on_storyline(story.as_ref().clone()),
            Self::Files {
                format,
                manifest,
                source,
                ..
            } => for_each_file_storyline(*format, manifest, source.max_file_bytes(), on_storyline),
            Self::Storyline { source, .. } => {
                let context = SessionContext::new();
                source.register(&context).map_err(other)?;
                let mut batches = context
                    .sql(
                        "SELECT document_id FROM runs \
                         ORDER BY storage_ordinal, document_id",
                    )
                    .await
                    .map_err(other)?
                    .execute_stream()
                    .await
                    .map_err(other)?;
                while let Some(batch) = batches.try_next().await.map_err(other)? {
                    for document_id in strings_from_batch(&batch, "document_id")? {
                        on_storyline(read_pinned_storyline(&context, &document_id).await?)?;
                    }
                }
                Ok(())
            }
            Self::Events { source, .. } => {
                let context = SessionContext::new();
                source.register(&context).map_err(other)?;
                let mut batches = context
                    .sql(
                        "SELECT DISTINCT session_id FROM events \
                         WHERE session_id IS NOT NULL ORDER BY session_id",
                    )
                    .await
                    .map_err(other)?
                    .execute_stream()
                    .await
                    .map_err(other)?;
                while let Some(batch) = batches.try_next().await.map_err(other)? {
                    for session_id in strings_from_batch(&batch, "session_id")? {
                        let requested = BTreeSet::from([session_id]);
                        let records = source
                            .read_records_for_storylines_bounded(
                                &requested,
                                DEFAULT_MAX_EVENT_FALLBACK_ROWS,
                                DEFAULT_MAX_EVENT_FALLBACK_BYTES,
                            )
                            .await
                            .map_err(|error| {
                                if error.to_string().contains("exceeds max_event_fallback") {
                                    budget_error(self, &error.to_string())
                                } else {
                                    other(error)
                                }
                            })?;
                        on_storyline(project_event_records(&records)?)?;
                    }
                }
                Ok(())
            }
        }
    }

    pub(crate) fn source_count(&self) -> usize {
        match self {
            Self::Files { source, .. } => source.file_count(),
            _ => 1,
        }
    }

    pub(crate) fn file_metrics(&self) -> Option<super::FileTrajectoryQueryMetrics> {
        match self {
            Self::Files { source, .. } => Some(source.metrics()),
            _ => None,
        }
    }

    pub(crate) fn event_snapshot(&self) -> Option<&super::EventFactSnapshot> {
        match self {
            Self::Events { source, .. } => Some(source.fact_snapshot()),
            _ => None,
        }
    }

    pub(crate) fn storyline_generation(&self) -> Option<&str> {
        match self {
            Self::Storyline { source, .. } => Some(source.generation()),
            _ => None,
        }
    }
}

impl QueryDocumentSource for DocumentSourceImpl {
    fn format(&self) -> DocumentFormat {
        match self {
            Self::Events { .. } => DocumentFormat::CanonicalEvent,
            Self::Storyline { .. } => DocumentFormat::Storyline,
            Self::AgenticMd { .. } => DocumentFormat::AgenticMd,
            Self::Files { format, .. } => *format,
        }
    }

    fn tables(&self) -> QueryTables {
        match self {
            Self::Events { .. } => QueryTables::Events,
            _ => QueryTables::Storyline,
        }
    }

    fn capabilities(&self) -> QueryCapabilities {
        match self.format() {
            DocumentFormat::CanonicalEvent => QueryCapabilities {
                projection_pushdown: true,
                filter_pushdown: FilterPushdown::Exact,
                limit_pushdown: true,
                scalar_indexes: true,
                streaming_decode: true,
                late_content_materialization: false,
                snapshot_consistent: true,
            },
            DocumentFormat::Storyline => QueryCapabilities {
                projection_pushdown: true,
                filter_pushdown: FilterPushdown::ExpressionDependent,
                limit_pushdown: true,
                scalar_indexes: true,
                streaming_decode: false,
                late_content_materialization: true,
                snapshot_consistent: true,
            },
            DocumentFormat::Atif => QueryCapabilities {
                projection_pushdown: true,
                filter_pushdown: FilterPushdown::Inexact,
                limit_pushdown: true,
                scalar_indexes: false,
                streaming_decode: true,
                late_content_materialization: false,
                snapshot_consistent: false,
            },
            DocumentFormat::OpenaiMsg | DocumentFormat::Actf => QueryCapabilities {
                projection_pushdown: true,
                filter_pushdown: FilterPushdown::Unsupported,
                limit_pushdown: true,
                scalar_indexes: false,
                streaming_decode: false,
                late_content_materialization: false,
                snapshot_consistent: false,
            },
            DocumentFormat::AgenticMd => QueryCapabilities {
                projection_pushdown: true,
                filter_pushdown: FilterPushdown::Unsupported,
                limit_pushdown: false,
                scalar_indexes: false,
                streaming_decode: false,
                late_content_materialization: false,
                snapshot_consistent: false,
            },
        }
    }

    fn register(&self, context: &SessionContext) -> Result<()> {
        match self {
            Self::Events { source, .. } => source.register(context).map_err(other),
            Self::Storyline { source, .. } => source.register(context).map_err(other),
            Self::AgenticMd { source, .. } => source.register(context).map_err(other),
            Self::Files { source, .. } => source.register(context).map_err(other),
        }
    }
}

fn for_each_file_storyline<F>(
    format: DocumentFormat,
    manifest: &LocalQueryManifest,
    max_file_bytes: u64,
    mut on_storyline: F,
) -> Result<()>
where
    F: FnMut(StorylineDocument) -> Result<()>,
{
    match format {
        DocumentFormat::Atif => {
            for file in manifest.files() {
                let input = read_bounded_file(file, max_file_bytes, format)?;
                let input = std::str::from_utf8(&input)
                    .map_err(|error| Error::Other(format!("ATIF input is not UTF-8: {error}")))?;
                for story in super::files::parse_atif_storylines(input).map_err(other)? {
                    on_storyline(story)?;
                }
            }
        }
        DocumentFormat::OpenaiMsg => {
            for file in manifest.files() {
                let input = read_bounded_file(file, max_file_bytes, format)?;
                let document = serde_json::from_slice(&input)?;
                for story in parse_openai_msg_corpus_value(&document, file.relative_path())? {
                    on_storyline(story)?;
                }
            }
        }
        DocumentFormat::Actf => {
            for file in manifest.files() {
                let input = read_bounded_file(file, max_file_bytes, format)?;
                let input = std::str::from_utf8(&input)
                    .map_err(|error| Error::Other(format!("ACTF input is not UTF-8: {error}")))?;
                let document = ActfDocument::from_json_str(input)?;
                for story in actf_to_storylines(&document)? {
                    on_storyline(story)?;
                }
            }
        }
        DocumentFormat::CanonicalEvent | DocumentFormat::Storyline | DocumentFormat::AgenticMd => {
            return Err(Error::Other(format!(
                "{format} is not a file-backed trajectory document format"
            )));
        }
    }
    Ok(())
}

fn read_bounded_file(
    file: &super::LocalQueryInputFile,
    max_file_bytes: u64,
    format: DocumentFormat,
) -> Result<Vec<u8>> {
    file.validate_unchanged().map_err(other)?;
    if file.size_bytes() > max_file_bytes {
        return Err(Error::Other(format!(
            "{format} input {} is {} bytes, exceeding max_file_bytes {max_file_bytes}",
            file.path().display(),
            file.size_bytes()
        )));
    }
    let input = std::fs::read(file.path())?;
    if input.len() as u64 > max_file_bytes {
        return Err(Error::Other(format!(
            "{format} input {} exceeded max_file_bytes {max_file_bytes} while reading",
            file.path().display()
        )));
    }
    file.validate_unchanged().map_err(other)?;
    Ok(input)
}

fn strings_from_batch(
    batch: &datafusion::arrow::record_batch::RecordBatch,
    column: &str,
) -> Result<Vec<String>> {
    let mut values = Vec::new();
    let index = batch.schema().index_of(column).map_err(other)?;
    let array = batch
        .column(index)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| Error::Other(format!("query column '{column}' is not Utf8")))?;
    for row in 0..array.len() {
        if !array.is_null(row) {
            values.push(array.value(row).to_string());
        }
    }
    Ok(values)
}

async fn read_pinned_storyline(
    context: &SessionContext,
    document_id: &str,
) -> Result<StorylineDocument> {
    let literal = document_id.replace('\'', "''");
    let runs = context
        .sql(&format!(
            "SELECT * FROM runs WHERE document_id = '{literal}'"
        ))
        .await
        .map_err(other)?
        .collect()
        .await
        .map_err(other)?;
    let steps = context
        .sql(&format!(
            "SELECT * FROM steps WHERE document_id = '{literal}' ORDER BY step_id"
        ))
        .await
        .map_err(other)?
        .collect()
        .await
        .map_err(other)?;
    let tool_calls = context
        .sql(&format!(
            "SELECT * FROM tool_calls WHERE document_id = '{literal}' ORDER BY step_id, call_index"
        ))
        .await
        .map_err(other)?
        .collect()
        .await
        .map_err(other)?;

    let mut run_rows = Vec::new();
    for batch in &runs {
        run_rows.extend(super::story_runs_from_batch(batch).map_err(other)?);
    }
    if run_rows.len() != 1 {
        return Err(Error::Other(format!(
            "pinned Storyline source returned {} run rows for document_id '{document_id}'",
            run_rows.len()
        )));
    }
    let mut step_rows = Vec::new();
    for batch in &steps {
        step_rows.extend(super::story_steps_from_batch(batch).map_err(other)?);
    }
    let mut tool_call_rows = Vec::new();
    for batch in &tool_calls {
        tool_call_rows.extend(super::story_tool_calls_from_batch(batch).map_err(other)?);
    }
    crate::store::reconstruct_storyline(crate::store::StorylineTables {
        run: run_rows.remove(0),
        steps: step_rows,
        tool_calls: tool_call_rows,
    })
}

fn story_rows(story: &StorylineDocument) -> usize {
    1usize.saturating_add(story.turns.len()).saturating_add(
        story
            .turns
            .iter()
            .map(|turn| turn.tool_calls.as_ref().map_or(0, Vec::len))
            .sum::<usize>(),
    )
}

fn budget_error(source: &DocumentSourceImpl, budget: &str) -> Error {
    Error::SourceBudgetExceeded {
        format: source.format(),
        path: Some(source.path().to_path_buf()),
        budget: budget.into(),
    }
}

impl DocumentSourceImpl {
    fn path(&self) -> &Path {
        match self {
            Self::Events { path, .. }
            | Self::Storyline { path, .. }
            | Self::AgenticMd { path, .. }
            | Self::Files { path, .. } => path,
        }
    }
}

fn other(error: impl std::fmt::Display) -> Error {
    Error::Other(error.to_string())
}

fn with_path(error: Error, path: &Path) -> Error {
    match error {
        Error::InvalidDocument {
            format,
            location,
            message,
            ..
        } => Error::InvalidDocument {
            format,
            path: Some(path.to_path_buf()),
            location,
            message,
        },
        error => error,
    }
}

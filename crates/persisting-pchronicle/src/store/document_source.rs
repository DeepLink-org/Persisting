//! Private provider variants behind the public `DocumentSource` API.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context as _, Result};
use datafusion::arrow::array::{Array, StringArray};
use datafusion::prelude::SessionContext;
use futures::TryStreamExt;

use crate::agenticmd::parse_agenticmd;
use crate::convert::{actf_to_storylines, project_event_records};
use crate::document::encode_json_storylines;
use crate::document::{
    DEFAULT_DOCUMENT_MATERIALIZE_BYTES, DEFAULT_DOCUMENT_MATERIALIZE_ROWS, FilterPushdown,
    QueryCapabilities, QueryTables, decode_json_storylines,
};
use crate::format::DocumentFormat;
use crate::formats::actf::ActfDocument;
use crate::formats::{StorylineDocument, parse_openai_msg_corpus_value};

use super::files::DEFAULT_LOCAL_QUERY_MAX_RECORD_BYTES;
use super::{
    AgenticMdDataSource, AtifReader, DEFAULT_MAX_EVENT_FALLBACK_BYTES,
    DEFAULT_MAX_EVENT_FALLBACK_ROWS, FileTrajectoryDataSource, LocalQueryManifest,
    RawEventDataSource, StorylineDataSource, datafusion_bridge::from_datafusion,
};

#[derive(Debug)]
pub(crate) enum DocumentSourceImpl {
    Events {
        source: RawEventDataSource,
    },
    StorylineLance {
        source: Box<StorylineDataSource>,
    },
    AgenticMd {
        raw: String,
        story: Box<StorylineDocument>,
        source: AgenticMdDataSource,
    },
    Files {
        format: DocumentFormat,
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
            source: RawEventDataSource::open(&path).await?,
        }),
        DocumentFormat::StorylineLance => {
            let source = StorylineDataSource::open(&path).await?;
            Ok(DocumentSourceImpl::StorylineLance {
                source: Box::new(source),
            })
        }
        DocumentFormat::AgenticMd => {
            let input = std::fs::read_to_string(&path)
                .with_context(|| format!("read AgenticMD document {}", path.display()))?;
            let story = parse_agenticmd(&input)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("parse AgenticMD document {}", path.display()))?;
            let source = AgenticMdDataSource::new(&story)?;
            Ok(DocumentSourceImpl::AgenticMd {
                raw: input,
                story: Box::new(story),
                source,
            })
        }
        other => {
            anyhow::ensure!(
                crate::formats::registry::supports_direct_query(other),
                "unsupported document source format '{other}' in {}",
                path.display()
            );
            let manifest = LocalQueryManifest::for_format(&path, other)?;
            let source = FileTrajectoryDataSource::from_manifest(manifest.clone())?;
            debug_assert_eq!(source.format(), other);
            Ok(DocumentSourceImpl::Files {
                format: other,
                manifest,
                source,
            })
        }
    }
}

impl DocumentSourceImpl {
    pub(crate) fn format(&self) -> DocumentFormat {
        match self {
            Self::Events { .. } => DocumentFormat::CanonicalEvent,
            Self::StorylineLance { .. } => DocumentFormat::StorylineLance,
            Self::AgenticMd { .. } => DocumentFormat::AgenticMd,
            Self::Files { format, .. } => *format,
        }
    }

    pub(crate) fn capabilities(&self) -> QueryCapabilities {
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
            DocumentFormat::StorylineLance => QueryCapabilities {
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
            DocumentFormat::Actf => QueryCapabilities {
                projection_pushdown: true,
                filter_pushdown: FilterPushdown::Inexact,
                limit_pushdown: true,
                scalar_indexes: false,
                streaming_decode: true,
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
            _ => QueryCapabilities {
                projection_pushdown: true,
                filter_pushdown: FilterPushdown::Unsupported,
                limit_pushdown: true,
                scalar_indexes: false,
                streaming_decode: false,
                late_content_materialization: false,
                snapshot_consistent: false,
            },
        }
    }

    pub(crate) fn register_datafusion(&self, context: &SessionContext) -> Result<QueryTables> {
        self.register(context)?;
        Ok(self.tables())
    }

    /// Register format-shaped virtual tables (`atif`, `storyline`, ...).
    ///
    /// This is intentionally separate from the normalized tables so existing
    /// queries keep their schema and performance characteristics.
    pub(crate) async fn register_virtual_tables(&self, context: &SessionContext) -> Result<()> {
        for format in super::virtual_document::formats() {
            let Some(table_name) = super::virtual_document::table_name(format) else {
                continue;
            };
            let provider: std::sync::Arc<dyn datafusion::datasource::TableProvider> = match self {
                Self::Files {
                    format: source_format,
                    manifest,
                    source,
                    ..
                } if *source_format == format => {
                    std::sync::Arc::new(super::virtual_document::FileProvider::new(
                        format,
                        std::sync::Arc::new(manifest.clone()),
                        source.max_file_bytes(),
                        source.provider(super::StorylineTableKind::Runs),
                        source.provider(super::StorylineTableKind::Steps),
                        source.provider(super::StorylineTableKind::ToolCalls),
                    ))
                }
                _ => {
                    let rows = self.virtual_rows(format).await?;
                    super::virtual_document::provider(&rows)?
                }
            };
            context
                .register_table(table_name, provider)
                .map_err(|error| from_datafusion("register format virtual table", error))?;
        }
        Ok(())
    }

    pub(crate) async fn virtual_rows(
        &self,
        format: DocumentFormat,
    ) -> Result<Vec<(String, String)>> {
        match self {
            Self::Files {
                format: source_format,
                manifest,
                source,
                ..
            } if *source_format == format => {
                virtual_rows_for_files(format, manifest, source.max_file_bytes(), None)
            }
            Self::Events { source } if format == DocumentFormat::CanonicalEvent => {
                let context = SessionContext::new();
                source.register(&context)?;
                let mut stream = context
                    .sql("SELECT * FROM events ORDER BY seq")
                    .await?
                    .execute_stream()
                    .await?;
                let mut rows = Vec::new();
                while let Some(batch) = stream.try_next().await? {
                    for row in super::event_rows_from_batch(&batch)? {
                        rows.push((
                            row.event_id.unwrap_or_else(|| row.seq.to_string()),
                            row.payload_json,
                        ));
                    }
                }
                Ok(rows)
            }
            Self::AgenticMd { raw, story, .. } if format == DocumentFormat::AgenticMd => {
                let value = serde_json::json!({
                    "format": "agenticmd",
                    "content": raw,
                    "storyline": story.as_ref(),
                });
                Ok(vec![(story.document_id().to_string(), value.to_string())])
            }
            Self::StorylineLance { .. } if format == DocumentFormat::Storyline => {
                let stories = self.project_storylines().await?;
                stories
                    .iter()
                    .map(|story| {
                        let value = encode_json_storylines(
                            DocumentFormat::Storyline,
                            std::slice::from_ref(story),
                        )?;
                        Ok((story.document_id().to_string(), value.to_string()))
                    })
                    .collect()
            }
            _ => Ok(Vec::new()),
        }
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
            Self::StorylineLance { source, .. } => {
                let context = SessionContext::new();
                source.register(&context)?;
                let mut batches = context
                    .sql(
                        "SELECT document_id FROM runs \
                         ORDER BY storage_ordinal, document_id",
                    )
                    .await
                    .map_err(|error| from_datafusion("plan Storyline document scan", error))?
                    .execute_stream()
                    .await
                    .map_err(|error| from_datafusion("start Storyline document scan", error))?;
                while let Some(batch) = batches
                    .try_next()
                    .await
                    .map_err(|error| from_datafusion("stream Storyline documents", error))?
                {
                    for document_id in strings_from_batch(&batch, "document_id")? {
                        on_storyline(read_pinned_storyline(&context, &document_id).await?)?;
                    }
                }
                Ok(())
            }
            Self::Events { source, .. } => {
                let context = SessionContext::new();
                source.register(&context)?;
                let mut batches = context
                    .sql(
                        "SELECT DISTINCT session_id FROM events \
                         WHERE session_id IS NOT NULL ORDER BY session_id",
                    )
                    .await
                    .map_err(|error| from_datafusion("plan event document scan", error))?
                    .execute_stream()
                    .await
                    .map_err(|error| from_datafusion("start event document scan", error))?;
                while let Some(batch) = batches
                    .try_next()
                    .await
                    .map_err(|error| from_datafusion("stream event documents", error))?
                {
                    for session_id in strings_from_batch(&batch, "session_id")? {
                        let requested = BTreeSet::from([session_id]);
                        let records = source
                            .read_records_for_storylines_bounded(
                                &requested,
                                DEFAULT_MAX_EVENT_FALLBACK_ROWS,
                                DEFAULT_MAX_EVENT_FALLBACK_BYTES,
                            )
                            .await?;
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
            Self::StorylineLance { source, .. } => Some(source.generation()),
            _ => None,
        }
    }

    fn tables(&self) -> QueryTables {
        match self {
            Self::Events { .. } => QueryTables::Events,
            _ => QueryTables::Storyline,
        }
    }

    fn register(&self, context: &SessionContext) -> Result<()> {
        match self {
            Self::Events { source, .. } => source.register(context),
            Self::StorylineLance { source, .. } => source.register(context),
            Self::AgenticMd { source, .. } => source.register(context),
            Self::Files { source, .. } => source.register(context),
        }
    }
}

pub(crate) fn for_each_file_storyline<F>(
    format: DocumentFormat,
    manifest: &LocalQueryManifest,
    max_file_bytes: u64,
    mut on_storyline: F,
) -> Result<()>
where
    F: FnMut(StorylineDocument) -> Result<()>,
{
    match format {
        DocumentFormat::Storyline => {
            for file in manifest.files() {
                let input = read_bounded_file(file, max_file_bytes, format)?;
                let input = std::str::from_utf8(&input).context("Storyline input is not UTF-8")?;
                for story in
                    decode_json_storylines(DocumentFormat::Storyline, input, file.relative_path())
                        .map_err(anyhow::Error::from)?
                {
                    on_storyline(story)?;
                }
            }
        }
        DocumentFormat::Atif => {
            for story in AtifReader::from_manifest(
                manifest,
                max_file_bytes,
                DEFAULT_LOCAL_QUERY_MAX_RECORD_BYTES,
            ) {
                on_storyline(story?)?;
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
                let input = std::str::from_utf8(&input).context("ACTF input is not UTF-8")?;
                let document = ActfDocument::from_json_str(input)?;
                for story in actf_to_storylines(&document)? {
                    on_storyline(story)?;
                }
            }
        }
        other => {
            let handler = crate::formats::registry::get(other).ok_or_else(|| {
                anyhow::anyhow!("{other} is not a file-backed trajectory document format")
            })?;
            anyhow::ensure!(
                handler.capabilities().direct_query,
                "{other} is not a file-backed trajectory document format"
            );
            for file in manifest.files() {
                let input = read_bounded_file(file, max_file_bytes, other)?;
                let mut reader = std::io::Cursor::new(input);
                for story in crate::formats::codec::decode_all(
                    handler,
                    &mut reader,
                    &crate::formats::codec::DocumentSource::new(file.relative_path()),
                )
                .map_err(anyhow::Error::from)?
                {
                    on_storyline(story)?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn virtual_rows_for_files(
    format: DocumentFormat,
    manifest: &LocalQueryManifest,
    max_file_bytes: u64,
    candidate_ids: Option<&std::collections::BTreeSet<String>>,
) -> Result<Vec<(String, String)>> {
    let mut rows = Vec::new();
    for file in manifest.files() {
        let file_id = file.relative_path().replace('\\', "/");
        if matches!(format, DocumentFormat::Codex | DocumentFormat::ClaudeCode) {
            let input = read_bounded_file(file, max_file_bytes, format)?;
            for (line, raw) in std::str::from_utf8(&input)
                .with_context(|| format!("{format} input is not UTF-8"))?
                .lines()
                .enumerate()
                .filter(|(_, line)| !line.trim().is_empty())
            {
                let row_id = format!("{file_id}#{}", line + 1);
                if candidate_ids.is_some_and(|ids| !ids.contains(&row_id)) {
                    continue;
                }
                let value: serde_json::Value = serde_json::from_str(raw)
                    .with_context(|| format!("parse {format} record {}:{}", file_id, line + 1))?;
                rows.push((row_id, value.to_string()));
            }
            continue;
        }

        let mut ordinal = 0usize;
        let mut error = None;
        for_each_file_storyline(
            format,
            &LocalQueryManifest::from_frozen_files(
                file.path().parent().unwrap_or_else(|| Path::new(".")),
                format,
                vec![file.clone()],
            )?,
            max_file_bytes,
            |story| {
                ordinal += 1;
                if candidate_ids.is_some_and(|ids| !ids.contains(story.document_id())) {
                    return Ok(());
                }
                let row_id = if ordinal == 1 {
                    story.document_id().to_string()
                } else {
                    format!("{file_id}#{ordinal}")
                };
                let value = encode_json_storylines(format, std::slice::from_ref(&story))
                    .map_err(|e| anyhow::anyhow!(e));
                match value {
                    Ok(value) => rows.push((row_id, value.to_string())),
                    Err(e) => error = Some(e),
                }
                Ok(())
            },
        )?;
        if let Some(error) = error {
            return Err(error).with_context(|| format!("encode {format} virtual table row"));
        }
    }
    Ok(rows)
}

fn read_bounded_file(
    file: &super::LocalQueryInputFile,
    max_file_bytes: u64,
    format: DocumentFormat,
) -> Result<Vec<u8>> {
    file.validate_unchanged()?;
    if file.size_bytes() > max_file_bytes {
        anyhow::bail!(
            "{format} input {} is {} bytes, exceeding max_file_bytes {max_file_bytes}",
            file.path().display(),
            file.size_bytes()
        );
    }
    let input = std::fs::read(file.path())?;
    if input.len() as u64 > max_file_bytes {
        anyhow::bail!(
            "{format} input {} exceeded max_file_bytes {max_file_bytes} while reading",
            file.path().display()
        );
    }
    file.validate_unchanged()?;
    Ok(input)
}

fn strings_from_batch(
    batch: &datafusion::arrow::record_batch::RecordBatch,
    column: &str,
) -> Result<Vec<String>> {
    let mut values = Vec::new();
    let index = batch.schema().index_of(column)?;
    let array = batch
        .column(index)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow::anyhow!("query column '{column}' is not Utf8"))?;
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
        .map_err(|error| from_datafusion("plan pinned Storyline runs", error))?
        .collect()
        .await
        .map_err(|error| from_datafusion("read pinned Storyline runs", error))?;
    let steps = context
        .sql(&format!(
            "SELECT * FROM steps WHERE document_id = '{literal}' ORDER BY step_id"
        ))
        .await
        .map_err(|error| from_datafusion("plan pinned Storyline steps", error))?
        .collect()
        .await
        .map_err(|error| from_datafusion("read pinned Storyline steps", error))?;
    let tool_calls = context
        .sql(&format!(
            "SELECT * FROM tool_calls WHERE document_id = '{literal}' ORDER BY step_id, call_index"
        ))
        .await
        .map_err(|error| from_datafusion("plan pinned Storyline tool calls", error))?
        .collect()
        .await
        .map_err(|error| from_datafusion("read pinned Storyline tool calls", error))?;

    let mut run_rows = Vec::new();
    for batch in &runs {
        run_rows.extend(super::story_runs_from_batch(batch)?);
    }
    if run_rows.len() != 1 {
        anyhow::bail!(
            "pinned Storyline source returned {} run rows for document_id '{document_id}'",
            run_rows.len()
        );
    }
    let mut step_rows = Vec::new();
    for batch in &steps {
        step_rows.extend(super::story_steps_from_batch(batch)?);
    }
    let mut tool_call_rows = Vec::new();
    for batch in &tool_calls {
        tool_call_rows.extend(super::story_tool_calls_from_batch(batch)?);
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

fn budget_error(source: &DocumentSourceImpl, budget: &str) -> anyhow::Error {
    anyhow::anyhow!("{} source budget exceeded: {budget}", source.format())
}

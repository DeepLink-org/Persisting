use super::*;
use crate::store::opendal_store::Store as OpendalStore;

pub(super) async fn load_storyline_from_source(
    source: &ResolvedSource,
    key: &CatalogStorylineKey,
) -> Result<Option<StorylineDocument>> {
    let context = SessionContext::new();
    register_normalized_source(&context, source).await?;
    let document_predicate = sql_string(&key.document_id);
    let run_batches = context
        .sql(&format!(
            "SELECT * FROM runs WHERE document_id = {document_predicate}"
        ))
        .await?
        .collect()
        .await?;
    let mut runs = Vec::new();
    for batch in &run_batches {
        runs.extend(story_runs_from_batch(batch)?);
    }
    if runs.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        runs.len() == 1,
        "Catalog Storyline key resolved {} rows for {}/{}/{}",
        runs.len(),
        key.dataset,
        key.file,
        key.document_id
    );
    let step_batches = context
        .sql(&format!(
            "SELECT * FROM steps WHERE document_id = {document_predicate} ORDER BY step_id"
        ))
        .await?
        .collect()
        .await?;
    let tool_batches = context
        .sql(&format!(
            "SELECT * FROM tool_calls WHERE document_id = {document_predicate} ORDER BY step_id, call_index"
        ))
        .await?
        .collect()
        .await?;
    let mut steps = Vec::new();
    let mut tool_calls = Vec::new();
    for batch in &step_batches {
        steps.extend(story_steps_from_batch(batch)?);
    }
    for batch in &tool_batches {
        tool_calls.extend(story_tool_calls_from_batch(batch)?);
    }
    Ok(Some(reconstruct_storyline(
        crate::store::StorylineTables {
            run: runs.remove(0),
            steps,
            tool_calls,
        },
    )?))
}

#[derive(Debug)]
pub(super) struct SnapshotTempDir {
    path: PathBuf,
}

impl SnapshotTempDir {
    pub(super) fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "pchronicle-catalog-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir(&path)
            .with_context(|| format!("create catalog temporary directory {}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SnapshotTempDir {
    fn drop(&mut self) {
        if self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("pchronicle-catalog-"))
        {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Debug)]
pub(super) struct PreparedDataset {
    pub(super) name: String,
    pub(super) sources: Vec<Arc<LazySource>>,
    pub(super) max_concurrent_sources: usize,
}

#[derive(Clone, Debug)]
pub(super) struct SharedResolutionFailure(Arc<dyn std::error::Error + Send + Sync>);

impl SharedResolutionFailure {
    fn new(error: anyhow::Error) -> Self {
        Self(Arc::from(error.into_boxed_dyn_error()))
    }
}

impl std::fmt::Display for SharedResolutionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("cached Dataset source resolution failure")
    }
}

impl std::error::Error for SharedResolutionFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

#[cfg(all(test, feature = "proptest"))]
mod proptests {
    use proptest::prelude::*;
    use std::error::Error;

    use super::*;

    proptest! {
        #[test]
        fn shared_resolution_failures_preserve_a_source_error(
            message in proptest::string::string_regex("[A-Za-z0-9 .,!?]{1,64}").unwrap(),
        ) {
            let failure = SharedResolutionFailure::new(anyhow::anyhow!("{}", message.clone()));
            prop_assert_eq!(failure.to_string(), "cached Dataset source resolution failure");
            prop_assert_eq!(failure.source().map(ToString::to_string), Some(message));
        }

    }
}

#[derive(Debug)]
pub(super) struct LazySource {
    pub(super) file: String,
    pub(super) spec: LazySourceSpec,
    pub(super) options: CatalogSnapshotOptions,
    pub(super) temporary_files: Arc<SnapshotTempDir>,
    pub(super) resolved:
        OnceCell<std::result::Result<Arc<ResolvedSource>, SharedResolutionFailure>>,
    pub(super) resolution_count: AtomicUsize,
}

#[derive(Debug)]
pub(super) enum LazySourceSpec {
    Storyline {
        paths: StorylineTablePaths,
    },

    Compact {
        uri: String,
    },
    LocalFile {
        root: PathBuf,
        file: LocalQueryInputFile,
        format_hint: Option<DocumentFormat>,
    },
    RemoteFile {
        store: OpendalStore,
        meta: RemoteObjectMeta,
        format_hint: Option<DocumentFormat>,
    },
}

impl LazySource {
    pub(super) fn new(
        file: String,
        spec: LazySourceSpec,
        options: CatalogSnapshotOptions,
        temporary_files: Arc<SnapshotTempDir>,
    ) -> Self {
        Self {
            file,
            spec,
            options,
            temporary_files,
            resolved: OnceCell::new(),
            resolution_count: AtomicUsize::new(0),
        }
    }

    pub(super) fn file(&self) -> &str {
        &self.file
    }

    pub(super) fn format_hint(&self) -> Option<DocumentFormat> {
        match &self.spec {
            LazySourceSpec::Storyline { .. } => Some(DocumentFormat::StorylineLance),
            LazySourceSpec::Compact { .. } => None,
            LazySourceSpec::LocalFile { format_hint, .. }
            | LazySourceSpec::RemoteFile { format_hint, .. } => *format_hint,
        }
    }

    pub(super) async fn resolve(&self) -> Result<Arc<ResolvedSource>> {
        let result = self
            .resolved
            .get_or_init(|| async {
                self.resolution_count.fetch_add(1, Ordering::Relaxed);
                self.resolve_inner()
                    .await
                    .map(Arc::new)
                    .map_err(SharedResolutionFailure::new)
            })
            .await;
        match result {
            Ok(source) => Ok(source.clone()),
            Err(error) => Err(anyhow::Error::new(error.clone())),
        }
    }

    async fn resolve_inner(&self) -> Result<ResolvedSource> {
        match &self.spec {
            LazySourceSpec::Storyline { paths } => Ok(ResolvedSource::Storyline(
                StorylineDataSource::from_pinned_paths_with_options(
                    paths.clone(),
                    self.options.storyline,
                )
                .await?,
            )),

            LazySourceSpec::Compact { .. } => Ok(ResolvedSource::Compact),
            LazySourceSpec::LocalFile {
                root,
                file,
                format_hint,
            } => {
                let format = match format_hint {
                    Some(format) => *format,
                    None => file.detect_format_with_options(self.options.manifest)?,
                };
                let manifest =
                    LocalQueryManifest::from_frozen_files(root, format, vec![file.clone()])?;
                Ok(ResolvedSource::File(
                    FileTrajectoryDataSource::from_manifest_with_options(
                        manifest,
                        self.options.files,
                    )?,
                ))
            }
            LazySourceSpec::RemoteFile {
                store,
                meta,
                format_hint,
            } => {
                let extension = Path::new(&self.file)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .unwrap_or("json");
                let local = self.temporary_files.path().join(format!(
                    "remote-{}.{}",
                    uuid::Uuid::new_v4().simple(),
                    extension
                ));
                materialize_pinned_object(store, meta, &local, self.options.files.max_file_bytes)
                    .await
                    .with_context(|| {
                        format!("materialize pinned trajectory object {}", self.file)
                    })?;
                let format = match format_hint {
                    Some(format) => *format,
                    None => LocalQueryManifest::detect_with_options(&local, self.options.manifest)
                        .with_context(|| {
                            format!("detect format for remote trajectory object {}", self.file)
                        })?
                        .format(),
                };
                let manifest = LocalQueryManifest::from_explicit_files(
                    self.temporary_files.path(),
                    format,
                    vec![(local, self.file.clone())],
                )?;
                Ok(ResolvedSource::File(
                    FileTrajectoryDataSource::from_manifest_with_options(
                        manifest,
                        self.options.files,
                    )?,
                ))
            }
        }
    }

    pub(super) fn file_metrics(&self) -> Option<FileTrajectoryQueryMetrics> {
        match self.resolved.get()?.as_ref().ok()?.as_ref() {
            ResolvedSource::File(source) => Some(source.metrics()),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(super) enum ResolvedSource {
    Storyline(StorylineDataSource),
    File(FileTrajectoryDataSource),
    Compact,
}

pub(super) struct ResolvedTable {
    pub(super) provider: Arc<dyn TableProvider>,
    pub(super) carries_file_column: bool,
}

impl ResolvedSource {
    /// Evaluate virtual-table predicates against normalized run, step, and
    /// tool-call columns and return the document identities that can possibly
    /// match. `None` means this source cannot provide a normalized projection,
    /// so callers should use the normal fallback.
    pub(super) async fn document_ids_for_virtual_filters(
        &self,
        format: DocumentFormat,
        predicates: &[crate::store::virtual_document::NormalizedJsonPredicate],
    ) -> Result<Option<BTreeSet<String>>> {
        if matches!(format, DocumentFormat::Codex | DocumentFormat::ClaudeCode) {
            return Ok(None);
        }
        let mut candidates: Option<BTreeSet<String>> = None;
        for predicate in predicates {
            let kind = match predicate.table {
                crate::store::virtual_document::NormalizedVirtualTable::Runs => {
                    CatalogTableKind::Runs
                }
                crate::store::virtual_document::NormalizedVirtualTable::Steps => {
                    CatalogTableKind::Steps
                }
                crate::store::virtual_document::NormalizedVirtualTable::ToolCalls => {
                    CatalogTableKind::ToolCalls
                }
            };
            let Some(table) = self.table(kind).await? else {
                return Ok(None);
            };
            if table.provider.schema().index_of("document_id").is_err() {
                return Ok(None);
            }
            let matching = crate::store::virtual_document::matching_document_ids(
                table.provider,
                &predicate.filter,
            )
            .await?;
            candidates = Some(match candidates {
                Some(current) => current.intersection(&matching).cloned().collect(),
                None => matching,
            });
        }
        Ok(candidates)
    }

    pub(super) async fn virtual_document_rows_filtered(
        &self,
        format: DocumentFormat,
        candidate_ids: Option<&BTreeSet<String>>,
    ) -> Result<Vec<(String, String)>> {
        match self {
            Self::File(source) if source.format() == format => {
                source.virtual_document_rows_filtered(candidate_ids)
            }
            Self::Storyline(source) if format == DocumentFormat::Storyline => {
                let context = SessionContext::new();
                source.register(&context)?;
                let batches = context
                    .sql("SELECT document_id FROM runs ORDER BY storage_ordinal, document_id")
                    .await?
                    .collect()
                    .await?;
                let mut rows = Vec::new();
                for batch in &batches {
                    let index = batch.schema().index_of("document_id")?;
                    let values = batch
                        .column(index)
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .with_context(|| "Storyline document_id is not Utf8")?;
                    for row in 0..values.len() {
                        if values.is_null(row) {
                            continue;
                        }
                        let document_id = values.value(row).to_string();
                        if candidate_ids.is_some_and(|ids| !ids.contains(&document_id)) {
                            continue;
                        }
                        let key = CatalogStorylineKey {
                            dataset: String::new(),
                            file: String::new(),
                            document_id: document_id.clone(),
                            session_id: String::new(),
                        };
                        let story = load_storyline_from_source(self, &key)
                            .await?
                            .with_context(|| format!("missing Storyline document {document_id}"))?;
                        let value = crate::document::encode_json_storylines(
                            DocumentFormat::Storyline,
                            std::slice::from_ref(&story),
                        )?;
                        rows.push((document_id, value.to_string()));
                    }
                }
                Ok(rows)
            }

            Self::Compact => Ok(Vec::new()),
            _ => Ok(Vec::new()),
        }
    }

    pub(super) async fn table(&self, kind: CatalogTableKind) -> Result<Option<ResolvedTable>> {
        let storyline_kind = || match kind {
            CatalogTableKind::Runs => Some(StorylineTableKind::Runs),
            CatalogTableKind::Steps => Some(StorylineTableKind::Steps),
            CatalogTableKind::ToolCalls => Some(StorylineTableKind::ToolCalls),
        };
        Ok(match self {
            Self::Storyline(source) => storyline_kind().map(|kind| ResolvedTable {
                provider: source.provider(kind),
                carries_file_column: false,
            }),
            Self::File(source) => storyline_kind().map(|kind| ResolvedTable {
                provider: source.provider(kind),
                carries_file_column: true,
            }),

            Self::Compact => None,
        })
    }
}

async fn register_normalized_source(
    context: &SessionContext,
    source: &ResolvedSource,
) -> Result<()> {
    match source {
        ResolvedSource::Storyline(source) => source.register(context),
        ResolvedSource::File(source) => source.register(context),

        ResolvedSource::Compact => {
            anyhow::bail!("compact JSONL has no normalized trajectory tables")
        }
    }
}

async fn materialize_pinned_object(
    store: &OpendalStore,
    meta: &RemoteObjectMeta,
    destination: &Path,
    max_bytes: u64,
) -> Result<()> {
    let (bytes, _) = store
        .read(&meta.location)
        .await
        .with_context(|| format!("read pinned Dataset object {}", meta.location))?
        .with_context(|| format!("pinned Dataset object {} disappeared", meta.location))?;
    let mut output = tokio::fs::File::create(destination)
        .await
        .with_context(|| format!("create pinned Dataset file {}", destination.display()))?;
    let mut written = 0u64;
    for chunk in bytes.chunks(64 * 1024) {
        written = written.saturating_add(chunk.len() as u64);
        anyhow::ensure!(
            written <= max_bytes,
            "pinned Dataset object {} exceeds max_file_bytes {max_bytes}",
            meta.location
        );
        output
            .write_all(chunk)
            .await
            .with_context(|| format!("write pinned Dataset file {}", destination.display()))?;
    }
    output
        .flush()
        .await
        .with_context(|| format!("flush pinned Dataset file {}", destination.display()))?;
    anyhow::ensure!(
        written == meta.size,
        "object {} size changed while freezing Dataset snapshot",
        meta.location
    );
    Ok(())
}

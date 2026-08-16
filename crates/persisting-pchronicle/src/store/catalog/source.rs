use super::*;

pub(super) async fn load_storyline_from_source(
    source: &ResolvedSource,
    key: &CatalogStorylineKey,
) -> Result<Option<StorylineDocument>> {
    if let ResolvedSource::Events(events) = source {
        if events.projection.is_none() {
            let Some(records) = events.records_for_storyline(&key.session_id).await? else {
                return Ok(None);
            };
            return Ok(Some(project_event_records(&records)?));
        }
    }
    let context = SessionContext::new();
    register_normalized_source(&context, source).await?;
    let session_predicate = sql_string(&key.session_id);
    let run_batches = context
        .sql(&format!(
            "SELECT * FROM runs WHERE session_id = {session_predicate}"
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
        key.session_id
    );
    let step_batches = context
        .sql(&format!(
            "SELECT * FROM steps WHERE session_id = {session_predicate} ORDER BY step_id"
        ))
        .await?
        .collect()
        .await?;
    let tool_batches = context
        .sql(&format!(
            "SELECT * FROM tool_calls WHERE session_id = {session_predicate} ORDER BY step_id, call_index"
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
    Ok(Some(reconstruct_storyline(crate::StorylineTables {
        run: runs.remove(0),
        steps,
        tool_calls,
    })?))
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

#[derive(Debug)]
pub(super) struct LazySource {
    pub(super) file: String,
    pub(super) spec: LazySourceSpec,
    pub(super) options: CatalogSnapshotOptions,
    pub(super) temporary_files: Arc<SnapshotTempDir>,
    pub(super) resolved: OnceCell<std::result::Result<Arc<ResolvedSource>, String>>,
    pub(super) resolution_count: AtomicUsize,
}

#[derive(Debug)]
pub(super) enum LazySourceSpec {
    Storyline {
        paths: StorylineTablePaths,
    },
    Events {
        uri: String,
        snapshot: RawEventSnapshot,
        projection: Option<StorylineTablePaths>,
    },
    LocalFile {
        root: PathBuf,
        file: LocalQueryInputFile,
        format_hint: Option<ChronicleFormat>,
    },
    RemoteFile {
        store: Arc<LanceObjectStore>,
        meta: ObjectMeta,
        format_hint: Option<ChronicleFormat>,
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

    pub(super) fn supports(&self, kind: CatalogTableKind) -> bool {
        match (&self.spec, kind) {
            (LazySourceSpec::Events { .. }, _) => true,
            (_, CatalogTableKind::Events) => false,
            _ => true,
        }
    }

    pub(super) fn canonical_event_uri(&self) -> Option<&str> {
        match &self.spec {
            LazySourceSpec::Events { uri, .. } => Some(uri),
            _ => None,
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
                    .map_err(|error| redact_error(&format!("{error:#}")))
            })
            .await;
        match result {
            Ok(source) => Ok(source.clone()),
            Err(error) => anyhow::bail!("{error}"),
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
            LazySourceSpec::Events {
                snapshot,
                projection,
                ..
            } => {
                let source = RawEventDataSource::from_pinned_snapshot_with_options(
                    snapshot.clone(),
                    RawEventDataSourceOptions::default(),
                )
                .await?;
                let projection = match projection {
                    Some(paths) => Some(
                        StorylineDataSource::from_pinned_paths_with_options(
                            paths.clone(),
                            self.options.storyline,
                        )
                        .await?,
                    ),
                    None => None,
                };
                Ok(ResolvedSource::Events(ResolvedEventSource {
                    source,
                    projection,
                    max_fallback_rows: self.options.max_event_fallback_rows,
                    max_fallback_bytes: self.options.max_event_fallback_bytes,
                    normalization_count: AtomicUsize::new(0),
                }))
            }
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
                    None => LocalQueryManifest::detect_with_options(&local, self.options.manifest)?
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
    Events(ResolvedEventSource),
    File(FileTrajectoryDataSource),
}

#[derive(Debug)]
pub(super) struct ResolvedEventSource {
    source: RawEventDataSource,
    projection: Option<StorylineDataSource>,
    max_fallback_rows: usize,
    max_fallback_bytes: usize,
    pub(super) normalization_count: AtomicUsize,
}

impl ResolvedEventSource {
    async fn normalized_for(
        &self,
        session_ids: Option<&BTreeSet<String>>,
        kind: CatalogTableKind,
    ) -> Result<Arc<MemTable>> {
        self.normalization_count.fetch_add(1, Ordering::Relaxed);
        normalize_event_storylines(
            &self.source,
            session_ids,
            kind,
            self.max_fallback_rows,
            self.max_fallback_bytes,
        )
        .await
    }

    pub(super) async fn records_for_storyline(
        &self,
        session_id: &str,
    ) -> Result<Option<Vec<EventRecord>>> {
        let session_ids = BTreeSet::from([session_id.to_string()]);
        let records = self
            .source
            .read_records_for_storylines_bounded(
                &session_ids,
                self.max_fallback_rows,
                self.max_fallback_bytes,
            )
            .await?;
        Ok((!records.is_empty()).then_some(records))
    }
}

pub(super) struct ResolvedTable {
    pub(super) provider: Arc<dyn TableProvider>,
    pub(super) carries_file_column: bool,
}

impl ResolvedSource {
    pub(super) async fn table(
        &self,
        kind: CatalogTableKind,
        event_session_ids: Option<&BTreeSet<String>>,
    ) -> Result<Option<ResolvedTable>> {
        let storyline_kind = || match kind {
            CatalogTableKind::Runs => Some(StorylineTableKind::Runs),
            CatalogTableKind::Steps => Some(StorylineTableKind::Steps),
            CatalogTableKind::ToolCalls => Some(StorylineTableKind::ToolCalls),
            CatalogTableKind::Events => None,
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
            Self::Events(source) if kind == CatalogTableKind::Events => Some(ResolvedTable {
                provider: source.source.provider(),
                carries_file_column: false,
            }),
            Self::Events(source) => {
                if let Some(projection) = &source.projection {
                    return Ok(storyline_kind().map(|kind| ResolvedTable {
                        provider: projection.provider(kind),
                        carries_file_column: false,
                    }));
                }
                let normalized = source.normalized_for(event_session_ids, kind).await?;
                Some(ResolvedTable {
                    provider: normalized,
                    carries_file_column: false,
                })
            }
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
        ResolvedSource::Events(source) => {
            if let Some(projection) = &source.projection {
                return projection.register(context);
            }
            anyhow::bail!(
                "registering all normalized canonical events requires a fresh Storyline projection"
            )
        }
    }
}

async fn materialize_pinned_object(
    store: &Arc<LanceObjectStore>,
    meta: &ObjectMeta,
    destination: &Path,
    max_bytes: u64,
) -> Result<()> {
    let options = GetOptions {
        if_match: meta.e_tag.clone(),
        version: meta.version.clone(),
        ..GetOptions::default()
    };
    let mut stream = store
        .inner
        .get_opts(&meta.location, options)
        .await
        .with_context(|| format!("read pinned Dataset object {}", meta.location))?
        .into_stream();
    let mut output = tokio::fs::File::create(destination)
        .await
        .with_context(|| format!("create pinned Dataset file {}", destination.display()))?;
    let mut written = 0u64;
    while let Some(chunk) = stream
        .try_next()
        .await
        .with_context(|| format!("stream pinned Dataset object {}", meta.location))?
    {
        written = written.saturating_add(chunk.len() as u64);
        anyhow::ensure!(
            written <= max_bytes,
            "pinned Dataset object {} exceeds max_file_bytes {max_bytes}",
            meta.location
        );
        output
            .write_all(&chunk)
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

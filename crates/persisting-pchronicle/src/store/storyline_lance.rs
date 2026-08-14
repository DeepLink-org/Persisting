//! Storyline-native normalized Lance store.
//!
//! `CURRENT` pins one exact MVCC version from each normalized Lance dataset and
//! the shared content-addressed object dataset.
//! A replacement deletes and appends rows for only the requested sessions,
//! then moves `CURRENT` after all table versions are durable. Readers therefore
//! never observe a partially updated Storyline.
//!
//! ```text
//! root/
//!   CURRENT  # logical snapshot id + exact table/object version ids
//!   objects.lance/
//!   generations/<table-generation>/
//!     runs.lance/
//!     steps.lance/
//!     tool_calls.lance/
//! ```

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use fs2::FileExt;
use futures::TryStreamExt;
use lance::dataset::optimize::{compact_files, CompactionOptions};
use lance::dataset::scanner::ColumnOrdering;
use lance::dataset::{InsertBuilder, WriteMode, WriteParams};
use lance::deps::arrow_array::{
    Array, Int64Array, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray,
};
use lance::deps::arrow_schema::{ArrowError, SchemaRef};
use lance::index::DatasetIndexExt;
use lance::io::ObjectStore;
use lance::Dataset;
use lance_index::optimize::OptimizeOptions;
use lance_index::scalar::{BuiltinIndexType, ScalarIndexParams};
use lance_index::IndexType;
use object_store::path::Path as ObjectPath;
use object_store::{Error as ObjectStoreError, ObjectStoreExt, PutMode, UpdateVersion};
use serde::{Deserialize, Serialize};

use crate::convert::atif_to_storyline;
use crate::storyline_schema::{
    reconstruct_storyline, split_storyline, StoryRunRow, StoryStepRow, StoryToolCallRow,
    StorylineTables, STORY_RUNS_TABLE, STORY_STEPS_TABLE, STORY_TOOL_CALLS_TABLE,
};
use crate::StorylineDocument;

use super::atif_datafusion::AtifReader;
use super::storyline_content::{
    collect_content_ids, commit_pending_content, content_columns, externalize_batches,
    hydrate_batches, open_objects, prune_unreferenced_objects, PendingContent,
    StorylineContentOptions, STORYLINE_OBJECTS_DATASET,
};
use super::storyline_datafusion::StorylineTableKind;
use super::storyline_lance_rows::{
    story_runs_arrow_schema, story_runs_from_batch, story_runs_to_batch, story_steps_arrow_schema,
    story_steps_from_batch, story_steps_to_batch, story_tool_calls_arrow_schema,
    story_tool_calls_from_batch, story_tool_calls_to_batch,
};
use super::{root_write_lock, LanceMaintenanceOptions, LanceMaintenanceReport};

const CURRENT_FILE: &str = "CURRENT";
const GENERATIONS_DIR: &str = "generations";
const WRITE_BATCH_ROWS: usize = 8192;
const STREAM_IMPORT_STORIES: usize = 256;
pub const DEFAULT_STORYLINE_PAGE_SIZE: usize = 100;
pub const MAX_STORYLINE_PAGE_SIZE: usize = 1_000;
const RUN_INDEXES: [(&str, IndexType); 2] = [
    ("session_id", IndexType::BTree),
    ("run_id", IndexType::BTree),
];
// `step_id` restarts inside every Storyline and has low global selectivity.
// `session_id` first narrows a lookup to one short Storyline, after which a
// step range is cheaper to filter than maintaining another BTree.
const STEP_INDEXES: [(&str, IndexType); 4] = [
    ("session_id", IndexType::BTree),
    ("timestamp", IndexType::BTree),
    ("effective_kind", IndexType::Bitmap),
    ("source", IndexType::Bitmap),
];
const TOOL_CALL_INDEXES: [(&str, IndexType); 3] = [
    ("session_id", IndexType::BTree),
    ("tool_call_id", IndexType::BTree),
    ("function_name", IndexType::Bitmap),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorylineTablePaths {
    /// Logical three-table snapshot id. Changes after every committed replace.
    pub generation: String,
    /// Physical Lance dataset generation shared by successive MVCC snapshots.
    pub table_generation: String,
    pub runs: PathBuf,
    pub steps: PathBuf,
    pub tool_calls: PathBuf,
    pub objects: PathBuf,
    pub runs_version: u64,
    pub steps_version: u64,
    pub tool_calls_version: u64,
    pub objects_version: u64,
}

/// Scalar-only run metadata suitable for catalog and list views.
///
/// This projection deliberately excludes every content-addressed column, so
/// listing runs does not open the objects dataset or read step/tool-call rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorylineRunSummary {
    pub run_id: String,
    pub session_id: String,
    pub schema_version: String,
    pub agent_id: String,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    pub agent_model_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorylineRunCursor {
    pub generation: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorylineStepCursor {
    pub generation: String,
    pub session_id: String,
    pub step_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorylineToolCallCursor {
    pub generation: String,
    pub session_id: String,
    pub step_id: i64,
    pub call_index: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorylinePageRequest<C> {
    pub limit: usize,
    pub after: Option<C>,
}

impl<C> Default for StorylinePageRequest<C> {
    fn default() -> Self {
        Self {
            limit: DEFAULT_STORYLINE_PAGE_SIZE,
            after: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorylinePage<T, C> {
    pub generation: Option<String>,
    pub items: Vec<T>,
    pub next_cursor: Option<C>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StorylineSnapshotPointer {
    generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_generation: Option<String>,
    table_generation: String,
    runs_version: u64,
    steps_version: u64,
    tool_calls_version: u64,
    objects_version: u64,
}

#[derive(Debug, Clone)]
pub struct StorylineLanceStore {
    root: PathBuf,
    root_uri: String,
    object_store: std::sync::Arc<ObjectStore>,
    object_root: ObjectPath,
    write_lock: Arc<tokio::sync::Mutex<()>>,
    content_options: StorylineContentOptions,
}

struct StoreWriteGuard {
    _process: tokio::sync::OwnedMutexGuard<()>,
    local_file: Option<File>,
}

impl Drop for StoreWriteGuard {
    fn drop(&mut self) {
        if let Some(file) = &self.local_file {
            let _ = FileExt::unlock(file);
        }
    }
}

struct CurrentPointerState {
    pointer: Option<StorylineSnapshotPointer>,
    version: Option<UpdateVersion>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorylineMaintenanceReport {
    pub generation: Option<String>,
    pub runs: LanceMaintenanceReport,
    pub steps: LanceMaintenanceReport,
    pub tool_calls: LanceMaintenanceReport,
    pub objects_removed: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorylineStreamImportReport {
    pub generation: String,
    pub storylines: usize,
    pub steps: usize,
    pub tool_calls: usize,
}

impl StorylineLanceStore {
    pub async fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        tokio::fs::create_dir_all(root.join(GENERATIONS_DIR))
            .await
            .with_context(|| format!("create Storyline Lance root {}", root.display()))?;
        let root_uri = root
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Storyline Lance root is not valid UTF-8"))?;
        Self::open_uri(root_uri).await
    }

    pub async fn open_with_content_options(
        root: impl AsRef<Path>,
        content_options: StorylineContentOptions,
    ) -> Result<Self> {
        let content_options = content_options.validate()?;
        let root = root.as_ref().to_path_buf();
        tokio::fs::create_dir_all(root.join(GENERATIONS_DIR))
            .await
            .with_context(|| format!("create Storyline Lance root {}", root.display()))?;
        let root_uri = root
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Storyline Lance root is not valid UTF-8"))?;
        Self::open_uri_with_content_options(root_uri, content_options).await
    }

    /// Open a Storyline store at a local path or object-store URI.
    ///
    /// `s3://`, `az://`, and `gs://` use Lance's standard credential and
    /// storage-option environment variables. The commit pointer is stored in
    /// the same backend as the versioned Lance datasets.
    pub async fn open_uri(root: impl AsRef<str>) -> Result<Self> {
        let store = Self::open_uri_unchecked(root).await?;
        // Fail early on a malformed or dangling commit pointer for callers
        // opening the store directly.
        let _ = store.current_table_paths().await?;
        Ok(store)
    }

    pub async fn open_uri_with_content_options(
        root: impl AsRef<str>,
        content_options: StorylineContentOptions,
    ) -> Result<Self> {
        let content_options = content_options.validate()?;
        let mut store = Self::open_uri_unchecked(root).await?;
        store.content_options = content_options;
        let _ = store.current_table_paths().await?;
        Ok(store)
    }

    pub(crate) async fn open_uri_unchecked(root: impl AsRef<str>) -> Result<Self> {
        let root_uri = normalize_root_uri(root.as_ref())?;
        let (object_store, object_root) = ObjectStore::from_uri(&root_uri)
            .await
            .with_context(|| format!("open Storyline object store {root_uri}"))?;
        Ok(Self {
            root: PathBuf::from(&root_uri),
            write_lock: root_write_lock::for_root(&root_uri),
            root_uri,
            object_store,
            object_root,
            content_options: StorylineContentOptions::default(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The exact local path or object-store URI used for Lance datasets.
    pub fn root_uri(&self) -> &str {
        &self.root_uri
    }

    pub fn storage_scheme(&self) -> &str {
        self.object_store.scheme()
    }

    async fn acquire_write_guard(&self) -> Result<StoreWriteGuard> {
        let process = self.write_lock.clone().lock_owned().await;
        let local_file = if matches!(self.storage_scheme(), "file" | "file+uring") {
            let lock_path = self.root.join(".storyline-write.lock");
            Some(
                tokio::task::spawn_blocking(move || -> Result<File> {
                    if let Some(parent) = lock_path.parent() {
                        std::fs::create_dir_all(parent).with_context(|| {
                            format!("create Storyline lock root {}", parent.display())
                        })?;
                    }
                    let file = OpenOptions::new()
                        .create(true)
                        .truncate(false)
                        .read(true)
                        .write(true)
                        .open(&lock_path)
                        .with_context(|| {
                            format!("open Storyline write lock {}", lock_path.display())
                        })?;
                    file.lock_exclusive().with_context(|| {
                        format!("lock Storyline write root {}", lock_path.display())
                    })?;
                    Ok(file)
                })
                .await
                .context("join Storyline write-lock task")??,
            )
        } else {
            None
        };
        Ok(StoreWriteGuard {
            _process: process,
            local_file,
        })
    }

    /// Paths and exact versions for the committed snapshot, or `None` for an empty store.
    pub async fn current_table_paths(&self) -> Result<Option<StorylineTablePaths>> {
        let Some(paths) = self.resolve_current_table_paths().await? else {
            return Ok(None);
        };
        tokio::try_join!(
            validate_table(&paths.generation, &paths.runs, paths.runs_version),
            validate_table(&paths.generation, &paths.steps, paths.steps_version),
            validate_table(
                &paths.generation,
                &paths.tool_calls,
                paths.tool_calls_version
            ),
            validate_table(&paths.generation, &paths.objects, paths.objects_version),
        )?;
        Ok(Some(paths))
    }

    pub(crate) async fn resolve_current_table_paths(&self) -> Result<Option<StorylineTablePaths>> {
        let Some(pointer) = self.read_current_pointer().await?.pointer else {
            return Ok(None);
        };
        let mut paths = self.paths_for_generation(&pointer.table_generation);
        paths.generation = pointer.generation;
        paths.runs_version = pointer.runs_version;
        paths.steps_version = pointer.steps_version;
        paths.tool_calls_version = pointer.tool_calls_version;
        paths.objects_version = pointer.objects_version;
        Ok(Some(paths))
    }

    async fn read_current_pointer(&self) -> Result<CurrentPointerState> {
        let pointer = self.object_root.clone().join(CURRENT_FILE);
        let result = match self.object_store.inner.get(&pointer).await {
            Ok(result) => result,
            Err(ObjectStoreError::NotFound { .. }) => {
                return Ok(CurrentPointerState {
                    pointer: None,
                    version: None,
                });
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read Storyline commit pointer {}/CURRENT", self.root_uri)
                });
            }
        };
        let version = UpdateVersion {
            e_tag: result.meta.e_tag.clone(),
            version: result.meta.version.clone(),
        };
        let contents = result
            .bytes()
            .await
            .with_context(|| format!("read Storyline commit pointer {}/CURRENT", self.root_uri))?;
        let contents = std::str::from_utf8(&contents)
            .context("Storyline commit pointer is not valid UTF-8")?
            .trim();
        if !contents.starts_with('{') {
            validate_generation_name(contents)?;
            anyhow::bail!(
                "Storyline generation '{contents}' is incomplete: CURRENT must pin all table and object versions"
            );
        }
        let pointer = serde_json::from_str::<StorylineSnapshotPointer>(contents)
            .context("decode Storyline snapshot pointer")?;
        validate_generation_name(&pointer.generation)?;
        if let Some(parent) = &pointer.parent_generation {
            validate_generation_name(parent)?;
        }
        validate_generation_name(&pointer.table_generation)?;
        Ok(CurrentPointerState {
            pointer: Some(pointer),
            version: Some(version),
        })
    }

    pub async fn replace_storyline(&self, story: &StorylineDocument) -> Result<()> {
        self.replace_storylines(std::slice::from_ref(story)).await
    }

    /// Build an empty Storyline store directly from a replayable ATIF input.
    ///
    /// One producer normalizes each trajectory once and fans bounded Arrow
    /// batches into three concurrent Lance create transactions. No intermediate
    /// Storyline collection or MVCC replacement versions are produced.
    pub async fn import_atif_stream(
        &self,
        input: impl AsRef<Path>,
    ) -> Result<StorylineStreamImportReport> {
        anyhow::ensure!(
            self.resolve_current_table_paths().await?.is_none(),
            "streaming ATIF create requires an empty Storyline store"
        );
        let stories = AtifReader::open(input.as_ref())?.map(|trajectory| {
            let trajectory = trajectory?;
            atif_to_storyline(&trajectory).map_err(anyhow::Error::from)
        });
        self.replace_storyline_stream(stories).await
    }

    /// Atomically replace a stream of Storylines without retaining the whole
    /// import in memory.
    ///
    /// At most [`STREAM_IMPORT_STORIES`] documents plus their normalized rows
    /// are materialized at once. Lance versions may advance while the stream is
    /// consumed, but `CURRENT` moves only after every chunk and final index
    /// maintenance succeed, so readers keep seeing the previous snapshot on
    /// failure.
    pub async fn replace_storyline_stream<I>(
        &self,
        stories: I,
    ) -> Result<StorylineStreamImportReport>
    where
        I: IntoIterator<Item = Result<StorylineDocument>>,
    {
        let _guard = self.acquire_write_guard().await?;
        let original = self.resolve_current_table_paths().await?;
        let expected_generation = original.as_ref().map(|paths| paths.generation.clone());
        let mut paths = original.clone();
        let mut new_table_generation = None;
        let mut iterator = stories.into_iter();
        let mut session_ids = HashSet::new();
        let mut report = StorylineStreamImportReport::default();

        let result = async {
            loop {
                let Some(chunk) = next_storyline_stream_chunk(&mut iterator, &mut session_ids)?
                else {
                    break;
                };
                report.storylines += chunk.runs.len();
                report.steps += chunk.steps.len();
                report.tool_calls += chunk.tool_calls.len();
                let externalized = externalize_rows(
                    chunk.runs,
                    chunk.steps,
                    chunk.tool_calls,
                    self.content_options,
                )?;
                let ExternalizedStorylineBatches {
                    runs: run_batches,
                    steps: step_batches,
                    tool_calls: tool_call_batches,
                    pending,
                } = externalized;

                paths = Some(match paths {
                    None => {
                        let generation = next_generation();
                        let mut created = self.paths_for_generation(&generation);
                        let objects_version =
                            commit_pending_content(&created.objects, None, pending).await?;
                        let (runs_version, steps_version, tool_calls_version) = tokio::try_join!(
                            write_batches(
                                &created.runs,
                                run_batches,
                                story_runs_arrow_schema(),
                                &RUN_INDEXES,
                            ),
                            write_batches(
                                &created.steps,
                                step_batches,
                                story_steps_arrow_schema(),
                                &STEP_INDEXES,
                            ),
                            write_batches(
                                &created.tool_calls,
                                tool_call_batches,
                                story_tool_calls_arrow_schema(),
                                &TOOL_CALL_INDEXES,
                            ),
                        )?;
                        created.runs_version = runs_version;
                        created.steps_version = steps_version;
                        created.tool_calls_version = tool_calls_version;
                        created.objects_version = objects_version;
                        new_table_generation = Some(generation);
                        created
                    }
                    Some(mut current) => {
                        let predicate = session_set_predicate(&chunk.session_ids);
                        let objects_version = commit_pending_content(
                            &current.objects,
                            Some(current.objects_version),
                            pending,
                        )
                        .await?;
                        let (runs_version, steps_version, tool_calls_version) = tokio::try_join!(
                            replace_table_batches(
                                &current.runs,
                                current.runs_version,
                                &predicate,
                                run_batches,
                                story_runs_arrow_schema(),
                            ),
                            replace_table_batches(
                                &current.steps,
                                current.steps_version,
                                &predicate,
                                step_batches,
                                story_steps_arrow_schema(),
                            ),
                            replace_table_batches(
                                &current.tool_calls,
                                current.tool_calls_version,
                                &predicate,
                                tool_call_batches,
                                story_tool_calls_arrow_schema(),
                            ),
                        )?;
                        current.runs_version = runs_version;
                        current.steps_version = steps_version;
                        current.tool_calls_version = tool_calls_version;
                        current.objects_version = objects_version;
                        current
                    }
                });
            }

            anyhow::ensure!(report.storylines > 0, "Storyline stream is empty");
            let current = paths
                .as_ref()
                .context("missing streamed Storyline tables")?;
            let (runs_version, steps_version, tool_calls_version) =
                if report.storylines > STREAM_IMPORT_STORIES {
                    let maintenance = LanceMaintenanceOptions {
                        // Extend indices once after a genuinely multi-chunk
                        // import, without putting compaction in the ingest
                        // path or rewriting the corpus.
                        compact: false,
                        optimize_indices: true,
                        vacuum_older_than: None,
                        ..Default::default()
                    };
                    let (runs, steps, tool_calls) = tokio::try_join!(
                        maintain_table_layout(
                            &current.runs,
                            current.runs_version,
                            &RUN_INDEXES,
                            &maintenance,
                        ),
                        maintain_table_layout(
                            &current.steps,
                            current.steps_version,
                            &STEP_INDEXES,
                            &maintenance,
                        ),
                        maintain_table_layout(
                            &current.tool_calls,
                            current.tool_calls_version,
                            &TOOL_CALL_INDEXES,
                            &maintenance,
                        ),
                    )?;
                    (
                        runs.final_version
                            .context("missing imported runs version")?,
                        steps
                            .final_version
                            .context("missing imported steps version")?,
                        tool_calls
                            .final_version
                            .context("missing imported tool_calls version")?,
                    )
                } else {
                    (
                        current.runs_version,
                        current.steps_version,
                        current.tool_calls_version,
                    )
                };
            let generation = next_generation();
            self.commit_snapshot(
                &StorylineSnapshotPointer {
                    generation: generation.clone(),
                    parent_generation: expected_generation.clone(),
                    table_generation: current.table_generation.clone(),
                    runs_version,
                    steps_version,
                    tool_calls_version,
                    objects_version: current.objects_version,
                },
                expected_generation.as_deref(),
            )
            .await?;
            report.generation = generation;
            Ok(report)
        }
        .await;

        if result.is_err() && original.is_none() {
            if let Some(generation) = new_table_generation {
                let _ = self
                    .object_store
                    .remove_dir_all(self.generation_object_path(&generation))
                    .await;
            }
        }
        result
    }

    /// Compact fragments, extend scalar indices to appended fragments, and
    /// optionally vacuum old Lance versions while preserving one atomic
    /// three-table CURRENT snapshot.
    pub async fn maintain(
        &self,
        options: &LanceMaintenanceOptions,
    ) -> Result<StorylineMaintenanceReport> {
        let _guard = self.acquire_write_guard().await?;
        let Some(paths) = self.resolve_current_table_paths().await? else {
            return Ok(StorylineMaintenanceReport::default());
        };
        let (runs, steps, tool_calls) = tokio::try_join!(
            maintain_table_layout(&paths.runs, paths.runs_version, &RUN_INDEXES, options,),
            maintain_table_layout(&paths.steps, paths.steps_version, &STEP_INDEXES, options,),
            maintain_table_layout(
                &paths.tool_calls,
                paths.tool_calls_version,
                &TOOL_CALL_INDEXES,
                options,
            ),
        )?;
        let runs_version = runs
            .final_version
            .context("missing maintained runs version")?;
        let steps_version = steps
            .final_version
            .context("missing maintained steps version")?;
        let tool_calls_version = tool_calls
            .final_version
            .context("missing maintained tool_calls version")?;
        let run_content_columns = content_column_projection(StorylineTableKind::Runs);
        let step_content_columns = content_column_projection(StorylineTableKind::Steps);
        let tool_call_content_columns = content_column_projection(StorylineTableKind::ToolCalls);
        let (run_batches, step_batches, tool_call_batches) = tokio::try_join!(
            read_projected_batches(&paths.runs, runs_version, &run_content_columns, None),
            read_projected_batches(&paths.steps, steps_version, &step_content_columns, None),
            read_projected_batches(
                &paths.tool_calls,
                tool_calls_version,
                &tool_call_content_columns,
                None,
            ),
        )?;
        let mut live_objects = collect_content_ids(&run_batches, StorylineTableKind::Runs)?;
        live_objects.extend(collect_content_ids(
            &step_batches,
            StorylineTableKind::Steps,
        )?);
        live_objects.extend(collect_content_ids(
            &tool_call_batches,
            StorylineTableKind::ToolCalls,
        )?);
        let (objects_version, objects_removed) =
            prune_unreferenced_objects(&paths.objects, paths.objects_version, &live_objects)
                .await?;
        let generation = next_generation();
        self.commit_snapshot(
            &StorylineSnapshotPointer {
                generation: generation.clone(),
                parent_generation: Some(paths.generation.clone()),
                table_generation: paths.table_generation.clone(),
                runs_version,
                steps_version,
                tool_calls_version,
                objects_version,
            },
            Some(&paths.generation),
        )
        .await?;

        let (runs_vacuum, steps_vacuum, tool_calls_vacuum) = tokio::try_join!(
            vacuum_table(&paths.runs, options.vacuum_older_than),
            vacuum_table(&paths.steps, options.vacuum_older_than),
            vacuum_table(&paths.tool_calls, options.vacuum_older_than),
        )?;
        Ok(StorylineMaintenanceReport {
            generation: Some(generation),
            runs: merge_maintenance_reports(runs, runs_vacuum),
            steps: merge_maintenance_reports(steps, steps_vacuum),
            tool_calls: merge_maintenance_reports(tool_calls, tool_calls_vacuum),
            objects_removed,
        })
    }

    /// Atomically replace multiple Storylines in one snapshot.
    ///
    /// This is the preferred ingestion path for imports and benchmarks: all
    /// documents are validated before the lock is acquired, and a new store
    /// builds each table and its indices only once.
    pub async fn replace_storylines(&self, stories: &[StorylineDocument]) -> Result<()> {
        if stories.is_empty() {
            return Ok(());
        }
        // Validation and normalization happen before taking the writer lock or
        // creating a generation, so invalid input cannot affect committed data.
        let replacements = stories
            .iter()
            .map(split_storyline)
            .collect::<crate::Result<Vec<_>>>()
            .map_err(anyhow::Error::from)?;
        let mut session_ids = HashSet::with_capacity(replacements.len());
        for replacement in &replacements {
            if !session_ids.insert(replacement.run.session_id.clone()) {
                anyhow::bail!(
                    "duplicate session_id '{}' in Storyline batch",
                    replacement.run.session_id
                );
            }
        }
        let _guard = self.acquire_write_guard().await?;

        let mut runs = Vec::with_capacity(replacements.len());
        let mut steps = Vec::new();
        let mut tool_calls = Vec::new();
        for replacement in replacements {
            runs.push(replacement.run);
            steps.extend(replacement.steps);
            tool_calls.extend(replacement.tool_calls);
        }
        sort_rows(&mut runs, &mut steps, &mut tool_calls);
        match self.resolve_current_table_paths().await? {
            None => self.create_initial_snapshot(runs, steps, tool_calls).await,
            Some(paths) => {
                self.replace_snapshot_rows(&paths, &session_ids, runs, steps, tool_calls)
                    .await
            }
        }
    }

    /// Materialize one complete Storyline, including every step, tool call, and
    /// offloaded content object.
    pub async fn get_storyline_full(&self, session_id: &str) -> Result<Option<StorylineDocument>> {
        Ok(self
            .get_storylines_full(&[session_id.to_string()])
            .await?
            .into_iter()
            .next()
            .flatten())
    }

    /// Read multiple Storylines from one committed three-table snapshot.
    ///
    /// The returned vector is aligned with `session_ids`; missing sessions are
    /// represented by `None`. All requested rows are fetched with one indexed
    /// predicate per table rather than one store open per session.
    pub async fn get_storylines_full(
        &self,
        session_ids: &[String],
    ) -> Result<Vec<Option<StorylineDocument>>> {
        if session_ids.is_empty() {
            return Ok(Vec::new());
        }
        let requested = session_ids.iter().cloned().collect::<HashSet<_>>();
        anyhow::ensure!(
            requested.len() == session_ids.len(),
            "duplicate session_id in Storyline point batch"
        );
        let Some(paths) = self.resolve_current_table_paths().await? else {
            return Ok(vec![None; session_ids.len()]);
        };
        let predicate = session_set_predicate(&requested);
        let (run_batches, step_batches, tool_call_batches) = tokio::try_join!(
            read_filtered_batches(&paths.runs, paths.runs_version, &predicate),
            read_filtered_batches(&paths.steps, paths.steps_version, &predicate),
            read_filtered_batches(&paths.tool_calls, paths.tool_calls_version, &predicate),
        )?;
        let objects = Arc::new(open_objects(&paths.objects, paths.objects_version).await?);
        let (run_batches, step_batches, tool_call_batches) = tokio::try_join!(
            hydrate_batches(&objects, run_batches, StorylineTableKind::Runs),
            hydrate_batches(&objects, step_batches, StorylineTableKind::Steps),
            hydrate_batches(&objects, tool_call_batches, StorylineTableKind::ToolCalls,),
        )?;
        let mut runs = HashMap::with_capacity(session_ids.len());
        for run in decode_run_batches(&run_batches)? {
            let session_id = run.session_id.clone();
            if runs.insert(session_id.clone(), run).is_some() {
                anyhow::bail!("duplicate runs rows for session_id '{session_id}'");
            }
        }
        let mut steps = HashMap::<String, Vec<StoryStepRow>>::new();
        for step in decode_step_batches(&step_batches)? {
            steps.entry(step.session_id.clone()).or_default().push(step);
        }
        let mut tool_calls = HashMap::<String, Vec<StoryToolCallRow>>::new();
        for tool_call in decode_tool_call_batches(&tool_call_batches)? {
            tool_calls
                .entry(tool_call.session_id.clone())
                .or_default()
                .push(tool_call);
        }

        session_ids
            .iter()
            .map(|session_id| {
                let Some(run) = runs.remove(session_id) else {
                    return Ok(None);
                };
                reconstruct_storyline(StorylineTables {
                    run,
                    steps: steps.remove(session_id).unwrap_or_default(),
                    tool_calls: tool_calls.remove(session_id).unwrap_or_default(),
                })
                .map(Some)
                .map_err(anyhow::Error::from)
            })
            .collect()
    }

    /// List scalar-only run summaries with stable, generation-pinned cursors.
    pub async fn list_run_summaries(
        &self,
        request: StorylinePageRequest<StorylineRunCursor>,
    ) -> Result<StorylinePage<StorylineRunSummary, StorylineRunCursor>> {
        validate_page_limit(request.limit)?;
        let Some(paths) = self.resolve_current_table_paths().await? else {
            anyhow::ensure!(
                request.after.is_none(),
                "cannot continue an empty Storyline store"
            );
            return Ok(empty_page());
        };
        validate_cursor_generation(
            request
                .after
                .as_ref()
                .map(|cursor| cursor.generation.as_str()),
            &paths.generation,
        )?;
        let predicate = request
            .after
            .as_ref()
            .map(|cursor| format!("session_id > '{}'", sql_string(&cursor.session_id)));
        let batches = read_ordered_page_batches(
            &paths.runs,
            paths.runs_version,
            &[
                "run_id",
                "session_id",
                "schema_version",
                "agent_id",
                "agent_name",
                "agent_version",
                "agent_model_name",
            ],
            predicate.as_deref(),
            &["session_id"],
            request.limit + 1,
        )
        .await?;
        let mut items = decode_run_summary_batches(&batches)?;
        items.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        ensure_unique_by(&items, |item| item.session_id.as_str(), "run session_id")?;
        let has_more = items.len() > request.limit;
        items.truncate(request.limit);
        let next_cursor = has_more.then(|| StorylineRunCursor {
            generation: paths.generation.clone(),
            session_id: items.last().expect("non-empty page").session_id.clone(),
        });
        Ok(StorylinePage {
            generation: Some(paths.generation),
            items,
            next_cursor,
        })
    }

    /// Read one ordered page of complete steps. The first scan reads only step
    /// IDs; full columns and content objects are fetched for the selected page.
    pub async fn list_steps_page(
        &self,
        session_id: &str,
        request: StorylinePageRequest<StorylineStepCursor>,
    ) -> Result<StorylinePage<StoryStepRow, StorylineStepCursor>> {
        validate_page_limit(request.limit)?;
        let Some(paths) = self.resolve_current_table_paths().await? else {
            anyhow::ensure!(
                request.after.is_none(),
                "cannot continue an empty Storyline store"
            );
            return Ok(empty_page());
        };
        validate_detail_cursor(
            request
                .after
                .as_ref()
                .map(|cursor| (cursor.generation.as_str(), cursor.session_id.as_str())),
            &paths.generation,
            session_id,
        )?;
        let mut predicate = session_predicate(session_id);
        if let Some(cursor) = &request.after {
            predicate.push_str(&format!(" AND step_id > {}", cursor.step_id));
        }
        let key_batches = read_ordered_page_batches(
            &paths.steps,
            paths.steps_version,
            &["step_id"],
            Some(&predicate),
            &["step_id"],
            request.limit + 1,
        )
        .await?;
        let mut ids = decode_i64_column(&key_batches, "step_id")?;
        ids.sort_unstable();
        ensure_unique_values(&ids, "step_id")?;
        let has_more = ids.len() > request.limit;
        ids.truncate(request.limit);
        let items = if ids.is_empty() {
            Vec::new()
        } else {
            let predicate = format!(
                "{} AND step_id IN ({})",
                session_predicate(session_id),
                ids.iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let batches =
                read_filtered_batches(&paths.steps, paths.steps_version, &predicate).await?;
            let objects = Arc::new(open_objects(&paths.objects, paths.objects_version).await?);
            let batches = hydrate_batches(&objects, batches, StorylineTableKind::Steps).await?;
            let mut rows = decode_step_batches(&batches)?;
            rows.sort_by_key(|row| row.step_id);
            rows
        };
        let next_cursor = has_more.then(|| StorylineStepCursor {
            generation: paths.generation.clone(),
            session_id: session_id.to_string(),
            step_id: *ids.last().expect("non-empty page"),
        });
        Ok(StorylinePage {
            generation: Some(paths.generation),
            items,
            next_cursor,
        })
    }

    /// Read one ordered page of complete tool calls. Blob hydration is delayed
    /// until after `(step_id, call_index)` selects the requested page.
    pub async fn list_tool_calls_page(
        &self,
        session_id: &str,
        request: StorylinePageRequest<StorylineToolCallCursor>,
    ) -> Result<StorylinePage<StoryToolCallRow, StorylineToolCallCursor>> {
        validate_page_limit(request.limit)?;
        let Some(paths) = self.resolve_current_table_paths().await? else {
            anyhow::ensure!(
                request.after.is_none(),
                "cannot continue an empty Storyline store"
            );
            return Ok(empty_page());
        };
        validate_detail_cursor(
            request
                .after
                .as_ref()
                .map(|cursor| (cursor.generation.as_str(), cursor.session_id.as_str())),
            &paths.generation,
            session_id,
        )?;
        let mut predicate = session_predicate(session_id);
        if let Some(cursor) = &request.after {
            predicate.push_str(&format!(
                " AND (step_id > {} OR (step_id = {} AND call_index > {}))",
                cursor.step_id, cursor.step_id, cursor.call_index
            ));
        }
        let key_batches = read_ordered_page_batches(
            &paths.tool_calls,
            paths.tool_calls_version,
            &["step_id", "call_index"],
            Some(&predicate),
            &["step_id", "call_index"],
            request.limit + 1,
        )
        .await?;
        let mut keys = decode_i64_pair(&key_batches, "step_id", "call_index")?;
        keys.sort_unstable();
        ensure_unique_values(&keys, "tool-call (step_id, call_index)")?;
        let has_more = keys.len() > request.limit;
        keys.truncate(request.limit);
        let items = if keys.is_empty() {
            Vec::new()
        } else {
            let selected = keys
                .iter()
                .map(|(step_id, call_index)| {
                    format!("(step_id = {step_id} AND call_index = {call_index})")
                })
                .collect::<Vec<_>>()
                .join(" OR ");
            let predicate = format!("{} AND ({selected})", session_predicate(session_id));
            let batches =
                read_filtered_batches(&paths.tool_calls, paths.tool_calls_version, &predicate)
                    .await?;
            let objects = Arc::new(open_objects(&paths.objects, paths.objects_version).await?);
            let batches = hydrate_batches(&objects, batches, StorylineTableKind::ToolCalls).await?;
            let mut rows = decode_tool_call_batches(&batches)?;
            rows.sort_by_key(|row| (row.step_id, row.call_index));
            rows
        };
        let next_cursor = has_more.then(|| {
            let (step_id, call_index) = *keys.last().expect("non-empty page");
            StorylineToolCallCursor {
                generation: paths.generation.clone(),
                session_id: session_id.to_string(),
                step_id,
                call_index,
            }
        });
        Ok(StorylinePage {
            generation: Some(paths.generation),
            items,
            next_cursor,
        })
    }

    async fn create_initial_snapshot(
        &self,
        runs: Vec<StoryRunRow>,
        steps: Vec<StoryStepRow>,
        tool_calls: Vec<StoryToolCallRow>,
    ) -> Result<()> {
        let generation = next_generation();
        let paths = self.paths_for_generation(&generation);
        let ExternalizedStorylineBatches {
            runs: run_batches,
            steps: step_batches,
            tool_calls: tool_call_batches,
            pending,
        } = externalize_rows(runs, steps, tool_calls, self.content_options)?;
        let write_result = async {
            // Objects become durable before any descriptor can become visible.
            // A later failure can leave unreachable objects, never dangling refs.
            let objects_version = commit_pending_content(&paths.objects, None, pending).await?;
            let (runs_version, steps_version, tool_calls_version) = tokio::try_join!(
                write_batches(
                    &paths.runs,
                    run_batches,
                    story_runs_arrow_schema(),
                    &RUN_INDEXES,
                ),
                write_batches(
                    &paths.steps,
                    step_batches,
                    story_steps_arrow_schema(),
                    &STEP_INDEXES,
                ),
                write_batches(
                    &paths.tool_calls,
                    tool_call_batches,
                    story_tool_calls_arrow_schema(),
                    &TOOL_CALL_INDEXES,
                ),
            )?;
            self.commit_snapshot(
                &StorylineSnapshotPointer {
                    generation: generation.clone(),
                    parent_generation: None,
                    table_generation: generation.clone(),
                    runs_version,
                    steps_version,
                    tool_calls_version,
                    objects_version,
                },
                None,
            )
            .await
        }
        .await;
        if write_result.is_err() {
            let _ = self
                .object_store
                .remove_dir_all(self.generation_object_path(&generation))
                .await;
        }
        write_result
    }

    async fn replace_snapshot_rows(
        &self,
        paths: &StorylineTablePaths,
        session_ids: &HashSet<String>,
        runs: Vec<StoryRunRow>,
        steps: Vec<StoryStepRow>,
        tool_calls: Vec<StoryToolCallRow>,
    ) -> Result<()> {
        let predicate = session_set_predicate(session_ids);
        let ExternalizedStorylineBatches {
            runs: run_batches,
            steps: step_batches,
            tool_calls: tool_call_batches,
            pending,
        } = externalize_rows(runs, steps, tool_calls, self.content_options)?;
        let objects_version =
            commit_pending_content(&paths.objects, Some(paths.objects_version), pending).await?;
        let (runs_version, steps_version, tool_calls_version) = tokio::try_join!(
            replace_table_batches(
                &paths.runs,
                paths.runs_version,
                &predicate,
                run_batches,
                story_runs_arrow_schema(),
            ),
            replace_table_batches(
                &paths.steps,
                paths.steps_version,
                &predicate,
                step_batches,
                story_steps_arrow_schema(),
            ),
            replace_table_batches(
                &paths.tool_calls,
                paths.tool_calls_version,
                &predicate,
                tool_call_batches,
                story_tool_calls_arrow_schema(),
            ),
        )?;
        self.commit_snapshot(
            &StorylineSnapshotPointer {
                generation: next_generation(),
                parent_generation: Some(paths.generation.clone()),
                table_generation: paths.table_generation.clone(),
                runs_version,
                steps_version,
                tool_calls_version,
                objects_version,
            },
            Some(&paths.generation),
        )
        .await
    }

    fn paths_for_generation(&self, generation: &str) -> StorylineTablePaths {
        let base = join_location(&self.root_uri, &[GENERATIONS_DIR, generation]);
        StorylineTablePaths {
            generation: generation.to_string(),
            table_generation: generation.to_string(),
            runs: PathBuf::from(join_location(
                &base,
                &[&format!("{STORY_RUNS_TABLE}.lance")],
            )),
            steps: PathBuf::from(join_location(
                &base,
                &[&format!("{STORY_STEPS_TABLE}.lance")],
            )),
            tool_calls: PathBuf::from(join_location(
                &base,
                &[&format!("{STORY_TOOL_CALLS_TABLE}.lance")],
            )),
            objects: PathBuf::from(join_location(&self.root_uri, &[STORYLINE_OBJECTS_DATASET])),
            runs_version: 0,
            steps_version: 0,
            tool_calls_version: 0,
            objects_version: 0,
        }
    }

    fn generation_object_path(&self, generation: &str) -> ObjectPath {
        self.object_root
            .clone()
            .join(GENERATIONS_DIR)
            .join(generation)
    }

    async fn commit_snapshot(
        &self,
        snapshot: &StorylineSnapshotPointer,
        expected_generation: Option<&str>,
    ) -> Result<()> {
        let pointer = self.object_root.clone().join(CURRENT_FILE);
        let contents = serde_json::to_vec(snapshot).context("encode Storyline snapshot pointer")?;
        let current = self.read_current_pointer().await?;
        let actual_generation = current
            .pointer
            .as_ref()
            .map(|pointer| pointer.generation.as_str());
        anyhow::ensure!(
            actual_generation == expected_generation,
            "Storyline commit conflict: expected CURRENT generation {:?}, found {:?}",
            expected_generation,
            actual_generation
        );

        if matches!(self.storage_scheme(), "file" | "file+uring") {
            write_local_current(self.root.join(CURRENT_FILE), contents).await?;
            return Ok(());
        }

        let mode = match current.version {
            None => PutMode::Create,
            Some(version) => PutMode::Update(version),
        };
        match self
            .object_store
            .inner
            .put_opts(&pointer, contents.into(), mode.into())
            .await
        {
            Ok(_) => Ok(()),
            Err(ObjectStoreError::AlreadyExists { .. })
            | Err(ObjectStoreError::Precondition { .. }) => anyhow::bail!(
                "Storyline commit conflict while publishing generation {}",
                snapshot.generation
            ),
            Err(error) => Err(error)
                .with_context(|| format!("commit Storyline generation {}", snapshot.generation)),
        }
    }
}

async fn write_local_current(path: PathBuf, contents: Vec<u8>) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<()> {
        let parent = path
            .parent()
            .context("Storyline CURRENT path has no parent")?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create Storyline root {}", parent.display()))?;
        let temporary = path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .with_context(|| format!("create Storyline CURRENT temp {}", temporary.display()))?;
        file.write_all(&contents)
            .with_context(|| format!("write Storyline CURRENT temp {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync Storyline CURRENT temp {}", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("publish Storyline CURRENT {}", path.display()))?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })
    .await
    .context("join Storyline CURRENT commit task")?
}

async fn validate_table(generation: &str, path: &Path, version: u64) -> Result<()> {
    let dataset = Dataset::open(path.to_string_lossy().as_ref())
        .await
        .with_context(|| {
            format!(
                "Storyline generation '{}' is incomplete: cannot open {}",
                generation,
                path.display()
            )
        })?;
    dataset.checkout_version(version).await.with_context(|| {
        format!(
            "Storyline generation '{generation}' references missing version {version} of {}",
            path.display()
        )
    })?;
    Ok(())
}

fn normalize_root_uri(value: &str) -> Result<String> {
    let mut value = value.trim().to_string();
    anyhow::ensure!(!value.is_empty(), "Storyline Lance root must not be empty");
    let minimum = value.find("://").map_or(1, |index| index + 3);
    while value.len() > minimum && value.ends_with('/') {
        value.pop();
    }
    Ok(value)
}

fn join_location(root: &str, parts: &[&str]) -> String {
    let mut location = root.to_string();
    for part in parts {
        if !location.ends_with('/') {
            location.push('/');
        }
        location.push_str(part.trim_matches('/'));
    }
    location
}

fn validate_generation_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || !value.starts_with("gen-")
    {
        anyhow::bail!("invalid Storyline generation name '{value}'");
    }
    Ok(())
}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);

fn next_generation() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    format!("gen-{nanos}-{}-{sequence}", std::process::id())
}

fn sort_rows(
    runs: &mut [StoryRunRow],
    steps: &mut [StoryStepRow],
    tool_calls: &mut [StoryToolCallRow],
) {
    runs.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    steps.sort_by(|a, b| {
        a.session_id
            .cmp(&b.session_id)
            .then(a.step_id.cmp(&b.step_id))
    });
    tool_calls.sort_by(|a, b| {
        a.session_id
            .cmp(&b.session_id)
            .then(a.step_id.cmp(&b.step_id))
            .then(a.call_index.cmp(&b.call_index))
    });
}

struct StorylineStreamChunk {
    session_ids: HashSet<String>,
    runs: Vec<StoryRunRow>,
    steps: Vec<StoryStepRow>,
    tool_calls: Vec<StoryToolCallRow>,
}

fn next_storyline_stream_chunk<I>(
    iterator: &mut I,
    all_session_ids: &mut HashSet<String>,
) -> Result<Option<StorylineStreamChunk>>
where
    I: Iterator<Item = Result<StorylineDocument>>,
{
    let mut session_ids = HashSet::with_capacity(STREAM_IMPORT_STORIES);
    let mut runs = Vec::with_capacity(STREAM_IMPORT_STORIES);
    let mut steps = Vec::new();
    let mut tool_calls = Vec::new();
    while runs.len() < STREAM_IMPORT_STORIES {
        let Some(story) = iterator.next() else {
            break;
        };
        let tables = split_storyline(&story?).map_err(anyhow::Error::from)?;
        let session_id = tables.run.session_id.clone();
        if !all_session_ids.insert(session_id.clone()) {
            anyhow::bail!("duplicate session_id '{session_id}' in Storyline stream");
        }
        session_ids.insert(session_id);
        runs.push(tables.run);
        steps.extend(tables.steps);
        tool_calls.extend(tables.tool_calls);
    }
    if runs.is_empty() {
        return Ok(None);
    }
    sort_rows(&mut runs, &mut steps, &mut tool_calls);
    Ok(Some(StorylineStreamChunk {
        session_ids,
        runs,
        steps,
        tool_calls,
    }))
}

struct EncodedBatchIterator<T> {
    rows: std::sync::Arc<[T]>,
    offset: usize,
    emitted_empty: bool,
    encode: fn(&[T]) -> Result<RecordBatch>,
}

impl<T> EncodedBatchIterator<T> {
    fn new(rows: Vec<T>, encode: fn(&[T]) -> Result<RecordBatch>) -> Self {
        Self {
            rows: rows.into(),
            offset: 0,
            emitted_empty: false,
            encode,
        }
    }
}

impl<T> Iterator for EncodedBatchIterator<T> {
    type Item = std::result::Result<RecordBatch, ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rows.is_empty() {
            if self.emitted_empty {
                return None;
            }
            self.emitted_empty = true;
            return Some(
                (self.encode)(&[]).map_err(|error| ArrowError::ComputeError(error.to_string())),
            );
        }
        if self.offset >= self.rows.len() {
            return None;
        }
        let end = (self.offset + WRITE_BATCH_ROWS).min(self.rows.len());
        let result = (self.encode)(&self.rows[self.offset..end])
            .map_err(|error| ArrowError::ComputeError(error.to_string()));
        self.offset = end;
        Some(result)
    }
}

fn encode_rows<T>(
    rows: Vec<T>,
    encode: fn(&[T]) -> Result<RecordBatch>,
) -> Result<Vec<RecordBatch>> {
    EncodedBatchIterator::new(rows, encode)
        .map(|batch| batch.map_err(anyhow::Error::from))
        .collect()
}

struct ExternalizedStorylineBatches {
    runs: Vec<RecordBatch>,
    steps: Vec<RecordBatch>,
    tool_calls: Vec<RecordBatch>,
    pending: PendingContent,
}

fn externalize_rows(
    runs: Vec<StoryRunRow>,
    steps: Vec<StoryStepRow>,
    tool_calls: Vec<StoryToolCallRow>,
    options: StorylineContentOptions,
) -> Result<ExternalizedStorylineBatches> {
    let mut pending = PendingContent::default();
    let runs = externalize_batches(
        encode_rows(runs, story_runs_to_batch)?,
        StorylineTableKind::Runs,
        options,
        &mut pending,
    )?;
    let steps = externalize_batches(
        encode_rows(steps, story_steps_to_batch)?,
        StorylineTableKind::Steps,
        options,
        &mut pending,
    )?;
    let tool_calls = externalize_batches(
        encode_rows(tool_calls, story_tool_calls_to_batch)?,
        StorylineTableKind::ToolCalls,
        options,
        &mut pending,
    )?;
    Ok(ExternalizedStorylineBatches {
        runs,
        steps,
        tool_calls,
        pending,
    })
}

fn batch_reader(
    batches: Vec<RecordBatch>,
    schema: SchemaRef,
) -> RecordBatchIterator<impl Iterator<Item = std::result::Result<RecordBatch, ArrowError>>> {
    RecordBatchIterator::new(batches.into_iter().map(Ok), schema)
}

async fn write_batches(
    path: &Path,
    batches: Vec<RecordBatch>,
    schema: SchemaRef,
    indexes: &[(&str, IndexType)],
) -> Result<u64> {
    write_record_batch_reader(path, Box::new(batch_reader(batches, schema)), indexes).await
}

async fn replace_table_batches(
    path: &Path,
    snapshot_version: u64,
    predicate: &str,
    batches: Vec<RecordBatch>,
    schema: SchemaRef,
) -> Result<u64> {
    let mut dataset = open_table_version(path, snapshot_version).await?;
    let latest_version = latest_table_version(path).await?;
    if latest_version != snapshot_version {
        dataset.restore().await.with_context(|| {
            format!(
                "restore committed Storyline table version {} for {}",
                snapshot_version,
                path.display()
            )
        })?;
    }
    dataset
        .delete(predicate)
        .await
        .with_context(|| format!("replace rows in Storyline table {}", path.display()))?;
    let has_rows = batches.iter().any(|batch| batch.num_rows() > 0);
    if has_rows {
        dataset = InsertBuilder::new(Arc::new(dataset))
            .with_params(&WriteParams {
                mode: WriteMode::Append,
                ..Default::default()
            })
            .execute_stream(batch_reader(batches, schema))
            .await
            .with_context(|| format!("append replacement rows to {}", path.display()))?;
    }
    Ok(dataset.version_id())
}

async fn write_record_batch_reader(
    path: &Path,
    reader: Box<dyn RecordBatchReader + Send>,
    indexes: &[(&str, IndexType)],
) -> Result<u64> {
    let uri = path.to_string_lossy().into_owned();
    let mut dataset = InsertBuilder::new(&uri)
        .with_params(&WriteParams {
            mode: WriteMode::Create,
            ..Default::default()
        })
        .execute_stream(reader)
        .await
        .with_context(|| format!("stream ATIF into Storyline table {}", path.display()))?;
    if dataset.count_rows(None).await? > 0 {
        for (column, index_type) in indexes {
            let builtin = match index_type {
                IndexType::Bitmap => BuiltinIndexType::Bitmap,
                _ => BuiltinIndexType::BTree,
            };
            let _admission = super::index_build_gate::acquire().await;
            dataset
                .create_index(
                    &[*column],
                    *index_type,
                    Some(format!("pchronicle_{column}_idx")),
                    &ScalarIndexParams::for_builtin(builtin),
                    false,
                )
                .await
                .with_context(|| {
                    format!(
                        "create {:?} index on {}.{}",
                        index_type,
                        path.display(),
                        column
                    )
                })?;
        }
    }
    Ok(dataset.version_id())
}

async fn ensure_table_indexes(dataset: &mut Dataset, indexes: &[(&str, IndexType)]) -> Result<()> {
    if dataset.count_rows(None).await? == 0 {
        return Ok(());
    }
    for (column, index_type) in indexes {
        let name = format!("pchronicle_{column}_idx");
        if !dataset.load_indices_by_name(&name).await?.is_empty() {
            continue;
        }
        let builtin = match index_type {
            IndexType::Bitmap => BuiltinIndexType::Bitmap,
            _ => BuiltinIndexType::BTree,
        };
        let _admission = super::index_build_gate::acquire().await;
        dataset
            .create_index(
                &[*column],
                *index_type,
                Some(name),
                &ScalarIndexParams::for_builtin(builtin),
                false,
            )
            .await?;
    }
    Ok(())
}

async fn maintain_table_layout(
    path: &Path,
    snapshot_version: u64,
    indexes: &[(&str, IndexType)],
    options: &LanceMaintenanceOptions,
) -> Result<LanceMaintenanceReport> {
    let mut dataset = open_table_version(path, snapshot_version).await?;
    let latest_version = latest_table_version(path).await?;
    if latest_version != snapshot_version {
        dataset.restore().await.with_context(|| {
            format!(
                "restore Storyline table {} to committed version {snapshot_version}",
                path.display()
            )
        })?;
    }
    if options.optimize_indices {
        ensure_table_indexes(&mut dataset, indexes)
            .await
            .with_context(|| format!("ensure Storyline indices for {}", path.display()))?;
        dataset
            .optimize_indices(&OptimizeOptions::append())
            .await
            .with_context(|| format!("optimize Storyline indices for {}", path.display()))?;
    }
    let mut report = LanceMaintenanceReport::default();
    if options.compact {
        let metrics = compact_files(
            &mut dataset,
            CompactionOptions {
                target_rows_per_fragment: options.target_rows_per_fragment,
                ..Default::default()
            },
            None,
        )
        .await
        .with_context(|| format!("compact Storyline table {}", path.display()))?;
        report.fragments_removed = metrics.fragments_removed;
        report.fragments_added = metrics.fragments_added;
    }
    report.final_version = Some(dataset.version_id());
    Ok(report)
}

async fn vacuum_table(
    path: &Path,
    retention: Option<std::time::Duration>,
) -> Result<LanceMaintenanceReport> {
    let Some(retention) = retention else {
        return Ok(LanceMaintenanceReport::default());
    };
    let dataset = Dataset::open(path.to_string_lossy().as_ref())
        .await
        .with_context(|| format!("open Storyline table {} for vacuum", path.display()))?;
    let retention = chrono::Duration::from_std(retention)
        .context("Storyline Lance vacuum retention is too large")?;
    let removed = dataset
        .cleanup_old_versions(retention, Some(false), Some(true))
        .await
        .with_context(|| format!("vacuum Storyline table {}", path.display()))?;
    Ok(LanceMaintenanceReport {
        old_versions_removed: removed.old_versions,
        bytes_removed: removed.bytes_removed,
        ..Default::default()
    })
}

fn merge_maintenance_reports(
    mut layout: LanceMaintenanceReport,
    vacuum: LanceMaintenanceReport,
) -> LanceMaintenanceReport {
    layout.old_versions_removed += vacuum.old_versions_removed;
    layout.bytes_removed += vacuum.bytes_removed;
    layout
}

async fn latest_table_version(path: &Path) -> Result<u64> {
    Ok(Dataset::open(path.to_string_lossy().as_ref())
        .await
        .with_context(|| format!("open Storyline Lance table {}", path.display()))?
        .version_id())
}

async fn open_table_version(path: &Path, version: u64) -> Result<Dataset> {
    let dataset = Dataset::open(path.to_string_lossy().as_ref())
        .await
        .with_context(|| format!("open Storyline Lance table {}", path.display()))?;
    dataset.checkout_version(version).await.with_context(|| {
        format!(
            "open Storyline Lance table {} at version {version}",
            path.display()
        )
    })
}

fn content_column_projection(kind: StorylineTableKind) -> Vec<&'static str> {
    content_columns(kind)
        .iter()
        .map(|(column, _)| *column)
        .collect()
}

async fn read_projected_batches(
    path: &Path,
    version: u64,
    projection: &[&str],
    predicate: Option<&str>,
) -> Result<Vec<RecordBatch>> {
    read_scan_batches(path, version, projection, predicate, &[], None).await
}

async fn read_ordered_page_batches(
    path: &Path,
    version: u64,
    projection: &[&str],
    predicate: Option<&str>,
    ordering: &[&str],
    limit: usize,
) -> Result<Vec<RecordBatch>> {
    read_scan_batches(path, version, projection, predicate, ordering, Some(limit)).await
}

async fn read_scan_batches(
    path: &Path,
    version: u64,
    projection: &[&str],
    predicate: Option<&str>,
    ordering: &[&str],
    limit: Option<usize>,
) -> Result<Vec<RecordBatch>> {
    let dataset = open_table_version(path, version).await?;
    let mut scan = dataset.scan();
    if !projection.is_empty() {
        scan.project(projection)
            .with_context(|| format!("project Storyline Lance table {}", path.display()))?;
    }
    if let Some(predicate) = predicate {
        scan.filter(predicate)
            .with_context(|| format!("filter Storyline Lance table {}", path.display()))?;
        scan.use_scalar_index(true);
    }
    if !ordering.is_empty() {
        scan.order_by(Some(
            ordering
                .iter()
                .map(|column| ColumnOrdering::asc_nulls_first((*column).to_string()))
                .collect(),
        ))
        .with_context(|| format!("order Storyline Lance table {}", path.display()))?;
    }
    if let Some(limit) = limit {
        scan.limit(Some(i64::try_from(limit)?), None)
            .with_context(|| format!("limit Storyline Lance table {}", path.display()))?;
    }
    scan.try_into_stream()
        .await
        .with_context(|| format!("scan Storyline Lance table {}", path.display()))?
        .try_collect()
        .await
        .with_context(|| format!("read Storyline Lance table {}", path.display()))
}

async fn read_filtered_batches(
    path: &Path,
    version: u64,
    predicate: &str,
) -> Result<Vec<RecordBatch>> {
    read_projected_batches(path, version, &[], Some(predicate)).await
}

fn session_predicate(session_id: &str) -> String {
    format!("session_id = '{}'", sql_string(session_id))
}

fn sql_string(value: &str) -> String {
    value.replace('\'', "''")
}

fn session_set_predicate(session_ids: &HashSet<String>) -> String {
    let mut values = session_ids
        .iter()
        .map(|session_id| format!("'{}'", session_id.replace('\'', "''")))
        .collect::<Vec<_>>();
    values.sort();
    format!("session_id IN ({})", values.join(", "))
}

fn decode_run_batches(batches: &[RecordBatch]) -> Result<Vec<StoryRunRow>> {
    Ok(batches
        .iter()
        .map(story_runs_from_batch)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

fn decode_run_summary_batches(batches: &[RecordBatch]) -> Result<Vec<StorylineRunSummary>> {
    let mut summaries = Vec::new();
    for batch in batches {
        for row in 0..batch.num_rows() {
            summaries.push(StorylineRunSummary {
                run_id: required_string_column(batch, "run_id", row)?,
                session_id: required_string_column(batch, "session_id", row)?,
                schema_version: required_string_column(batch, "schema_version", row)?,
                agent_id: required_string_column(batch, "agent_id", row)?,
                agent_name: optional_string_column(batch, "agent_name", row)?,
                agent_version: optional_string_column(batch, "agent_version", row)?,
                agent_model_name: optional_string_column(batch, "agent_model_name", row)?,
            });
        }
    }
    Ok(summaries)
}

fn optional_string_column(batch: &RecordBatch, name: &str, row: usize) -> Result<Option<String>> {
    let column = batch
        .column_by_name(name)
        .ok_or_else(|| anyhow::anyhow!("batch missing column '{name}'"))?;
    let array = column
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow::anyhow!("expected Utf8 column '{name}'"))?;
    Ok((!array.is_null(row)).then(|| array.value(row).to_string()))
}

fn required_string_column(batch: &RecordBatch, name: &str, row: usize) -> Result<String> {
    optional_string_column(batch, name, row)?
        .ok_or_else(|| anyhow::anyhow!("null required column '{name}'"))
}

fn decode_i64_column(batches: &[RecordBatch], name: &str) -> Result<Vec<i64>> {
    let mut values = Vec::new();
    for batch in batches {
        let column = batch
            .column_by_name(name)
            .ok_or_else(|| anyhow::anyhow!("batch missing column '{name}'"))?;
        let array = column
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| anyhow::anyhow!("expected Int64 column '{name}'"))?;
        for row in 0..batch.num_rows() {
            anyhow::ensure!(!array.is_null(row), "null required column '{name}'");
            values.push(array.value(row));
        }
    }
    Ok(values)
}

fn decode_i64_pair(batches: &[RecordBatch], first: &str, second: &str) -> Result<Vec<(i64, i64)>> {
    let mut values = Vec::new();
    for batch in batches {
        let first_values = decode_i64_column(std::slice::from_ref(batch), first)?;
        let second_values = decode_i64_column(std::slice::from_ref(batch), second)?;
        values.extend(first_values.into_iter().zip(second_values));
    }
    Ok(values)
}

fn validate_page_limit(limit: usize) -> Result<()> {
    anyhow::ensure!(limit > 0, "Storyline page limit must be greater than zero");
    anyhow::ensure!(
        limit <= MAX_STORYLINE_PAGE_SIZE,
        "Storyline page limit {limit} exceeds maximum {MAX_STORYLINE_PAGE_SIZE}"
    );
    Ok(())
}

fn validate_cursor_generation(cursor: Option<&str>, generation: &str) -> Result<()> {
    if let Some(cursor) = cursor {
        anyhow::ensure!(
            cursor == generation,
            "Storyline changed while paging: cursor generation '{cursor}', current generation '{generation}'; restart pagination"
        );
    }
    Ok(())
}

fn validate_detail_cursor(
    cursor: Option<(&str, &str)>,
    generation: &str,
    session_id: &str,
) -> Result<()> {
    if let Some((cursor_generation, cursor_session_id)) = cursor {
        validate_cursor_generation(Some(cursor_generation), generation)?;
        anyhow::ensure!(
            cursor_session_id == session_id,
            "Storyline cursor belongs to session '{cursor_session_id}', not '{session_id}'"
        );
    }
    Ok(())
}

fn ensure_unique_by<'a, T>(
    items: &'a [T],
    key: impl Fn(&'a T) -> &'a str,
    label: &str,
) -> Result<()> {
    for pair in items.windows(2) {
        anyhow::ensure!(key(&pair[0]) != key(&pair[1]), "duplicate {label}");
    }
    Ok(())
}

fn ensure_unique_values<T: PartialEq>(items: &[T], label: &str) -> Result<()> {
    for pair in items.windows(2) {
        anyhow::ensure!(pair[0] != pair[1], "duplicate {label}");
    }
    Ok(())
}

fn empty_page<T, C>() -> StorylinePage<T, C> {
    StorylinePage {
        generation: None,
        items: Vec::new(),
        next_cursor: None,
    }
}

fn decode_step_batches(batches: &[RecordBatch]) -> Result<Vec<StoryStepRow>> {
    Ok(batches
        .iter()
        .map(story_steps_from_batch)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

fn decode_tool_call_batches(batches: &[RecordBatch]) -> Result<Vec<StoryToolCallRow>> {
    Ok(batches
        .iter()
        .map(story_tool_calls_from_batch)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::super::storyline_content::CONTENT_REF_MAGIC;
    use super::*;
    use crate::{StorylineAgent, StorylineToolCall, StorylineTurn, STORYLINE_SCHEMA_VERSION};

    fn remote_uri(label: &str) -> String {
        format!(
            "shared-memory://pchronicle-storyline-{}-{label}-{}/root",
            std::process::id(),
            NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
        )
    }

    async fn put_remote_object(uri: &str, relative: &str, contents: &[u8]) {
        let (store, root) = ObjectStore::from_uri(uri).await.unwrap();
        store.put(&root.join(relative), contents).await.unwrap();
    }

    fn story(session_id: &str) -> StorylineDocument {
        StorylineDocument {
            schema_version: STORYLINE_SCHEMA_VERSION.into(),
            run_id: Some("run-1".into()),
            session_id: session_id.into(),
            agent: StorylineAgent {
                id: "agent-1".into(),
                name: Some("Agent".into()),
                version: Some("1".into()),
                model_name: Some("model".into()),
                tool_definitions: Some(serde_json::json!([{"name": "lookup"}])),
                extra: None,
            },
            parent: None,
            child_session_ids: None,
            notes: Some("test".into()),
            final_metrics: None,
            continued_trajectory_ref: None,
            extra: None,
            turns: vec![
                StorylineTurn {
                    id: 1,
                    kind: None,
                    timestamp: Some("2026-01-01T00:00:00Z".into()),
                    source: "user".into(),
                    message: serde_json::json!("price?"),
                    reasoning_content: None,
                    reasoning_effort: None,
                    tool_calls: None,
                    observation: None,
                    metrics: None,
                    model_name: None,
                    llm_call_count: None,
                    is_copied_context: None,
                    latency_ms: None,
                    ttft_ms: None,
                    extra: None,
                },
                StorylineTurn {
                    id: 2,
                    kind: Some("autonomous".into()),
                    timestamp: None,
                    source: "agent".into(),
                    message: serde_json::json!("checking"),
                    reasoning_content: Some("need tool".into()),
                    reasoning_effort: None,
                    tool_calls: Some(vec![StorylineToolCall {
                        tool_call_id: "call-1".into(),
                        function_name: "lookup".into(),
                        arguments: serde_json::json!({"symbol": "ACME"}),
                        duration_ms: Some(12),
                        extra: None,
                    }]),
                    observation: Some(serde_json::json!({
                        "results": [{"source_call_id": "call-1", "content": "42"}]
                    })),
                    metrics: None,
                    model_name: Some("model".into()),
                    llm_call_count: Some(1),
                    is_copied_context: Some(false),
                    latency_ms: Some(20),
                    ttft_ms: Some(5),
                    extra: None,
                },
            ],
        }
    }

    #[tokio::test]
    async fn persists_three_tables_and_round_trips_storyline() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(dir.path()).await.unwrap();
        let expected = story("session-1");
        store.replace_storyline(&expected).await.unwrap();

        let paths = store.current_table_paths().await.unwrap().unwrap();
        assert!(paths.runs.is_dir());
        assert!(paths.steps.is_dir());
        assert!(paths.tool_calls.is_dir());
        assert_eq!(
            store.get_storyline_full("session-1").await.unwrap(),
            Some(expected)
        );
    }

    #[tokio::test]
    async fn large_content_is_lossless_deduplicated_and_never_exposed_as_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let options = StorylineContentOptions {
            offload_threshold: 64,
            preview_bytes: 24,
            ..Default::default()
        };
        let store = StorylineLanceStore::open_with_content_options(dir.path(), options)
            .await
            .unwrap();
        let large = "shared large content ".repeat(128);
        let mut first = story("large-a");
        first.notes = Some(large.clone());
        let mut second = story("large-b");
        second.notes = Some(large.clone());
        store
            .replace_storylines(&[first.clone(), second.clone()])
            .await
            .unwrap();

        let paths = store.current_table_paths().await.unwrap().unwrap();
        let objects = open_objects(&paths.objects, paths.objects_version)
            .await
            .unwrap();
        assert_eq!(objects.count_rows(None).await.unwrap(), 1);

        let raw_runs = read_projected_batches(&paths.runs, paths.runs_version, &[], None)
            .await
            .unwrap();
        let raw_notes = raw_runs[0]
            .column_by_name("notes")
            .unwrap()
            .as_any()
            .downcast_ref::<lance::deps::arrow_array::StringArray>()
            .unwrap();
        assert!(raw_notes.value(0).starts_with(CONTENT_REF_MAGIC));
        assert_eq!(
            store.get_storyline_full("large-a").await.unwrap(),
            Some(first)
        );
        assert_eq!(
            store.get_storyline_full("large-b").await.unwrap(),
            Some(second)
        );

        let source = super::super::storyline_datafusion::StorylineDataSource::open(dir.path())
            .await
            .unwrap();
        let context = source.session_context().unwrap();
        let metadata = context
            .sql("SELECT session_id FROM runs ORDER BY session_id")
            .await
            .unwrap();
        let metadata_plan = metadata.clone().create_physical_plan().await.unwrap();
        let metadata_plan = datafusion::physical_plan::displayable(metadata_plan.as_ref())
            .indent(true)
            .to_string();
        assert!(
            !metadata_plan.contains("ContentHydrationExec"),
            "{metadata_plan}"
        );

        let escaped = large.replace('\'', "''");
        let filtered = context
            .sql(&format!(
                "SELECT notes FROM runs WHERE notes = '{escaped}' ORDER BY session_id"
            ))
            .await
            .unwrap();
        let filtered_plan = filtered.clone().create_physical_plan().await.unwrap();
        let filtered_plan = datafusion::physical_plan::displayable(filtered_plan.as_ref())
            .indent(true)
            .to_string();
        assert!(
            filtered_plan.contains("ContentHydrationExec"),
            "{filtered_plan}"
        );
        let batches = filtered.collect().await.unwrap();
        assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 2);
        for batch in batches {
            let notes = batch
                .column_by_name("notes")
                .unwrap()
                .as_any()
                .downcast_ref::<lance::deps::arrow_array::StringArray>()
                .unwrap();
            assert!(notes.iter().flatten().all(|value| value == large));
        }
        let count = context
            .sql(&format!(
                "SELECT COUNT(*) AS matches FROM runs WHERE notes = '{escaped}'"
            ))
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        let matches = count[0]
            .column_by_name("matches")
            .unwrap()
            .as_any()
            .downcast_ref::<lance::deps::arrow_array::Int64Array>()
            .unwrap();
        assert_eq!(matches.value(0), 2);

        let preview_source =
            super::super::storyline_datafusion::StorylineDataSource::open_with_options(
                dir.path(),
                super::super::storyline_datafusion::StorylineDataSourceOptions {
                    content_read_mode:
                        super::super::storyline_datafusion::StorylineContentReadMode::Preview,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let preview_context = preview_source.session_context().unwrap();
        let preview = preview_context
            .sql("SELECT notes FROM runs WHERE session_id = 'large-a'")
            .await
            .unwrap();
        let preview_plan = preview.clone().create_physical_plan().await.unwrap();
        let preview_plan = datafusion::physical_plan::displayable(preview_plan.as_ref())
            .indent(true)
            .to_string();
        assert!(preview_plan.contains("mode=preview"), "{preview_plan}");
        let preview = preview.collect().await.unwrap();
        let notes = preview[0]
            .column_by_name("notes")
            .unwrap()
            .as_any()
            .downcast_ref::<lance::deps::arrow_array::StringArray>()
            .unwrap();
        assert_eq!(notes.value(0), &large[..24]);
        let preview_filter_error = preview_context
            .sql(&format!(
                "SELECT session_id FROM runs WHERE notes = '{escaped}'"
            ))
            .await
            .unwrap()
            .collect()
            .await
            .unwrap_err();
        assert!(
            preview_filter_error
                .to_string()
                .contains("content predicates require full"),
            "{preview_filter_error}"
        );
    }

    #[tokio::test]
    async fn maintenance_prunes_objects_unreachable_from_current_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let options = StorylineContentOptions {
            offload_threshold: 32,
            ..Default::default()
        };
        let store = StorylineLanceStore::open_with_content_options(dir.path(), options)
            .await
            .unwrap();
        let mut document = story("gc");
        document.notes = Some("old unreachable content ".repeat(64));
        store.replace_storyline(&document).await.unwrap();
        document.notes = Some("new live content ".repeat(64));
        store.replace_storyline(&document).await.unwrap();

        let before = store.current_table_paths().await.unwrap().unwrap();
        let before_objects = open_objects(&before.objects, before.objects_version)
            .await
            .unwrap()
            .count_rows(None)
            .await
            .unwrap();
        let report = store
            .maintain(&LanceMaintenanceOptions {
                vacuum_older_than: None,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(report.objects_removed, 1);
        let after = store.current_table_paths().await.unwrap().unwrap();
        let after_objects = open_objects(&after.objects, after.objects_version)
            .await
            .unwrap()
            .count_rows(None)
            .await
            .unwrap();
        assert_eq!(after_objects + 1, before_objects);
        assert_eq!(
            store.get_storyline_full("gc").await.unwrap(),
            Some(document)
        );
    }

    #[tokio::test]
    async fn content_descriptor_magic_in_user_text_round_trips_as_literal() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open_with_content_options(
            dir.path(),
            StorylineContentOptions {
                offload_threshold: usize::MAX,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let literal = format!("{CONTENT_REF_MAGIC}user-controlled-not-a-descriptor");
        let mut expected = story("magic");
        expected.notes = Some(literal);
        store.replace_storyline(&expected).await.unwrap();
        assert_eq!(
            store.get_storyline_full("magic").await.unwrap(),
            Some(expected)
        );
    }

    #[tokio::test]
    async fn empty_storyline_still_creates_queryable_tables() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(dir.path()).await.unwrap();
        let mut expected = story("empty");
        expected.turns.clear();
        store.replace_storyline(&expected).await.unwrap();

        let paths = store.current_table_paths().await.unwrap().unwrap();
        assert_eq!(
            Dataset::open(paths.steps.to_string_lossy().as_ref())
                .await
                .unwrap()
                .count_rows(None)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            Dataset::open(paths.tool_calls.to_string_lossy().as_ref())
                .await
                .unwrap()
                .count_rows(None)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            store.get_storyline_full("empty").await.unwrap(),
            Some(expected)
        );
    }

    #[tokio::test]
    async fn replacement_is_session_scoped_and_switches_generation() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(dir.path()).await.unwrap();
        store.replace_storyline(&story("a")).await.unwrap();
        let first = store.current_table_paths().await.unwrap().unwrap();
        store.replace_storyline(&story("b")).await.unwrap();
        let second = store.current_table_paths().await.unwrap().unwrap();
        assert_ne!(first.generation, second.generation);
        assert_eq!(first.table_generation, second.table_generation);
        assert_eq!(first.runs, second.runs);
        assert_eq!(first.steps, second.steps);
        assert_eq!(first.tool_calls, second.tool_calls);
        assert!(second.runs_version > first.runs_version);
        assert!(second.steps_version > first.steps_version);
        assert!(second.tool_calls_version > first.tool_calls_version);
        assert!(store.get_storyline_full("a").await.unwrap().is_some());
        assert!(store.get_storyline_full("b").await.unwrap().is_some());

        let mut updated = story("a");
        updated.notes = Some("updated".into());
        updated.turns.truncate(1);
        store.replace_storyline(&updated).await.unwrap();
        assert_eq!(
            store
                .list_run_summaries(Default::default())
                .await
                .unwrap()
                .items
                .len(),
            2
        );
        assert_eq!(
            store
                .list_steps_page("a", Default::default())
                .await
                .unwrap()
                .items
                .len(),
            1
        );
        assert!(store
            .list_tool_calls_page("a", Default::default())
            .await
            .unwrap()
            .items
            .is_empty());
        assert_eq!(store.get_storyline_full("a").await.unwrap(), Some(updated));
    }

    #[tokio::test]
    async fn run_summary_pages_are_projected_and_generation_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let options = StorylineContentOptions {
            offload_threshold: 8,
            ..Default::default()
        };
        let store = StorylineLanceStore::open_with_content_options(dir.path(), options)
            .await
            .unwrap();
        let mut alpha = story("alpha");
        alpha.notes = Some("offloaded run notes".repeat(32));
        store
            .replace_storylines(&[story("charlie"), alpha, story("bravo")])
            .await
            .unwrap();
        let paths = store.resolve_current_table_paths().await.unwrap().unwrap();
        tokio::fs::remove_dir_all(&paths.steps).await.unwrap();
        tokio::fs::remove_dir_all(&paths.tool_calls).await.unwrap();
        tokio::fs::remove_dir_all(&paths.objects).await.unwrap();

        let first = store
            .list_run_summaries(StorylinePageRequest {
                limit: 2,
                after: None,
            })
            .await
            .unwrap();
        assert_eq!(
            first
                .items
                .iter()
                .map(|run| run.session_id.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "bravo"]
        );
        let second = store
            .list_run_summaries(StorylinePageRequest {
                limit: 2,
                after: first.next_cursor,
            })
            .await
            .unwrap();
        assert_eq!(second.items[0].session_id, "charlie");
        assert!(second.next_cursor.is_none());
    }

    #[tokio::test]
    async fn detail_pages_hydrate_only_selected_rows_and_reject_stale_cursors() {
        let dir = tempfile::tempdir().unwrap();
        let options = StorylineContentOptions {
            offload_threshold: 8,
            ..Default::default()
        };
        let store = StorylineLanceStore::open_with_content_options(dir.path(), options)
            .await
            .unwrap();
        let mut document = story("paged");
        for id in 3..=5 {
            let mut turn = document.turns[1].clone();
            turn.id = id;
            turn.message = serde_json::json!(format!("large message {id} {}", "x".repeat(64)));
            let call = turn.tool_calls.as_mut().unwrap().first_mut().unwrap();
            call.tool_call_id = format!("call-{id}");
            turn.observation = Some(serde_json::json!({
                "results": [{"source_call_id": format!("call-{id}"), "content": "result"}]
            }));
            document.turns.push(turn);
        }
        store.replace_storyline(&document).await.unwrap();

        let first_steps = store
            .list_steps_page(
                "paged",
                StorylinePageRequest {
                    limit: 2,
                    after: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            first_steps
                .items
                .iter()
                .map(|step| step.step_id)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        let second_steps = store
            .list_steps_page(
                "paged",
                StorylinePageRequest {
                    limit: 2,
                    after: first_steps.next_cursor.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            second_steps
                .items
                .iter()
                .map(|step| step.step_id)
                .collect::<Vec<_>>(),
            [3, 4]
        );

        let first_calls = store
            .list_tool_calls_page(
                "paged",
                StorylinePageRequest {
                    limit: 2,
                    after: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            first_calls
                .items
                .iter()
                .map(|call| call.step_id)
                .collect::<Vec<_>>(),
            [2, 3]
        );

        store.replace_storyline(&story("other")).await.unwrap();
        let stale = store
            .list_steps_page(
                "paged",
                StorylinePageRequest {
                    limit: 2,
                    after: first_steps.next_cursor,
                },
            )
            .await
            .unwrap_err();
        assert!(stale.to_string().contains("changed while paging"));
    }

    #[tokio::test]
    async fn batch_replace_commits_once_and_rejects_duplicate_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(dir.path()).await.unwrap();
        let stories = vec![story("a"), story("b")];
        store.replace_storylines(&stories).await.unwrap();
        let committed = store
            .current_table_paths()
            .await
            .unwrap()
            .unwrap()
            .generation;
        assert_eq!(
            store
                .list_run_summaries(Default::default())
                .await
                .unwrap()
                .items
                .len(),
            2
        );

        let duplicate = vec![story("same"), story("same")];
        assert!(store.replace_storylines(&duplicate).await.is_err());
        assert_eq!(
            store
                .current_table_paths()
                .await
                .unwrap()
                .unwrap()
                .generation,
            committed
        );
    }

    #[tokio::test]
    async fn batch_get_preserves_request_order_and_missing_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(dir.path()).await.unwrap();
        let first = story("a");
        let second = story("b");
        store
            .replace_storylines(&[first.clone(), second.clone()])
            .await
            .unwrap();

        let actual = store
            .get_storylines_full(&["b".into(), "missing".into(), "a".into()])
            .await
            .unwrap();
        assert_eq!(actual, [Some(second), None, Some(first)]);
        assert!(store
            .get_storylines_full(&["a".into(), "a".into()])
            .await
            .unwrap_err()
            .to_string()
            .contains("duplicate session_id"));
    }

    #[tokio::test]
    async fn streamed_replace_is_bounded_and_commits_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(dir.path()).await.unwrap();
        let stories = (0..300).map(|index| Ok(story(&format!("stream-{index:03}"))));
        let report = store.replace_storyline_stream(stories).await.unwrap();
        assert_eq!(report.storylines, 300);
        assert_eq!(report.steps, 600);
        assert_eq!(report.tool_calls, 300);
        assert!(!report.generation.is_empty());
        assert_eq!(
            store
                .list_run_summaries(StorylinePageRequest {
                    limit: 300,
                    after: None,
                })
                .await
                .unwrap()
                .items
                .len(),
            300
        );
        assert_eq!(
            store
                .current_table_paths()
                .await
                .unwrap()
                .unwrap()
                .generation,
            report.generation
        );
    }

    #[tokio::test]
    async fn atif_stream_create_writes_one_fragment_per_table() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(dir.path()).await.unwrap();
        let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif");
        let report = store.import_atif_stream(input).await.unwrap();
        assert_eq!(report.storylines, 8);
        assert_eq!(report.steps, 118);
        assert_eq!(report.tool_calls, 23);
        let paths = store.current_table_paths().await.unwrap().unwrap();
        for path in [&paths.runs, &paths.steps, &paths.tool_calls] {
            assert_eq!(
                Dataset::open(path.to_string_lossy().as_ref())
                    .await
                    .unwrap()
                    .get_fragments()
                    .len(),
                1
            );
        }

        let empty_tools = tempfile::tempdir().unwrap();
        let empty_tools_store = StorylineLanceStore::open(empty_tools.path()).await.unwrap();
        let dialogue =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif/dialogue_10.json");
        let report = empty_tools_store
            .import_atif_stream(dialogue)
            .await
            .unwrap();
        assert_eq!(report.tool_calls, 0);
        assert!(empty_tools_store
            .list_tool_calls_page("fixture-dialogue_10", Default::default())
            .await
            .unwrap()
            .items
            .is_empty());
    }

    #[tokio::test]
    async fn streamed_replace_error_after_first_chunk_keeps_current() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(dir.path()).await.unwrap();
        store.replace_storyline(&story("committed")).await.unwrap();
        let before = store.current_table_paths().await.unwrap().unwrap();
        let stories = (0..257)
            .map(|index| Ok(story(&format!("pending-{index:03}"))))
            .chain(std::iter::once(Err(anyhow::anyhow!("broken stream"))));
        let error = store.replace_storyline_stream(stories).await.unwrap_err();
        assert!(error.to_string().contains("broken stream"));
        let after = store.current_table_paths().await.unwrap().unwrap();
        assert_eq!(before.generation, after.generation);
        assert_eq!(
            store
                .list_run_summaries(Default::default())
                .await
                .unwrap()
                .items
                .len(),
            1
        );
        assert!(store
            .get_storyline_full("committed")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn invalid_result_does_not_move_current_generation() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(dir.path()).await.unwrap();
        store.replace_storyline(&story("a")).await.unwrap();
        let before = store
            .current_table_paths()
            .await
            .unwrap()
            .unwrap()
            .generation;
        let mut invalid = story("a");
        invalid.turns[1].observation = Some(serde_json::json!({
            "results": [{"source_call_id": "missing", "content": "x"}]
        }));
        assert!(store.replace_storyline(&invalid).await.is_err());
        let after = store
            .current_table_paths()
            .await
            .unwrap()
            .unwrap()
            .generation;
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn replace_defers_compaction_until_explicit_maintenance() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(dir.path()).await.unwrap();
        let expected = story("a");
        store
            .replace_storylines(&[expected.clone(), story("b")])
            .await
            .unwrap();
        let before = store.current_table_paths().await.unwrap().unwrap();
        for session in 0..36 {
            store
                .replace_storyline(&story(&format!("fragment-{session}")))
                .await
                .unwrap();
        }
        let fragmented = store.resolve_current_table_paths().await.unwrap().unwrap();
        assert!(
            open_table_version(&fragmented.runs, fragmented.runs_version)
                .await
                .unwrap()
                .get_fragments()
                .len()
                > 32
        );
        let report = store
            .maintain(&LanceMaintenanceOptions {
                vacuum_older_than: None,
                ..Default::default()
            })
            .await
            .unwrap();
        let after = store.current_table_paths().await.unwrap().unwrap();
        assert_ne!(before.generation, after.generation);
        assert_eq!(
            report.generation.as_deref(),
            Some(after.generation.as_str())
        );
        assert!(report.runs.fragments_removed > 0);
        assert_eq!(store.get_storyline_full("a").await.unwrap(), Some(expected));
    }

    #[tokio::test]
    async fn empty_store_is_queryable_and_empty_batch_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(dir.path()).await.unwrap();
        assert!(store.current_table_paths().await.unwrap().is_none());
        assert!(store
            .list_run_summaries(Default::default())
            .await
            .unwrap()
            .items
            .is_empty());
        assert!(store
            .list_steps_page("missing", Default::default())
            .await
            .unwrap()
            .items
            .is_empty());
        assert!(store
            .list_tool_calls_page("missing", Default::default())
            .await
            .unwrap()
            .items
            .is_empty());
        assert!(store.get_storyline_full("missing").await.unwrap().is_none());

        store.replace_storylines(&[]).await.unwrap();
        assert!(store.current_table_paths().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn open_rejects_malformed_or_incomplete_commit_pointer() {
        let invalid = tempfile::tempdir().unwrap();
        tokio::fs::write(invalid.path().join(CURRENT_FILE), "../outside\n")
            .await
            .unwrap();
        let error = StorylineLanceStore::open(invalid.path()).await.unwrap_err();
        assert!(error.to_string().contains("invalid Storyline generation"));

        let incomplete = tempfile::tempdir().unwrap();
        tokio::fs::write(incomplete.path().join(CURRENT_FILE), "gen-missing\n")
            .await
            .unwrap();
        let error = StorylineLanceStore::open(incomplete.path())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("is incomplete"));
    }

    #[tokio::test]
    async fn object_store_uri_round_trips_across_store_instances() {
        let uri = format!("{}/", remote_uri("round-trip"));
        let store = StorylineLanceStore::open_uri(&uri).await.unwrap();
        assert_eq!(store.storage_scheme(), "shared-memory");
        assert!(!store.root_uri().ends_with('/'));
        store.replace_storyline(&story("remote-1")).await.unwrap();

        let reopened = StorylineLanceStore::open_uri(store.root_uri())
            .await
            .unwrap();
        assert_eq!(
            reopened
                .get_storyline_full("remote-1")
                .await
                .unwrap()
                .unwrap()
                .session_id,
            "remote-1"
        );
        let paths = reopened.current_table_paths().await.unwrap().unwrap();
        assert!(paths
            .runs
            .to_string_lossy()
            .starts_with("shared-memory://pchronicle-storyline-"));
    }

    #[tokio::test]
    async fn object_store_rejects_invalid_utf8_unsafe_and_dangling_current() {
        let cases: [(&str, &[u8], &str); 3] = [
            ("utf8", &[0xff], "not valid UTF-8"),
            ("unsafe", b"../outside\n", "invalid Storyline generation"),
            ("dangling", b"gen-missing\n", "is incomplete"),
        ];
        for (label, contents, expected) in cases {
            let uri = remote_uri(label);
            put_remote_object(&uri, CURRENT_FILE, contents).await;
            let error = StorylineLanceStore::open_uri(&uri).await.unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "unexpected error for {label}: {error:#}"
            );
        }
    }

    #[tokio::test]
    async fn object_store_detects_partially_deleted_generation() {
        let uri = remote_uri("partial-generation");
        let store = StorylineLanceStore::open_uri(&uri).await.unwrap();
        store.replace_storyline(&story("session")).await.unwrap();
        let paths = store.current_table_paths().await.unwrap().unwrap();
        let steps_uri = paths.steps.to_string_lossy().into_owned();
        let (object_store, steps_root) = ObjectStore::from_uri(&steps_uri).await.unwrap();
        object_store.remove_dir_all(steps_root).await.unwrap();

        let error = StorylineLanceStore::open_uri(&uri).await.unwrap_err();
        assert!(error.to_string().contains("is incomplete"), "{error:#}");
    }

    #[tokio::test]
    async fn object_store_prefixes_are_isolated() {
        let left_uri = remote_uri("isolation-left");
        let right_uri = remote_uri("isolation-right");
        let left = StorylineLanceStore::open_uri(&left_uri).await.unwrap();
        let right = StorylineLanceStore::open_uri(&right_uri).await.unwrap();

        left.replace_storyline(&story("left")).await.unwrap();
        assert!(left.get_storyline_full("left").await.unwrap().is_some());
        assert!(right.current_table_paths().await.unwrap().is_none());
        assert!(right
            .list_run_summaries(Default::default())
            .await
            .unwrap()
            .items
            .is_empty());
    }

    #[tokio::test]
    async fn concurrent_object_store_replacements_do_not_lose_sessions() {
        let uri = remote_uri("concurrent");
        let stores = futures::future::join_all((0..6).map(|_| StorylineLanceStore::open_uri(&uri)))
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let writes = stores
            .into_iter()
            .enumerate()
            .map(|(index, store)| async move {
                store
                    .replace_storyline(&story(&format!("session-{index}")))
                    .await
            });
        for result in futures::future::join_all(writes).await {
            result.unwrap();
        }

        let reopened = StorylineLanceStore::open_uri(&uri).await.unwrap();
        let mut sessions = reopened
            .list_run_summaries(Default::default())
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|run| run.session_id)
            .collect::<Vec<_>>();
        sessions.sort();
        assert_eq!(
            sessions,
            (0..6)
                .map(|index| format!("session-{index}"))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn stale_current_commit_is_rejected_without_moving_snapshot() {
        let uri = remote_uri("stale-current");
        let store = StorylineLanceStore::open_uri(&uri).await.unwrap();
        store.replace_storyline(&story("first")).await.unwrap();
        let stale = store.current_table_paths().await.unwrap().unwrap();

        store.replace_storyline(&story("second")).await.unwrap();
        let committed = store.current_table_paths().await.unwrap().unwrap();
        let attempted_generation = next_generation();
        let error = store
            .commit_snapshot(
                &StorylineSnapshotPointer {
                    generation: attempted_generation,
                    parent_generation: Some(stale.generation.clone()),
                    table_generation: stale.table_generation.clone(),
                    runs_version: stale.runs_version,
                    steps_version: stale.steps_version,
                    tool_calls_version: stale.tool_calls_version,
                    objects_version: stale.objects_version,
                },
                Some(&stale.generation),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("commit conflict"), "{error:#}");

        let after = store.current_table_paths().await.unwrap().unwrap();
        assert_eq!(after.generation, committed.generation);
        assert!(store.get_storyline_full("second").await.unwrap().is_some());
    }

    #[test]
    fn joins_object_store_locations_without_losing_uri_scheme() {
        assert_eq!(
            normalize_root_uri("s3://bucket/trajectory-root///").unwrap(),
            "s3://bucket/trajectory-root"
        );
        assert_eq!(
            join_location(
                "s3://bucket/trajectory-root",
                &["generations", "gen-1", "runs.lance"]
            ),
            "s3://bucket/trajectory-root/generations/gen-1/runs.lance"
        );
        assert_eq!(normalize_root_uri("/").unwrap(), "/");
        assert_eq!(normalize_root_uri("s3://bucket///").unwrap(), "s3://bucket");
        assert_eq!(
            join_location("s3://bucket/轨迹", &["generations", "/gen-1/"]),
            "s3://bucket/轨迹/generations/gen-1"
        );
        assert!(normalize_root_uri("  ").is_err());
    }
}

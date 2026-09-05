//! Storyline-native normalized Lance store.
//!
//! `CURRENT` pins one exact MVCC version from each normalized Lance dataset and
//! the shared content-addressed object dataset.
//! A replacement merge-upserts rows for only the requested sessions, removes
//! obsolete keys in that session set, then moves `CURRENT` after all table
//! versions are durable. Readers never observe a partially updated Storyline.
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

mod content;
pub(super) mod datafusion;
mod mutation;
pub(super) mod rows;
mod writer_control;

use mutation::{
    ExternalizedStorylineBatches, StorylineChunkState, externalize_rows,
    next_storyline_stream_chunk, replace_table_batches, write_batches,
};

pub use content::{
    DEFAULT_CONTENT_OFFLOAD_THRESHOLD, DEFAULT_CONTENT_PREVIEW_BYTES, StorylineContentOptions,
};
pub use datafusion::{
    DATAFUSION_RUNS_TABLE, DATAFUSION_STEPS_TABLE, DATAFUSION_TOOL_CALLS_TABLE,
    StorylineContentReadMode, StorylineDataFusionTableNames, StorylineDataSource,
    StorylineDataSourceOptions, StorylineTableKind,
};
pub use rows::{
    story_runs_arrow_schema, story_runs_from_batch, story_runs_to_batch, story_steps_arrow_schema,
    story_steps_from_batch, story_steps_to_batch, story_tool_calls_arrow_schema,
    story_tool_calls_from_batch, story_tool_calls_to_batch,
};

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use fs2::FileExt;
use futures::TryStreamExt;
use lance::Dataset;
use lance::dataset::optimize::{CompactionOptions, compact_files};
use lance::dataset::{
    InsertBuilder, MergeInsertBuilder, WhenMatched, WhenNotMatched, WhenNotMatchedBySource,
    WriteMode, WriteParams,
};
use lance::deps::arrow_array::{
    Array, Int64Array, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray,
};
use lance::deps::arrow_schema::{ArrowError, SchemaRef};
use lance::index::DatasetIndexExt;
use lance_index::IndexType;
use lance_index::optimize::OptimizeOptions;
use lance_index::scalar::{BuiltinIndexType, ScalarIndexParams};
use serde::{Deserialize, Serialize};

use super::opendal_store::Store as OpendalStore;
use super::storyline_model::{
    STORY_RUNS_TABLE, STORY_STEPS_TABLE, STORY_TOOL_CALLS_TABLE, StoryRunRow, StoryStepRow,
    StoryToolCallRow, StorylineTables, reconstruct_storyline, split_storyline_with_unknown_limits,
};
use crate::StorylineDocument;
use crate::formats::unknown_fields::{compute_unknown_key_counts, validate_unknown_fields};

use self::content::{
    PendingContent, STORYLINE_OBJECTS_DATASET, collect_content_ids, commit_pending_content,
    content_columns, externalize_batches, externalize_unknown_field_values, hydrate_batches,
    open_objects, prune_unreferenced_objects,
};
use super::AtifReader;
use super::{LanceMaintenanceOptions, LanceMaintenanceReport, root_write_lock};

const CURRENT_FILE: &str = "CURRENT";
const GENERATIONS_DIR: &str = "generations";
const STORYLINE_LANCE_SCHEMA_VERSION: u32 = 1;
const WRITE_BATCH_ROWS: usize = 8192;
const STREAM_IMPORT_STORIES: usize = 256;
const RUN_INDEXES: [(&str, IndexType); 3] = [
    ("document_id", IndexType::BTree),
    ("session_id", IndexType::BTree),
    ("run_id", IndexType::BTree),
];
// `step_id` restarts inside every Storyline and has low global selectivity.
// `session_id` first narrows a lookup to one short Storyline, after which a
// step range is cheaper to filter than maintaining another BTree.
const STEP_INDEXES: [(&str, IndexType); 5] = [
    ("document_id", IndexType::BTree),
    ("session_id", IndexType::BTree),
    ("timestamp", IndexType::BTree),
    ("effective_kind", IndexType::Bitmap),
    ("source", IndexType::Bitmap),
];
const TOOL_CALL_INDEXES: [(&str, IndexType); 4] = [
    ("document_id", IndexType::BTree),
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
    /// Verified derivation metadata. `None` marks a directly-written store.
    pub projection: Option<StorylineProjectionLineage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectionSourceSnapshot {
    CanonicalEvents {
        source_uri: String,
        fact_version: u64,
        fact_rows: u64,
        layout_revision: u64,
    },
    Exchange {
        source_uri: String,
        snapshot_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_digest: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorylineProjectionLineage {
    pub source_id: String,
    pub source_file: String,
    pub source: ProjectionSourceSnapshot,
    pub projector_name: String,
    pub recipe_hash: String,
    pub completeness: String,
}

impl StorylineProjectionLineage {
    fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("source_id", self.source_id.as_str()),
            ("source_file", self.source_file.as_str()),
            ("projector_name", self.projector_name.as_str()),
            ("recipe_hash", self.recipe_hash.as_str()),
            ("completeness", self.completeness.as_str()),
        ] {
            anyhow::ensure!(
                !value.trim().is_empty(),
                "projection {name} must not be empty"
            );
        }
        match &self.source {
            ProjectionSourceSnapshot::CanonicalEvents { source_uri, .. }
            | ProjectionSourceSnapshot::Exchange { source_uri, .. } => {
                anyhow::ensure!(
                    !source_uri.trim().is_empty(),
                    "projection source_uri must not be empty"
                );
            }
        }
        if let ProjectionSourceSnapshot::Exchange { snapshot_ref, .. } = &self.source {
            anyhow::ensure!(
                !snapshot_ref.trim().is_empty(),
                "projection exchange snapshot_ref must not be empty"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StorylineSnapshotPointer {
    schema_version: u32,
    generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_generation: Option<String>,
    table_generation: String,
    runs_version: u64,
    steps_version: u64,
    tool_calls_version: u64,
    objects_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    projection: Option<StorylineProjectionLineage>,
}

#[derive(Debug, Clone)]
pub struct StorylineLanceStore {
    root: PathBuf,
    root_uri: String,
    control_store: OpendalStore,
    write_lock: Arc<tokio::sync::Mutex<()>>,
    control_lock: Arc<tokio::sync::Mutex<()>>,
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorylineMaintenanceReport {
    pub generation: Option<String>,
    pub runs: LanceMaintenanceReport,
    pub steps: LanceMaintenanceReport,
    pub tool_calls: LanceMaintenanceReport,
    pub objects: LanceMaintenanceReport,
    pub objects_removed: usize,
    pub generations_removed: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorylineStreamImportReport {
    pub generation: String,
    pub storylines: usize,
    pub steps: usize,
    pub tool_calls: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StorylineProjectionPublicationOutcome {
    Published(StorylineStreamImportReport),
    OutputNotEmpty,
}

fn published_storyline_report(
    outcome: StorylineProjectionPublicationOutcome,
) -> Result<StorylineStreamImportReport> {
    match outcome {
        StorylineProjectionPublicationOutcome::Published(report) => Ok(report),
        StorylineProjectionPublicationOutcome::OutputNotEmpty => {
            anyhow::bail!("non-create Storyline publication reported nonempty output")
        }
    }
}

fn attach_stream_cleanup_failures(
    result: Result<StorylineProjectionPublicationOutcome>,
    cleanup_failures: Vec<String>,
) -> Result<StorylineProjectionPublicationOutcome> {
    if cleanup_failures.is_empty() {
        return result;
    }
    let cleanup = format!(
        "Storyline cleanup also failed: {}",
        cleanup_failures.join("; ")
    );
    match result {
        Ok(_) => Err(anyhow::anyhow!(cleanup)),
        Err(error) => Err(error.context(cleanup)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorylineStreamWriteMode {
    Replace,
    Rebuild,
    CreateProjection,
}

#[cfg(test)]
#[derive(Clone)]
struct CreateAfterEmptyReadBarrierHook {
    root_uri: String,
    barrier: Arc<tokio::sync::Barrier>,
    content_arrivals: Arc<std::sync::atomic::AtomicUsize>,
    content_created: Arc<tokio::sync::Notify>,
}

#[cfg(test)]
static CREATE_AFTER_EMPTY_READ_BARRIER: std::sync::Mutex<Option<CreateAfterEmptyReadBarrierHook>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
#[derive(Clone)]
struct ReplacementAfterCurrentReadBarrierHook {
    root_uri: String,
    barrier: Arc<tokio::sync::Barrier>,
}

#[cfg(test)]
static REPLACEMENT_AFTER_CURRENT_READ_BARRIER: std::sync::Mutex<
    Option<ReplacementAfterCurrentReadBarrierHook>,
> = std::sync::Mutex::new(None);

#[cfg(test)]
#[derive(Clone)]
struct MaintenanceAfterPublishPauseHook {
    root_uri: String,
    reached: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
}

#[cfg(test)]
static MAINTENANCE_AFTER_PUBLISH_PAUSE: std::sync::Mutex<Option<MaintenanceAfterPublishPauseHook>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn suppress_inverted_index_roots() -> std::sync::MutexGuard<'static, HashSet<String>> {
    static ROOTS: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> =
        std::sync::OnceLock::new();
    ROOTS
        .get_or_init(|| std::sync::Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
fn dataset_is_under_root(dataset_uri: &str, root: &str) -> bool {
    let dataset = dataset_uri
        .strip_prefix("file://")
        .unwrap_or(dataset_uri)
        .trim_end_matches('/');
    let root = root
        .strip_prefix("file://")
        .unwrap_or(root)
        .trim_end_matches('/');
    dataset == root || dataset.starts_with(&format!("{root}/"))
}

#[cfg(test)]
fn inverted_indexes_suppressed_for(dataset: &Dataset) -> bool {
    suppress_inverted_index_roots()
        .iter()
        .any(|root| dataset_is_under_root(dataset.uri(), root))
}

#[cfg(test)]
fn install_inverted_index_suppression(root_uri: &str) {
    suppress_inverted_index_roots().insert(root_uri.to_string());
}

#[cfg(test)]
fn remove_inverted_index_suppression(root_uri: &str) {
    suppress_inverted_index_roots().remove(root_uri);
}

#[cfg(test)]
async fn wait_after_empty_current_read(root_uri: &str) {
    let barrier = CREATE_AFTER_EMPTY_READ_BARRIER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|hook| hook.root_uri == root_uri)
        .map(|hook| hook.barrier.clone());
    if let Some(barrier) = barrier {
        barrier.wait().await;
    }
}

#[cfg(test)]
async fn wait_after_replacement_current_read(root_uri: &str) {
    let barrier = REPLACEMENT_AFTER_CURRENT_READ_BARRIER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|hook| hook.root_uri == root_uri)
        .map(|hook| hook.barrier.clone());
    if let Some(barrier) = barrier {
        barrier.wait().await;
    }
}

#[cfg(test)]
async fn wait_after_maintenance_publish(root_uri: &str) {
    let hook = MAINTENANCE_AFTER_PUBLISH_PAUSE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|hook| hook.root_uri == root_uri)
        .cloned();
    if let Some(hook) = hook {
        hook.reached.notify_one();
        hook.resume.notified().await;
    }
}

#[cfg(test)]
async fn wait_for_first_content_create(root_uri: &str) -> bool {
    let hook = CREATE_AFTER_EMPTY_READ_BARRIER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|hook| hook.root_uri == root_uri)
        .cloned();
    let Some(hook) = hook else {
        return false;
    };
    if hook
        .content_arrivals
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        == 0
    {
        true
    } else {
        hook.content_created.notified().await;
        false
    }
}

#[cfg(test)]
fn release_waiting_content_create(root_uri: &str, first: bool) {
    if !first {
        return;
    }
    let notify = CREATE_AFTER_EMPTY_READ_BARRIER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|hook| hook.root_uri == root_uri)
        .map(|hook| hook.content_created.clone());
    if let Some(notify) = notify {
        notify.notify_one();
    }
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

    /// Return whether any state already exists at a potential Storyline root.
    /// This is observational: it never creates the local directory, a lock
    /// file, or an object-store key.
    pub async fn destination_exists(root: impl AsRef<str>) -> Result<bool> {
        if !root.as_ref().contains("://") {
            return Ok(Path::new(root.as_ref()).exists());
        }
        let store = Self::open_uri_unchecked(root).await?;
        if !store.root_uri.contains("://")
            || matches!(store.storage_scheme(), "file" | "file+uring")
        {
            return Ok(store.root.exists());
        }
        store
            .control_store
            .exists()
            .await
            .context("inspect Storyline destination prefix")
    }

    pub(crate) async fn open_uri_unchecked(root: impl AsRef<str>) -> Result<Self> {
        let root_uri = normalize_root_uri(root.as_ref())?;
        let control_store = OpendalStore::from_uri(&root_uri).await?;
        Ok(Self {
            root: PathBuf::from(&root_uri),
            write_lock: root_write_lock::for_root(&root_uri),
            control_lock: Arc::new(tokio::sync::Mutex::new(())),
            root_uri,
            control_store,
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
        self.root_uri
            .split_once("://")
            .map(|(scheme, _)| scheme)
            .unwrap_or("file")
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

    /// Return the generation and every stable per-document identity from one
    /// committed snapshot.
    pub async fn document_ids_snapshot(&self) -> Result<Option<(String, Vec<String>)>> {
        let Some(paths) = self.current_table_paths().await? else {
            return Ok(None);
        };
        let batches =
            read_projected_batches(&paths.runs, paths.runs_version, &["document_id"], None).await?;
        let mut ids = Vec::new();
        for batch in batches {
            let document_ids = batch
                .column_by_name("document_id")
                .and_then(|array| array.as_any().downcast_ref::<StringArray>())
                .context("Storyline runs document_id column is missing or invalid")?;
            ids.extend(document_ids.iter().flatten().map(str::to_string));
        }
        ids.sort_unstable();
        anyhow::ensure!(
            ids.windows(2).all(|pair| pair[0] != pair[1]),
            "duplicate document_id in committed Storyline snapshot"
        );
        Ok(Some((paths.generation, ids)))
    }

    pub(crate) async fn resolve_current_table_paths(&self) -> Result<Option<StorylineTablePaths>> {
        let current = self.read_current_control().await?;
        let Some(pointer) = current.control.committed else {
            return Ok(None);
        };
        let mut paths = self.paths_for_generation(&pointer.table_generation);
        paths.generation = pointer.generation;
        paths.runs_version = pointer.runs_version;
        paths.steps_version = pointer.steps_version;
        paths.tool_calls_version = pointer.tool_calls_version;
        paths.objects_version = pointer.objects_version;
        paths.projection = pointer.projection;
        Ok(Some(paths))
    }

    pub async fn replace_storyline(&self, story: &StorylineDocument) -> Result<()> {
        self.replace_storylines(std::slice::from_ref(story)).await
    }

    /// Publish complete Storyline replacements with verifiable source lineage.
    pub async fn replace_projected_storylines(
        &self,
        stories: &[StorylineDocument],
        projection: StorylineProjectionLineage,
    ) -> Result<()> {
        if stories.is_empty() {
            return Ok(());
        }
        projection.validate()?;
        let outcome = self
            .replace_storyline_stream_with_projection(
                stories.iter().cloned().map(Ok::<_, anyhow::Error>),
                Some(projection),
                StorylineStreamWriteMode::Replace,
                None,
            )
            .await?;
        published_storyline_report(outcome)?;
        Ok(())
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
        let stories = AtifReader::open(input.as_ref())?;
        self.replace_storyline_stream(stories).await
    }

    /// 以流式方式原子替换 Storyline，不在内存中保留完整导入集。
    ///
    /// 每次只物化一个固定大小的有界文档批次及其规范化行。消费流期间 Lance version
    /// 可以前进，但只有全部批次和最终索引维护成功后才移动 `CURRENT`；失败时读者仍看到
    /// 上一个完整快照。
    pub async fn replace_storyline_stream<I>(
        &self,
        stories: I,
    ) -> Result<StorylineStreamImportReport>
    where
        I: IntoIterator<Item = Result<StorylineDocument>>,
    {
        let outcome = self
            .replace_storyline_stream_with_projection(
                stories,
                None,
                StorylineStreamWriteMode::Replace,
                None,
            )
            .await?;
        published_storyline_report(outcome)
    }

    /// Append pre-disambiguated Storylines only if the Dataset is still at
    /// the snapshot used to resolve duplicate IDs.
    pub async fn append_storyline_stream<I>(
        &self,
        stories: I,
        expected_generation: &str,
    ) -> Result<StorylineStreamImportReport>
    where
        I: IntoIterator<Item = Result<StorylineDocument>>,
    {
        let outcome = self
            .replace_storyline_stream_with_projection(
                stories,
                None,
                StorylineStreamWriteMode::Replace,
                Some(expected_generation),
            )
            .await?;
        published_storyline_report(outcome)
    }

    pub async fn replace_projected_storyline_stream<I>(
        &self,
        stories: I,
        projection: StorylineProjectionLineage,
    ) -> Result<StorylineStreamImportReport>
    where
        I: IntoIterator<Item = Result<StorylineDocument>>,
    {
        projection.validate()?;
        let outcome = self
            .replace_storyline_stream_with_projection(
                stories,
                Some(projection),
                StorylineStreamWriteMode::Replace,
                None,
            )
            .await?;
        published_storyline_report(outcome)
    }

    pub(crate) async fn create_projected_storyline_stream<I>(
        &self,
        stories: I,
        projection: StorylineProjectionLineage,
    ) -> Result<StorylineProjectionPublicationOutcome>
    where
        I: IntoIterator<Item = Result<StorylineDocument>>,
    {
        projection.validate()?;
        self.replace_storyline_stream_with_projection(
            stories,
            Some(projection),
            StorylineStreamWriteMode::CreateProjection,
            None,
        )
        .await
    }

    /// Rebuild every normalized table into a new physical generation, then
    /// atomically replace CURRENT. Existing readers remain pinned to the old
    /// generation until publication succeeds.
    pub async fn rebuild_projected_storyline_stream<I>(
        &self,
        stories: I,
        projection: StorylineProjectionLineage,
    ) -> Result<StorylineStreamImportReport>
    where
        I: IntoIterator<Item = Result<StorylineDocument>>,
    {
        projection.validate()?;
        let outcome = self
            .replace_storyline_stream_with_projection(
                stories,
                Some(projection),
                StorylineStreamWriteMode::Rebuild,
                None,
            )
            .await?;
        published_storyline_report(outcome)
    }

    async fn replace_storyline_stream_with_projection<I>(
        &self,
        stories: I,
        projection: Option<StorylineProjectionLineage>,
        mode: StorylineStreamWriteMode,
        required_generation: Option<&str>,
    ) -> Result<StorylineProjectionPublicationOutcome>
    where
        I: IntoIterator<Item = Result<StorylineDocument>>,
    {
        let _guard = self.acquire_write_guard().await?;
        let original = self.resolve_current_table_paths().await?;
        if let Some(required_generation) = required_generation {
            anyhow::ensure!(
                original.as_ref().map(|paths| paths.generation.as_str())
                    == Some(required_generation),
                "Storyline append conflict: Dataset changed after duplicate-ID resolution"
            );
        }
        if mode == StorylineStreamWriteMode::CreateProjection && original.is_some() {
            return Ok(StorylineProjectionPublicationOutcome::OutputNotEmpty);
        }
        #[cfg(test)]
        if mode == StorylineStreamWriteMode::CreateProjection {
            wait_after_empty_current_read(&self.root_uri).await;
        }
        let expected_generation = original.as_ref().map(|paths| paths.generation.clone());
        #[cfg(test)]
        if mode == StorylineStreamWriteMode::Replace {
            wait_after_replacement_current_read(&self.root_uri).await;
        }
        let writer_owner = next_generation();
        let writer_lease = if mode == StorylineStreamWriteMode::Replace && original.is_some() {
            Some(
                self.acquire_writer_lease_for_generation(
                    &writer_owner,
                    expected_generation.as_deref(),
                )
                .await?,
            )
        } else {
            None
        };
        let mut writer_renewal = writer_lease
            .as_ref()
            .map(|lease| self.start_writer_lease_renewal(writer_owner.clone(), lease.lease.epoch));
        let rebuild = mode == StorylineStreamWriteMode::Rebuild;
        let takeover_generation = writer_lease
            .as_ref()
            .filter(|lease| lease.takeover)
            .map(|_| next_generation());
        let mut paths = if rebuild || takeover_generation.is_some() {
            None
        } else {
            original.clone()
        };
        let mut new_table_generation = takeover_generation.clone();
        let mut iterator = stories.into_iter();
        let mut chunk_state = StorylineChunkState::default();
        let mut next_storage_ordinal = if rebuild {
            0
        } else if let Some(paths) = &original {
            next_storage_ordinal(paths).await?
        } else {
            0
        };
        let mut report = StorylineStreamImportReport::default();

        let result = async {
            if let Some(generation) = takeover_generation.as_deref() {
                let source = original
                    .as_ref()
                    .context("missing committed Storyline generation during lease takeover")?;
                paths = Some(self.clone_table_generation(source, generation).await?);
            }
            loop {
                let Some(mut chunk) = next_storyline_stream_chunk(
                    &mut iterator,
                    &mut chunk_state,
                    &mut next_storage_ordinal,
                    self.content_options,
                )?
                else {
                    break;
                };
                if !rebuild && let Some(original) = &original {
                    let existing =
                        read_storage_ordinals_for_document_ids(original, &chunk.document_ids)
                            .await?;
                    for run in &mut chunk.runs {
                        if let Some(ordinal) = existing.get(&run.document_id) {
                            run.storage_ordinal = *ordinal;
                        }
                    }
                }
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
                        #[cfg(test)]
                        let first_content_create =
                            if mode == StorylineStreamWriteMode::CreateProjection {
                                wait_for_first_content_create(&self.root_uri).await
                            } else {
                                false
                            };
                        let objects_result = commit_pending_content(
                            &created.objects,
                            original.as_ref().map(|paths| paths.objects_version),
                            pending,
                            mode == StorylineStreamWriteMode::CreateProjection,
                        )
                        .await;
                        #[cfg(test)]
                        release_waiting_content_create(&self.root_uri, first_content_create);
                        let objects_version = objects_result?;
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
                        let predicate = document_set_predicate(&chunk.document_ids);
                        let objects_version = commit_pending_content(
                            &current.objects,
                            Some(current.objects_version),
                            pending,
                            false,
                        )
                        .await?;
                        let (runs_version, steps_version, tool_calls_version) = tokio::try_join!(
                            replace_table_batches(
                                &current.runs,
                                current.runs_version,
                                &predicate,
                                &["document_id"],
                                run_batches,
                                story_runs_arrow_schema(),
                            ),
                            replace_table_batches(
                                &current.steps,
                                current.steps_version,
                                &predicate,
                                &["document_id", "step_id"],
                                step_batches,
                                story_steps_arrow_schema(),
                            ),
                            replace_table_batches(
                                &current.tool_calls,
                                current.tool_calls_version,
                                &predicate,
                                &["document_id", "step_id", "call_index"],
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
                // Build indexes for a new store (including a small import),
                // and periodically after a large streamed import. Replacing
                // one small region in an existing store must not rebuild and
                // optimize every FTS/JSON index on every write; callers that
                // need to catch up appended fragments can invoke `maintain`.
                if original.is_none() || report.storylines > STREAM_IMPORT_STORIES {
                    let maintenance = LanceMaintenanceOptions {
                        // Extend scalar, FTS, and JSON indices once after
                        // import, without putting compaction in the ingest
                        // path or rewriting corpus data.
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
            let snapshot = StorylineSnapshotPointer {
                schema_version: STORYLINE_LANCE_SCHEMA_VERSION,
                generation: generation.clone(),
                parent_generation: expected_generation.clone(),
                table_generation: current.table_generation.clone(),
                runs_version,
                steps_version,
                tool_calls_version,
                objects_version: current.objects_version,
                projection,
            };
            let published = if let Some(lease) = &writer_lease {
                let renewal = writer_renewal
                    .take()
                    .context("missing Storyline writer lease renewal")?;
                anyhow::ensure!(renewal.stop().await, "Storyline writer lease lost");
                let published = self
                    .publish_writer_snapshot(&writer_owner, lease.lease.epoch, &snapshot)
                    .await?;
                anyhow::ensure!(
                    published,
                    "Storyline writer lease lost while publishing generation {}",
                    snapshot.generation
                );
                true
            } else if mode == StorylineStreamWriteMode::CreateProjection {
                self.try_commit_snapshot(&snapshot, expected_generation.as_deref())
                    .await?
            } else {
                self.commit_snapshot(&snapshot, expected_generation.as_deref())
                    .await?;
                true
            };
            if !published {
                return Ok(StorylineProjectionPublicationOutcome::OutputNotEmpty);
            }
            report.generation = generation;
            Ok(StorylineProjectionPublicationOutcome::Published(report))
        }
        .await;

        let mut cleanup_failures = Vec::new();
        if let Some(renewal) = writer_renewal.take()
            && !renewal.stop().await
        {
            cleanup_failures.push("writer lease renewal reported ownership loss".to_string());
        }
        if result.is_err()
            && let Some(lease) = &writer_lease
        {
            match self
                .release_writer_lease(&writer_owner, lease.lease.epoch)
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    cleanup_failures.push("writer lease was lost before error cleanup".to_string())
                }
                Err(error) => {
                    cleanup_failures.push(format!("release writer lease after error: {error:#}"))
                }
            }
        }

        if (result.is_err()
            || matches!(
                &result,
                Ok(StorylineProjectionPublicationOutcome::OutputNotEmpty)
            ))
            && let Some(generation) = new_table_generation
            && let Err(error) = self
                .control_store
                .remove(&self.generation_object_path(&generation))
                .await
        {
            cleanup_failures.push(format!(
                "remove uncommitted Storyline generation {generation}: {error:#}"
            ));
        }
        attach_stream_cleanup_failures(result, cleanup_failures)
    }

    /// Compact fragments, extend scalar/FTS/JSON indices to appended
    /// fragments, and optionally vacuum old Lance versions while preserving
    /// one atomic three-table CURRENT snapshot.
    pub async fn maintain(
        &self,
        options: &LanceMaintenanceOptions,
    ) -> Result<StorylineMaintenanceReport> {
        let _guard = self.acquire_write_guard().await?;
        let Some(original) = self.resolve_current_table_paths().await? else {
            return Ok(StorylineMaintenanceReport::default());
        };
        // Freeze deletion candidates before acquiring the lease. If this
        // worker later loses ownership, a successor generation created after
        // this point can never enter the stale worker's deletion set.
        let expired_generations = self
            .expired_generation_candidates(&original.table_generation, options.vacuum_older_than)
            .await?;
        let writer_owner = next_generation();
        let writer_lease = self
            .acquire_writer_lease_for_generation(&writer_owner, Some(&original.generation))
            .await?;
        let mut writer_renewal =
            Some(self.start_writer_lease_renewal(writer_owner.clone(), writer_lease.lease.epoch));
        let takeover_generation = writer_lease.takeover.then(next_generation);
        let mut published = false;

        let mut result: Result<StorylineMaintenanceReport> = async {
            let paths = if let Some(generation) = takeover_generation.as_deref() {
                self.clone_table_generation(&original, generation).await?
            } else {
                original.clone()
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
            let tool_call_content_columns =
                content_column_projection(StorylineTableKind::ToolCalls);
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
            let snapshot = StorylineSnapshotPointer {
                schema_version: STORYLINE_LANCE_SCHEMA_VERSION,
                generation: generation.clone(),
                parent_generation: Some(original.generation.clone()),
                table_generation: paths.table_generation.clone(),
                runs_version,
                steps_version,
                tool_calls_version,
                objects_version,
                projection: paths.projection.clone(),
            };
            let published_snapshot = self
                .publish_writer_snapshot_retaining_lease(
                    &writer_owner,
                    writer_lease.lease.epoch,
                    &snapshot,
                )
                .await?;
            anyhow::ensure!(
                published_snapshot,
                "Storyline writer lease lost while publishing generation {}",
                snapshot.generation
            );
            published = true;
            #[cfg(test)]
            wait_after_maintenance_publish(&self.root_uri).await;

            let (runs_vacuum, steps_vacuum, tool_calls_vacuum) = tokio::try_join!(
                vacuum_table(&paths.runs, options.vacuum_older_than),
                vacuum_table(&paths.steps, options.vacuum_older_than),
                vacuum_table(&paths.tool_calls, options.vacuum_older_than),
            )?;
            // Local stores remain protected by the cross-process file lock.
            // Remote stores share objects.lance across physical generations,
            // so vacuuming it could remove a version pinned by a successor
            // after this lease expires.
            let objects_vacuum = if matches!(self.storage_scheme(), "file" | "file+uring") {
                vacuum_table(&paths.objects, options.vacuum_older_than).await?
            } else {
                LanceMaintenanceReport::default()
            };
            let generations_removed = self
                .prune_generation_candidates(expired_generations)
                .await?;
            Ok(StorylineMaintenanceReport {
                generation: Some(generation),
                runs: merge_maintenance_reports(runs, runs_vacuum),
                steps: merge_maintenance_reports(steps, steps_vacuum),
                tool_calls: merge_maintenance_reports(tool_calls, tool_calls_vacuum),
                objects: objects_vacuum,
                objects_removed,
                generations_removed,
            })
        }
        .await;

        let mut cleanup_failures = Vec::new();
        if let Some(renewal) = writer_renewal.take()
            && !renewal.stop().await
        {
            cleanup_failures.push("writer lease renewal reported ownership loss".to_string());
        }
        match self
            .release_writer_lease(&writer_owner, writer_lease.lease.epoch)
            .await
        {
            Ok(true) => {}
            Ok(false) => cleanup_failures.push("writer lease was lost before release".to_string()),
            Err(error) => cleanup_failures.push(format!("release writer lease: {error:#}")),
        }
        if result.is_err()
            && !published
            && let Some(generation) = takeover_generation
            && let Err(error) = self
                .control_store
                .remove(&self.generation_object_path(&generation))
                .await
        {
            cleanup_failures.push(format!(
                "remove uncommitted Storyline generation {generation}: {error:#}"
            ));
        }
        if !cleanup_failures.is_empty() {
            let cleanup = format!(
                "Storyline maintenance cleanup failed: {}",
                cleanup_failures.join("; ")
            );
            result = match result {
                Ok(_) => Err(anyhow::anyhow!(cleanup)),
                Err(error) => Err(error.context(cleanup)),
            };
        }
        result
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
        let outcome = self
            .replace_storyline_stream_with_projection(
                stories.iter().cloned().map(Ok::<_, anyhow::Error>),
                None,
                StorylineStreamWriteMode::Replace,
                None,
            )
            .await?;
        published_storyline_report(outcome)?;
        Ok(())
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
        self.get_storylines_full_by("session_id", session_ids).await
    }

    /// Read multiple Storylines by stable per-document identity.
    ///
    /// Unlike session lookup, this remains unambiguous when ATIF v1.7 sibling
    /// trajectories share one run-scoped `session_id`.
    pub async fn get_storylines_by_document_ids(
        &self,
        document_ids: &[String],
    ) -> Result<Vec<Option<StorylineDocument>>> {
        self.get_storylines_full_by("document_id", document_ids)
            .await
    }

    async fn get_storylines_full_by(
        &self,
        column: &str,
        ids: &[String],
    ) -> Result<Vec<Option<StorylineDocument>>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let requested = ids.iter().cloned().collect::<HashSet<_>>();
        anyhow::ensure!(
            requested.len() == ids.len(),
            "duplicate {column} in Storyline point batch"
        );
        let Some(paths) = self.resolve_current_table_paths().await? else {
            return Ok(vec![None; ids.len()]);
        };
        let predicate = id_set_predicate(column, &requested);
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
        let row_key = |document_id: &str, session_id: &str| match column {
            "document_id" => document_id.to_string(),
            _ => session_id.to_string(),
        };
        let mut runs = HashMap::with_capacity(ids.len());
        for mut run in decode_run_batches(&run_batches)? {
            run.unknown_key_counts = compute_unknown_key_counts(&run.unknown_fields)?;
            let key = row_key(&run.document_id, &run.session_id);
            if runs.insert(key.clone(), run).is_some() {
                anyhow::bail!("duplicate runs rows for {column} '{key}'");
            }
        }
        let mut steps = HashMap::<String, Vec<StoryStepRow>>::new();
        for step in decode_step_batches(&step_batches)? {
            let key = row_key(&step.document_id, &step.session_id);
            steps.entry(key).or_default().push(step);
        }
        let mut tool_calls = HashMap::<String, Vec<StoryToolCallRow>>::new();
        for tool_call in decode_tool_call_batches(&tool_call_batches)? {
            let key = row_key(&tool_call.document_id, &tool_call.session_id);
            tool_calls.entry(key).or_default().push(tool_call);
        }

        ids.iter()
            .map(|id| {
                let Some(run) = runs.remove(id) else {
                    return Ok(None);
                };
                let story = reconstruct_storyline(StorylineTables {
                    run,
                    steps: steps.remove(id).unwrap_or_default(),
                    tool_calls: tool_calls.remove(id).unwrap_or_default(),
                })?;
                validate_unknown_fields(
                    &story.unknown_fields,
                    self.content_options.unknown_field_limits(),
                )?;
                Ok(Some(story))
            })
            .collect()
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
            projection: None,
        }
    }

    async fn clone_table_generation(
        &self,
        source: &StorylineTablePaths,
        generation: &str,
    ) -> Result<StorylineTablePaths> {
        let (run_batches, step_batches, tool_call_batches) = tokio::try_join!(
            read_projected_batches(&source.runs, source.runs_version, &[], None),
            read_projected_batches(&source.steps, source.steps_version, &[], None),
            read_projected_batches(&source.tool_calls, source.tool_calls_version, &[], None,),
        )?;
        let mut cloned = self.paths_for_generation(generation);
        let (runs_version, steps_version, tool_calls_version) = tokio::try_join!(
            write_batches(
                &cloned.runs,
                run_batches,
                story_runs_arrow_schema(),
                &RUN_INDEXES,
            ),
            write_batches(
                &cloned.steps,
                step_batches,
                story_steps_arrow_schema(),
                &STEP_INDEXES,
            ),
            write_batches(
                &cloned.tool_calls,
                tool_call_batches,
                story_tool_calls_arrow_schema(),
                &TOOL_CALL_INDEXES,
            ),
        )?;
        cloned.generation.clone_from(&source.generation);
        cloned.runs_version = runs_version;
        cloned.steps_version = steps_version;
        cloned.tool_calls_version = tool_calls_version;
        cloned.objects_version = source.objects_version;
        cloned.projection.clone_from(&source.projection);
        Ok(cloned)
    }

    fn generation_object_path(&self, generation: &str) -> String {
        format!("{GENERATIONS_DIR}/{generation}")
    }

    async fn expired_generation_candidates(
        &self,
        current: &str,
        retention: Option<std::time::Duration>,
    ) -> Result<std::collections::BTreeSet<String>> {
        let Some(retention) = retention else {
            return Ok(std::collections::BTreeSet::new());
        };
        let cutoff_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .saturating_sub(retention)
            .as_nanos();
        let prefix = format!("{GENERATIONS_DIR}/");
        let objects = self
            .control_store
            .list(GENERATIONS_DIR)
            .await
            .context("list Storyline physical generations")?;
        let mut candidates = std::collections::BTreeSet::new();
        for object in objects {
            let Some(relative) = object.path.strip_prefix(&prefix) else {
                continue;
            };
            let Some(generation) = relative.split('/').next() else {
                continue;
            };
            if generation.is_empty() || generation == current {
                continue;
            }
            let Some(created_nanos) = parse_generation_timestamp(generation) else {
                continue;
            };
            if created_nanos < cutoff_nanos {
                candidates.insert(generation.to_string());
            }
        }

        Ok(candidates)
    }

    async fn prune_generation_candidates(
        &self,
        candidates: std::collections::BTreeSet<String>,
    ) -> Result<usize> {
        let mut removed = 0;
        for generation in candidates {
            self.control_store
                .remove(&self.generation_object_path(&generation))
                .await
                .with_context(|| format!("remove expired Storyline generation {generation}"))?;
            removed += 1;
        }
        Ok(removed)
    }

    async fn commit_snapshot(
        &self,
        snapshot: &StorylineSnapshotPointer,
        expected_generation: Option<&str>,
    ) -> Result<()> {
        if self
            .try_commit_snapshot(snapshot, expected_generation)
            .await?
        {
            return Ok(());
        }
        anyhow::bail!(
            "Storyline commit conflict while publishing generation {}",
            snapshot.generation
        )
    }

    async fn try_commit_snapshot(
        &self,
        snapshot: &StorylineSnapshotPointer,
        expected_generation: Option<&str>,
    ) -> Result<bool> {
        self.try_publish_unleased_snapshot(snapshot, expected_generation)
            .await
    }
}

fn validate_snapshot_pointer(pointer: &StorylineSnapshotPointer) -> Result<()> {
    anyhow::ensure!(
        pointer.schema_version == STORYLINE_LANCE_SCHEMA_VERSION,
        "unsupported Storyline Lance schema_version {}; expected {}",
        pointer.schema_version,
        STORYLINE_LANCE_SCHEMA_VERSION
    );
    if let Some(projection) = &pointer.projection {
        projection.validate()?;
    }
    validate_generation_name(&pointer.generation)?;
    if let Some(parent) = &pointer.parent_generation {
        validate_generation_name(parent)?;
    }
    validate_generation_name(&pointer.table_generation)
}

fn validate_current_control(control: &writer_control::StorylineCurrentControl) -> Result<()> {
    anyhow::ensure!(
        control.control_version == writer_control::CURRENT_CONTROL_VERSION,
        "unsupported Storyline CURRENT control_version {}; expected {}",
        control.control_version,
        writer_control::CURRENT_CONTROL_VERSION
    );
    if let Some(pointer) = &control.committed {
        validate_snapshot_pointer(pointer)?;
    }
    if let Some(lease) = &control.lease {
        anyhow::ensure!(
            !lease.owner_id.trim().is_empty(),
            "Storyline writer lease owner must not be empty"
        );
        anyhow::ensure!(
            lease.expires_at_unix_ms > lease.issued_at_unix_ms,
            "Storyline writer lease expiry must follow issuance"
        );
        anyhow::ensure!(
            lease.base_generation.as_deref()
                == control
                    .committed
                    .as_ref()
                    .map(|pointer| pointer.generation.as_str()),
            "Storyline writer lease base generation does not match committed generation"
        );
    }
    Ok(())
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
        File::open(parent)
            .with_context(|| format!("open Storyline root {} for sync", parent.display()))?
            .sync_all()
            .with_context(|| format!("sync Storyline root {}", parent.display()))?;
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
    // Validate object-store roots before opening Lance so malformed S3
    // endpoint-in-URI forms produce an actionable error instead of a vague
    // region-discovery failure from the underlying client.
    Ok(crate::storage::DatasetLocation::parse(&value)?
        .as_str()
        .to_owned())
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

fn parse_generation_timestamp(value: &str) -> Option<u128> {
    let mut parts = value.strip_prefix("gen-")?.split('-');
    let nanos = parts.next()?.parse::<u128>().ok()?;
    parts.next()?.parse::<u32>().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(nanos)
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
    runs.sort_by(|a, b| a.document_id.cmp(&b.document_id));
    steps.sort_by(|a, b| {
        a.document_id
            .cmp(&b.document_id)
            .then(a.step_id.cmp(&b.step_id))
    });
    tool_calls.sort_by(|a, b| {
        a.document_id
            .cmp(&b.document_id)
            .then(a.step_id.cmp(&b.step_id))
            .then(a.call_index.cmp(&b.call_index))
    });
}

async fn ensure_table_indexes(dataset: &mut Dataset, indexes: &[(&str, IndexType)]) -> Result<()> {
    if dataset.count_rows(None).await? == 0 {
        return Ok(());
    }
    for (column, index_type) in indexes {
        let builtin = match index_type {
            IndexType::Bitmap => BuiltinIndexType::Bitmap,
            _ => BuiltinIndexType::BTree,
        };
        ensure_named_index(
            dataset,
            column,
            *index_type,
            format!("pchronicle_{column}_idx"),
            &ScalarIndexParams::for_builtin(builtin),
        )
        .await?;
    }

    // Compaction tests can suppress inverted FTS/JSON indexes: building Jieba
    // inverted indexes on a new store, then remapping them during compact, is
    // the dominant cost of Storyline lib tests on macOS CI. Other unit tests
    // still build them; integration coverage lives in
    // `tests/atif_lance_corpus.rs`.
    #[cfg(test)]
    if inverted_indexes_suppressed_for(dataset) {
        return Ok(());
    }

    crate::search::storyline::ensure_storyline_search_indexes(dataset).await
}

async fn ensure_named_index(
    dataset: &mut Dataset,
    column: &str,
    index_type: IndexType,
    name: String,
    params: &dyn lance_index::IndexParams,
) -> Result<()> {
    let existing = dataset.load_indices_by_name(&name).await?;
    if !existing.is_empty() {
        return Ok(());
    }
    let _admission = super::index_build_gate::acquire().await;
    dataset
        .create_index(&[column], index_type, Some(name), params, false)
        .await?;
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
    let mut columns = content_columns(kind)
        .iter()
        .map(|(column, _)| *column)
        .collect::<Vec<_>>();
    if kind == StorylineTableKind::Runs {
        columns.push("unknown_fields");
    }
    columns
}

async fn next_storage_ordinal(paths: &StorylineTablePaths) -> Result<i64> {
    let dataset = open_table_version(&paths.runs, paths.runs_version).await?;
    let mut scan = dataset.scan();
    scan.project(&["storage_ordinal"])
        .with_context(|| format!("project Storyline Lance table {}", paths.runs.display()))?;
    let mut batches = scan
        .try_into_stream()
        .await
        .with_context(|| format!("scan Storyline Lance table {}", paths.runs.display()))?;
    let mut maximum = None::<i64>;
    while let Some(batch) = batches
        .try_next()
        .await
        .with_context(|| format!("read Storyline Lance table {}", paths.runs.display()))?
    {
        let ordinals = batch
            .column_by_name("storage_ordinal")
            .and_then(|array| array.as_any().downcast_ref::<Int64Array>())
            .context("Storyline runs storage_ordinal column is missing or invalid")?;
        for row in 0..ordinals.len() {
            let ordinal = ordinals.value(row);
            anyhow::ensure!(ordinal >= 0, "negative Storyline storage ordinal");
            maximum = Some(maximum.map_or(ordinal, |current| current.max(ordinal)));
        }
    }
    maximum.map_or(Ok(0), |ordinal| {
        ordinal
            .checked_add(1)
            .context("Storyline storage ordinal overflow")
    })
}

async fn read_storage_ordinals_for_document_ids(
    paths: &StorylineTablePaths,
    document_ids: &HashSet<String>,
) -> Result<HashMap<String, i64>> {
    let predicate = document_set_predicate(document_ids);
    let batches = read_projected_batches(
        &paths.runs,
        paths.runs_version,
        &["document_id", "storage_ordinal"],
        Some(&predicate),
    )
    .await?;
    let mut ordinals = HashMap::new();
    for batch in batches {
        let document_ids = batch
            .column_by_name("document_id")
            .and_then(|array| array.as_any().downcast_ref::<StringArray>())
            .context("Storyline runs document_id column is missing or invalid")?;
        let storage_ordinals = batch
            .column_by_name("storage_ordinal")
            .and_then(|array| array.as_any().downcast_ref::<Int64Array>())
            .context("Storyline runs storage_ordinal column is missing or invalid")?;
        anyhow::ensure!(
            document_ids.len() == storage_ordinals.len(),
            "Storyline storage ordinal projection has inconsistent lengths"
        );
        for row in 0..batch.num_rows() {
            let document_id = document_ids.value(row).to_string();
            let ordinal = storage_ordinals.value(row);
            anyhow::ensure!(ordinal >= 0, "negative storage ordinal for '{document_id}'");
            anyhow::ensure!(
                ordinals.insert(document_id.clone(), ordinal).is_none(),
                "duplicate Storyline run for document_id '{document_id}'"
            );
        }
    }
    Ok(ordinals)
}

async fn read_projected_batches(
    path: &Path,
    version: u64,
    projection: &[&str],
    predicate: Option<&str>,
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

fn document_set_predicate(document_ids: &HashSet<String>) -> String {
    id_set_predicate("document_id", document_ids)
}

fn id_set_predicate(column: &str, ids: &HashSet<String>) -> String {
    debug_assert!(matches!(column, "document_id" | "session_id"));
    let mut values = ids
        .iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect::<Vec<_>>();
    values.sort();
    format!("{column} IN ({})", values.join(", "))
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
mod tests;

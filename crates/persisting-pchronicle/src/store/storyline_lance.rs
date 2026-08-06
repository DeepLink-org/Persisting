//! Storyline-native normalized Lance store.
//!
//! `CURRENT` pins one exact MVCC version from each of the three Lance datasets.
//! A replacement deletes and appends rows for only the requested sessions,
//! then moves `CURRENT` after all table versions are durable. Readers therefore
//! never observe a partially updated Storyline.
//!
//! ```text
//! root/
//!   CURRENT  # logical snapshot id + three Lance version ids
//!   generations/<table-generation>/
//!     runs.lance/
//!     steps.lance/
//!     tool_calls.lance/
//! ```

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures::TryStreamExt;
use lance::dataset::optimize::{compact_files, CompactionOptions};
use lance::dataset::{InsertBuilder, WriteMode, WriteParams};
use lance::deps::arrow_array::{RecordBatch, RecordBatchIterator, RecordBatchReader};
use lance::deps::arrow_schema::{ArrowError, SchemaRef};
use lance::index::DatasetIndexExt;
use lance::io::ObjectStore;
use lance::Dataset;
use lance_index::optimize::OptimizeOptions;
use lance_index::scalar::{BuiltinIndexType, ScalarIndexParams};
use lance_index::IndexType;
use object_store::path::Path as ObjectPath;
use serde::{Deserialize, Serialize};

use crate::storyline_schema::{
    reconstruct_storyline, split_storyline, StoryRunRow, StoryStepRow, StoryToolCallRow,
    StorylineTables, STORY_RUNS_TABLE, STORY_STEPS_TABLE, STORY_TOOL_CALLS_TABLE,
};
use crate::StorylineDocument;

use super::atif_datafusion::atif_table_streams;
use super::storyline_lance_rows::{
    story_runs_arrow_schema, story_runs_from_batch, story_runs_to_batch, story_steps_arrow_schema,
    story_steps_from_batch, story_steps_to_batch, story_tool_calls_arrow_schema,
    story_tool_calls_from_batch, story_tool_calls_to_batch,
};
use super::{LanceMaintenanceOptions, LanceMaintenanceReport};

const CURRENT_FILE: &str = "CURRENT";
const GENERATIONS_DIR: &str = "generations";
const WRITE_BATCH_ROWS: usize = 8192;
const STREAM_IMPORT_STORIES: usize = 256;
const AUTO_COMPACT_FRAGMENT_COUNT: usize = 32;
const RUN_INDEXES: [(&str, IndexType); 2] = [
    ("session_id", IndexType::BTree),
    ("run_id", IndexType::BTree),
];
// `step_id` restarts inside every Storyline and has low global selectivity.
// `session_id` first narrows a lookup to one short Storyline, after which a
// step range is cheaper to filter than maintaining another BTree.
const STEP_INDEXES: [(&str, IndexType); 3] = [
    ("session_id", IndexType::BTree),
    ("effective_kind", IndexType::Bitmap),
    ("source", IndexType::Bitmap),
];
const TOOL_CALL_INDEXES: [(&str, IndexType); 3] = [
    ("session_id", IndexType::BTree),
    ("tool_call_id", IndexType::BTree),
    ("function_name", IndexType::Bitmap),
];

fn write_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorylineTablePaths {
    /// Logical three-table snapshot id. Changes after every committed replace.
    pub generation: String,
    /// Physical Lance dataset generation shared by successive MVCC snapshots.
    pub table_generation: String,
    pub runs: PathBuf,
    pub steps: PathBuf,
    pub tool_calls: PathBuf,
    pub runs_version: u64,
    pub steps_version: u64,
    pub tool_calls_version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StorylineSnapshotPointer {
    generation: String,
    table_generation: String,
    runs_version: u64,
    steps_version: u64,
    tool_calls_version: u64,
}

#[derive(Debug, Clone)]
pub struct StorylineLanceStore {
    root: PathBuf,
    root_uri: String,
    object_store: std::sync::Arc<ObjectStore>,
    object_root: ObjectPath,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorylineMaintenanceReport {
    pub generation: Option<String>,
    pub runs: LanceMaintenanceReport,
    pub steps: LanceMaintenanceReport,
    pub tool_calls: LanceMaintenanceReport,
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

    pub(crate) async fn open_uri_unchecked(root: impl AsRef<str>) -> Result<Self> {
        let root_uri = normalize_root_uri(root.as_ref())?;
        let (object_store, object_root) = ObjectStore::from_uri(&root_uri)
            .await
            .with_context(|| format!("open Storyline object store {root_uri}"))?;
        Ok(Self {
            root: PathBuf::from(&root_uri),
            root_uri,
            object_store,
            object_root,
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
        )?;
        Ok(Some(paths))
    }

    pub(crate) async fn resolve_current_table_paths(&self) -> Result<Option<StorylineTablePaths>> {
        let pointer = self.object_root.clone().join(CURRENT_FILE);
        if !self
            .object_store
            .exists(&pointer)
            .await
            .with_context(|| format!("check Storyline commit pointer {}/CURRENT", self.root_uri))?
        {
            return Ok(None);
        }
        let contents = self
            .object_store
            .read_one_all(&pointer)
            .await
            .with_context(|| format!("read Storyline commit pointer {}/CURRENT", self.root_uri))?;
        let contents = std::str::from_utf8(&contents)
            .context("Storyline commit pointer is not valid UTF-8")?
            .trim();
        let pointer = if contents.starts_with('{') {
            serde_json::from_str::<StorylineSnapshotPointer>(contents)
                .context("decode Storyline snapshot pointer")?
        } else {
            // Compatibility with the original pointer, which contained only a
            // generation name and implicitly selected each dataset's latest
            // version.
            validate_generation_name(contents)?;
            let paths = self.paths_for_generation(contents);
            let (runs_version, steps_version, tool_calls_version) = tokio::try_join!(
                latest_table_version(&paths.runs),
                latest_table_version(&paths.steps),
                latest_table_version(&paths.tool_calls),
            )
            .with_context(|| format!("Storyline generation '{contents}' is incomplete"))?;
            StorylineSnapshotPointer {
                generation: contents.to_string(),
                table_generation: contents.to_string(),
                runs_version,
                steps_version,
                tool_calls_version,
            }
        };
        validate_generation_name(&pointer.generation)?;
        validate_generation_name(&pointer.table_generation)?;
        let mut paths = self.paths_for_generation(&pointer.table_generation);
        paths.generation = pointer.generation;
        paths.runs_version = pointer.runs_version;
        paths.steps_version = pointer.steps_version;
        paths.tool_calls_version = pointer.tool_calls_version;
        Ok(Some(paths))
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
        let _guard = write_lock().lock().await;
        anyhow::ensure!(
            self.resolve_current_table_paths().await?.is_none(),
            "streaming ATIF create requires an empty Storyline store"
        );
        let streams = atif_table_streams(input.as_ref(), WRITE_BATCH_ROWS)?;
        let run_reader = streams.runs;
        let step_reader = streams.steps;
        let tool_call_reader = streams.tool_calls;
        let completion = streams.completion;

        let generation = next_generation();
        let paths = self.paths_for_generation(&generation);
        let write_result = async {
            let writes = tokio::try_join!(
                write_record_batch_reader(&paths.runs, run_reader, &RUN_INDEXES),
                write_record_batch_reader(&paths.steps, step_reader, &STEP_INDEXES),
                write_record_batch_reader(&paths.tool_calls, tool_call_reader, &TOOL_CALL_INDEXES,),
            );
            let produced = completion
                .join()
                .map_err(|_| anyhow::anyhow!("ATIF streaming producer panicked"))?;
            let (runs_version, steps_version, tool_calls_version) = writes?;
            let stats = produced?;
            let snapshot = next_generation();
            self.commit_snapshot(&StorylineSnapshotPointer {
                generation: snapshot.clone(),
                table_generation: generation.clone(),
                runs_version,
                steps_version,
                tool_calls_version,
            })
            .await?;
            Ok(StorylineStreamImportReport {
                generation: snapshot,
                storylines: stats.document_count,
                steps: stats.step_count,
                tool_calls: stats.tool_call_count,
            })
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
        let _guard = write_lock().lock().await;
        let original = self.resolve_current_table_paths().await?;
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

                paths = Some(match paths {
                    None => {
                        let generation = next_generation();
                        let mut created = self.paths_for_generation(&generation);
                        let (runs_version, steps_version, tool_calls_version) = tokio::try_join!(
                            write_rows(
                                &created.runs,
                                chunk.runs,
                                story_runs_arrow_schema(),
                                story_runs_to_batch,
                                &RUN_INDEXES,
                            ),
                            write_rows(
                                &created.steps,
                                chunk.steps,
                                story_steps_arrow_schema(),
                                story_steps_to_batch,
                                &STEP_INDEXES,
                            ),
                            write_rows(
                                &created.tool_calls,
                                chunk.tool_calls,
                                story_tool_calls_arrow_schema(),
                                story_tool_calls_to_batch,
                                &TOOL_CALL_INDEXES,
                            ),
                        )?;
                        created.runs_version = runs_version;
                        created.steps_version = steps_version;
                        created.tool_calls_version = tool_calls_version;
                        new_table_generation = Some(generation);
                        created
                    }
                    Some(mut current) => {
                        let predicate = session_set_predicate(&chunk.session_ids);
                        let (runs_version, steps_version, tool_calls_version) = tokio::try_join!(
                            replace_table_rows(
                                &current.runs,
                                current.runs_version,
                                &predicate,
                                chunk.runs,
                                story_runs_arrow_schema(),
                                story_runs_to_batch,
                            ),
                            replace_table_rows(
                                &current.steps,
                                current.steps_version,
                                &predicate,
                                chunk.steps,
                                story_steps_arrow_schema(),
                                story_steps_to_batch,
                            ),
                            replace_table_rows(
                                &current.tool_calls,
                                current.tool_calls_version,
                                &predicate,
                                chunk.tool_calls,
                                story_tool_calls_arrow_schema(),
                                story_tool_calls_to_batch,
                            ),
                        )?;
                        current.runs_version = runs_version;
                        current.steps_version = steps_version;
                        current.tool_calls_version = tool_calls_version;
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
                        // Each chunk already triggers threshold-based
                        // compaction. Extend indices once after a genuinely
                        // multi-chunk import, without rewriting the corpus.
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
            self.commit_snapshot(&StorylineSnapshotPointer {
                generation: generation.clone(),
                table_generation: current.table_generation.clone(),
                runs_version,
                steps_version,
                tool_calls_version,
            })
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
        let _guard = write_lock().lock().await;
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
        let generation = next_generation();
        self.commit_snapshot(&StorylineSnapshotPointer {
            generation: generation.clone(),
            table_generation: paths.table_generation.clone(),
            runs_version: runs
                .final_version
                .context("missing maintained runs version")?,
            steps_version: steps
                .final_version
                .context("missing maintained steps version")?,
            tool_calls_version: tool_calls
                .final_version
                .context("missing maintained tool_calls version")?,
        })
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
        let _guard = write_lock().lock().await;

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

    pub async fn get_storyline(&self, session_id: &str) -> Result<Option<StorylineDocument>> {
        Ok(self
            .get_storylines(&[session_id.to_string()])
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
    pub async fn get_storylines(
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

    pub async fn list_runs(&self) -> Result<Vec<StoryRunRow>> {
        Ok(self.read_all().await?.0)
    }

    pub async fn list_steps(&self, session_id: &str) -> Result<Vec<StoryStepRow>> {
        let Some(paths) = self.resolve_current_table_paths().await? else {
            return Ok(Vec::new());
        };
        decode_step_batches(
            &read_filtered_batches(
                &paths.steps,
                paths.steps_version,
                &session_predicate(session_id),
            )
            .await?,
        )
    }

    pub async fn list_tool_calls(&self, session_id: &str) -> Result<Vec<StoryToolCallRow>> {
        let Some(paths) = self.resolve_current_table_paths().await? else {
            return Ok(Vec::new());
        };
        decode_tool_call_batches(
            &read_filtered_batches(
                &paths.tool_calls,
                paths.tool_calls_version,
                &session_predicate(session_id),
            )
            .await?,
        )
    }

    async fn create_initial_snapshot(
        &self,
        runs: Vec<StoryRunRow>,
        steps: Vec<StoryStepRow>,
        tool_calls: Vec<StoryToolCallRow>,
    ) -> Result<()> {
        let generation = next_generation();
        let paths = self.paths_for_generation(&generation);
        let write_result = async {
            let (runs_version, steps_version, tool_calls_version) = tokio::try_join!(
                write_rows(
                    &paths.runs,
                    runs,
                    story_runs_arrow_schema(),
                    story_runs_to_batch,
                    &RUN_INDEXES,
                ),
                write_rows(
                    &paths.steps,
                    steps,
                    story_steps_arrow_schema(),
                    story_steps_to_batch,
                    &STEP_INDEXES,
                ),
                write_rows(
                    &paths.tool_calls,
                    tool_calls,
                    story_tool_calls_arrow_schema(),
                    story_tool_calls_to_batch,
                    &TOOL_CALL_INDEXES,
                ),
            )?;
            self.commit_snapshot(&StorylineSnapshotPointer {
                generation: generation.clone(),
                table_generation: generation.clone(),
                runs_version,
                steps_version,
                tool_calls_version,
            })
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
        let (runs_version, steps_version, tool_calls_version) = tokio::try_join!(
            replace_table_rows(
                &paths.runs,
                paths.runs_version,
                &predicate,
                runs,
                story_runs_arrow_schema(),
                story_runs_to_batch,
            ),
            replace_table_rows(
                &paths.steps,
                paths.steps_version,
                &predicate,
                steps,
                story_steps_arrow_schema(),
                story_steps_to_batch,
            ),
            replace_table_rows(
                &paths.tool_calls,
                paths.tool_calls_version,
                &predicate,
                tool_calls,
                story_tool_calls_arrow_schema(),
                story_tool_calls_to_batch,
            ),
        )?;
        self.commit_snapshot(&StorylineSnapshotPointer {
            generation: next_generation(),
            table_generation: paths.table_generation.clone(),
            runs_version,
            steps_version,
            tool_calls_version,
        })
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
            runs_version: 0,
            steps_version: 0,
            tool_calls_version: 0,
        }
    }

    fn generation_object_path(&self, generation: &str) -> ObjectPath {
        self.object_root
            .clone()
            .join(GENERATIONS_DIR)
            .join(generation)
    }

    async fn read_all(
        &self,
    ) -> Result<(Vec<StoryRunRow>, Vec<StoryStepRow>, Vec<StoryToolCallRow>)> {
        let Some(paths) = self.resolve_current_table_paths().await? else {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        };
        let (run_batches, step_batches, tool_call_batches) = tokio::try_join!(
            read_batches(&paths.runs, paths.runs_version),
            read_batches(&paths.steps, paths.steps_version),
            read_batches(&paths.tool_calls, paths.tool_calls_version),
        )?;
        let runs = decode_run_batches(&run_batches)?;
        let steps = decode_step_batches(&step_batches)?;
        let tool_calls = decode_tool_call_batches(&tool_call_batches)?;
        Ok((runs, steps, tool_calls))
    }

    async fn commit_snapshot(&self, snapshot: &StorylineSnapshotPointer) -> Result<()> {
        let pointer = self.object_root.clone().join(CURRENT_FILE);
        let contents = serde_json::to_vec(snapshot).context("encode Storyline snapshot pointer")?;
        self.object_store
            .put(&pointer, &contents)
            .await
            .with_context(|| format!("commit Storyline generation {}", snapshot.generation))?;
        Ok(())
    }
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

fn encoded_batch_reader<T: Send + Sync + 'static>(
    rows: Vec<T>,
    schema: SchemaRef,
    encode: fn(&[T]) -> Result<RecordBatch>,
) -> RecordBatchIterator<EncodedBatchIterator<T>> {
    RecordBatchIterator::new(EncodedBatchIterator::new(rows, encode), schema)
}

async fn write_rows<T: Send + Sync + 'static>(
    path: &Path,
    rows: Vec<T>,
    schema: SchemaRef,
    encode: fn(&[T]) -> Result<RecordBatch>,
    indexes: &[(&str, IndexType)],
) -> Result<u64> {
    let uri = path.to_string_lossy().into_owned();
    let row_count = rows.len();
    let mut dataset = InsertBuilder::new(&uri)
        .with_params(&WriteParams {
            mode: WriteMode::Create,
            ..Default::default()
        })
        .execute_stream(encoded_batch_reader(rows, schema, encode))
        .await
        .with_context(|| format!("write Storyline Lance table {}", path.display()))?;
    if row_count > 0 {
        for (column, index_type) in indexes {
            let builtin = match index_type {
                IndexType::Bitmap => BuiltinIndexType::Bitmap,
                _ => BuiltinIndexType::BTree,
            };
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

async fn replace_table_rows<T: Send + Sync + 'static>(
    path: &Path,
    snapshot_version: u64,
    predicate: &str,
    rows: Vec<T>,
    schema: SchemaRef,
    encode: fn(&[T]) -> Result<RecordBatch>,
) -> Result<u64> {
    let mut dataset = open_table_version(path, snapshot_version).await?;
    // A previous interrupted multi-table update may have advanced this Lance
    // dataset without advancing CURRENT. Restore the committed version before
    // deriving the next snapshot so uncommitted rows never leak forward.
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
    if !rows.is_empty() {
        dataset = InsertBuilder::new(std::sync::Arc::new(dataset))
            .with_params(&WriteParams {
                mode: WriteMode::Append,
                ..Default::default()
            })
            .execute_stream(encoded_batch_reader(rows, schema, encode))
            .await
            .with_context(|| format!("append replacement rows to {}", path.display()))?;
    }
    if dataset.get_fragments().len() >= AUTO_COMPACT_FRAGMENT_COUNT {
        // Cover appended fragments before compaction so indexed and unindexed
        // fragments can be planned together.
        dataset
            .optimize_indices(&OptimizeOptions::append())
            .await
            .with_context(|| format!("extend indices before compacting {}", path.display()))?;
        compact_files(&mut dataset, CompactionOptions::default(), None)
            .await
            .with_context(|| format!("compact Storyline table {}", path.display()))?;
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

async fn read_batches(path: &Path, version: u64) -> Result<Vec<RecordBatch>> {
    let dataset = open_table_version(path, version).await?;
    dataset
        .scan()
        .try_into_stream()
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
    let dataset = open_table_version(path, version).await?;
    let mut scan = dataset.scan();
    scan.filter(predicate)
        .with_context(|| format!("filter Storyline Lance table {}", path.display()))?;
    scan.use_scalar_index(true);
    scan.try_into_stream()
        .await
        .with_context(|| format!("scan Storyline Lance table {}", path.display()))?
        .try_collect()
        .await
        .with_context(|| format!("read Storyline Lance table {}", path.display()))
}

fn session_predicate(session_id: &str) -> String {
    format!("session_id = '{}'", session_id.replace('\'', "''"))
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
            store.get_storyline("session-1").await.unwrap(),
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
        assert_eq!(store.get_storyline("empty").await.unwrap(), Some(expected));
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
        assert!(store.get_storyline("a").await.unwrap().is_some());
        assert!(store.get_storyline("b").await.unwrap().is_some());

        let mut updated = story("a");
        updated.notes = Some("updated".into());
        updated.turns.truncate(1);
        store.replace_storyline(&updated).await.unwrap();
        assert_eq!(store.list_runs().await.unwrap().len(), 2);
        assert_eq!(store.list_steps("a").await.unwrap().len(), 1);
        assert!(store.list_tool_calls("a").await.unwrap().is_empty());
        assert_eq!(store.get_storyline("a").await.unwrap(), Some(updated));
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
        assert_eq!(store.list_runs().await.unwrap().len(), 2);

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
            .get_storylines(&["b".into(), "missing".into(), "a".into()])
            .await
            .unwrap();
        assert_eq!(actual, [Some(second), None, Some(first)]);
        assert!(store
            .get_storylines(&["a".into(), "a".into()])
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
        assert_eq!(store.list_runs().await.unwrap().len(), 300);
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
            .list_tool_calls("fixture-dialogue_10")
            .await
            .unwrap()
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
        assert_eq!(store.list_runs().await.unwrap().len(), 1);
        assert!(store.get_storyline("committed").await.unwrap().is_some());
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
    async fn maintenance_compacts_replacement_fragments_and_moves_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(dir.path()).await.unwrap();
        let mut expected = story("a");
        store
            .replace_storylines(&[expected.clone(), story("b")])
            .await
            .unwrap();
        let before = store.current_table_paths().await.unwrap().unwrap();
        for revision in 0..4 {
            expected.notes = Some(format!("revision-{revision}"));
            store.replace_storyline(&expected).await.unwrap();
        }
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
        assert_eq!(store.get_storyline("a").await.unwrap(), Some(expected));
    }

    #[tokio::test]
    async fn empty_store_is_queryable_and_empty_batch_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(dir.path()).await.unwrap();
        assert!(store.current_table_paths().await.unwrap().is_none());
        assert!(store.list_runs().await.unwrap().is_empty());
        assert!(store.list_steps("missing").await.unwrap().is_empty());
        assert!(store.list_tool_calls("missing").await.unwrap().is_empty());
        assert!(store.get_storyline("missing").await.unwrap().is_none());

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
                .get_storyline("remote-1")
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
        assert!(left.get_storyline("left").await.unwrap().is_some());
        assert!(right.current_table_paths().await.unwrap().is_none());
        assert!(right.list_runs().await.unwrap().is_empty());
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
            .list_runs()
            .await
            .unwrap()
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

//! Storyline-native normalized Lance store.
//!
//! Each committed generation contains three immutable datasets. `CURRENT` is
//! atomically replaced only after all tables are durable, so readers never
//! observe a partially updated Storyline.
//!
//! ```text
//! root/
//!   CURRENT
//!   generations/<generation>/
//!     runs.lance/
//!     steps.lance/
//!     tool_calls.lance/
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures::TryStreamExt;
use lance::dataset::{InsertBuilder, WriteMode, WriteParams};
use lance::deps::arrow_array::RecordBatch;
use lance::index::DatasetIndexExt;
use lance::Dataset;
use lance_index::scalar::{BuiltinIndexType, ScalarIndexParams};
use lance_index::IndexType;

use crate::storyline_schema::{
    reconstruct_storyline, split_storyline, StoryRunRow, StoryStepRow, StoryToolCallRow,
    StorylineTables, STORY_RUNS_TABLE, STORY_STEPS_TABLE, STORY_TOOL_CALLS_TABLE,
};
use crate::StorylineDocument;

use super::storyline_lance_rows::{
    story_runs_from_batch, story_runs_to_batch, story_steps_from_batch, story_steps_to_batch,
    story_tool_calls_from_batch, story_tool_calls_to_batch,
};

const CURRENT_FILE: &str = "CURRENT";
const GENERATIONS_DIR: &str = "generations";

fn write_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorylineTablePaths {
    pub generation: String,
    pub runs: PathBuf,
    pub steps: PathBuf,
    pub tool_calls: PathBuf,
}

#[derive(Debug, Clone)]
pub struct LanceStorylineStore {
    root: PathBuf,
}

impl LanceStorylineStore {
    pub async fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        tokio::fs::create_dir_all(root.join(GENERATIONS_DIR))
            .await
            .with_context(|| format!("create Storyline Lance root {}", root.display()))?;
        let store = Self { root };
        // Fail early on a malformed or dangling commit pointer.
        let _ = store.current_table_paths().await?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Paths for the currently committed generation, or `None` for an empty store.
    pub async fn current_table_paths(&self) -> Result<Option<StorylineTablePaths>> {
        let pointer = self.root.join(CURRENT_FILE);
        let generation = match tokio::fs::read_to_string(&pointer).await {
            Ok(value) => value.trim().to_string(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read Storyline commit pointer {}", pointer.display())
                });
            }
        };
        validate_generation_name(&generation)?;
        let paths = self.paths_for_generation(&generation);
        for path in [&paths.runs, &paths.steps, &paths.tool_calls] {
            if !path.is_dir() {
                anyhow::bail!(
                    "Storyline generation '{}' is incomplete: missing {}",
                    generation,
                    path.display()
                );
            }
        }
        Ok(Some(paths))
    }

    pub async fn replace_storyline(&self, story: &StorylineDocument) -> Result<()> {
        self.replace_storylines(std::slice::from_ref(story)).await
    }

    /// Atomically replace multiple Storylines in one generation.
    ///
    /// This is the preferred ingestion path for imports and benchmarks: all
    /// documents are validated before the lock is acquired and indices are
    /// built once for the resulting tables.
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

        let (mut runs, mut steps, mut tool_calls) = self.read_all().await?;
        runs.retain(|row| !session_ids.contains(&row.session_id));
        steps.retain(|row| !session_ids.contains(&row.session_id));
        tool_calls.retain(|row| !session_ids.contains(&row.session_id));
        for replacement in replacements {
            runs.push(replacement.run);
            steps.extend(replacement.steps);
            tool_calls.extend(replacement.tool_calls);
        }
        sort_rows(&mut runs, &mut steps, &mut tool_calls);

        let generation = next_generation();
        let paths = self.paths_for_generation(&generation);
        tokio::fs::create_dir_all(paths.runs.parent().expect("generation parent"))
            .await
            .with_context(|| format!("create Storyline generation {generation}"))?;

        let write_result = async {
            write_batch(
                &paths.runs,
                story_runs_to_batch(&runs)?,
                &[
                    ("session_id", IndexType::BTree),
                    ("run_id", IndexType::BTree),
                ],
            )
            .await?;
            write_batch(
                &paths.steps,
                story_steps_to_batch(&steps)?,
                &[
                    ("session_id", IndexType::BTree),
                    ("step_id", IndexType::BTree),
                    ("effective_kind", IndexType::Bitmap),
                    ("source", IndexType::Bitmap),
                ],
            )
            .await?;
            write_batch(
                &paths.tool_calls,
                story_tool_calls_to_batch(&tool_calls)?,
                &[
                    ("session_id", IndexType::BTree),
                    ("step_id", IndexType::BTree),
                    ("tool_call_id", IndexType::BTree),
                    ("function_name", IndexType::Bitmap),
                ],
            )
            .await?;
            self.commit_generation(&generation).await
        }
        .await;
        if write_result.is_err() {
            let _ =
                tokio::fs::remove_dir_all(paths.runs.parent().expect("generation parent exists"))
                    .await;
        }
        write_result
    }

    pub async fn get_storyline(&self, session_id: &str) -> Result<Option<StorylineDocument>> {
        let (runs, steps, tool_calls) = self.read_all().await?;
        let mut matching_runs = runs.into_iter().filter(|row| row.session_id == session_id);
        let Some(run) = matching_runs.next() else {
            return Ok(None);
        };
        if matching_runs.next().is_some() {
            anyhow::bail!("duplicate runs rows for session_id '{session_id}'");
        }
        let tables = StorylineTables {
            run,
            steps: steps
                .into_iter()
                .filter(|row| row.session_id == session_id)
                .collect(),
            tool_calls: tool_calls
                .into_iter()
                .filter(|row| row.session_id == session_id)
                .collect(),
        };
        reconstruct_storyline(tables)
            .map(Some)
            .map_err(anyhow::Error::from)
    }

    pub async fn list_runs(&self) -> Result<Vec<StoryRunRow>> {
        Ok(self.read_all().await?.0)
    }

    pub async fn list_steps(&self, session_id: &str) -> Result<Vec<StoryStepRow>> {
        Ok(self
            .read_all()
            .await?
            .1
            .into_iter()
            .filter(|row| row.session_id == session_id)
            .collect())
    }

    pub async fn list_tool_calls(&self, session_id: &str) -> Result<Vec<StoryToolCallRow>> {
        Ok(self
            .read_all()
            .await?
            .2
            .into_iter()
            .filter(|row| row.session_id == session_id)
            .collect())
    }

    fn paths_for_generation(&self, generation: &str) -> StorylineTablePaths {
        let base = self.root.join(GENERATIONS_DIR).join(generation);
        StorylineTablePaths {
            generation: generation.to_string(),
            runs: base.join(format!("{STORY_RUNS_TABLE}.lance")),
            steps: base.join(format!("{STORY_STEPS_TABLE}.lance")),
            tool_calls: base.join(format!("{STORY_TOOL_CALLS_TABLE}.lance")),
        }
    }

    async fn read_all(
        &self,
    ) -> Result<(Vec<StoryRunRow>, Vec<StoryStepRow>, Vec<StoryToolCallRow>)> {
        let Some(paths) = self.current_table_paths().await? else {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        };
        let runs = read_batches(&paths.runs)
            .await?
            .iter()
            .map(story_runs_from_batch)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        let steps = read_batches(&paths.steps)
            .await?
            .iter()
            .map(story_steps_from_batch)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        let tool_calls = read_batches(&paths.tool_calls)
            .await?
            .iter()
            .map(story_tool_calls_from_batch)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        Ok((runs, steps, tool_calls))
    }

    async fn commit_generation(&self, generation: &str) -> Result<()> {
        let pointer = self.root.join(CURRENT_FILE);
        let temp = self.root.join(format!(
            ".CURRENT.tmp-{}-{}",
            std::process::id(),
            NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = tokio::fs::File::create(&temp)
            .await
            .with_context(|| format!("create commit pointer {}", temp.display()))?;
        use tokio::io::AsyncWriteExt;
        file.write_all(format!("{generation}\n").as_bytes()).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temp, &pointer)
            .await
            .with_context(|| format!("commit Storyline generation {generation}"))?;
        Ok(())
    }
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

async fn write_batch(path: &Path, batch: RecordBatch, indexes: &[(&str, IndexType)]) -> Result<()> {
    let uri = path.to_string_lossy().into_owned();
    let row_count = batch.num_rows();
    let mut dataset = InsertBuilder::new(&uri)
        .with_params(&WriteParams {
            mode: WriteMode::Create,
            ..Default::default()
        })
        .execute(vec![batch])
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
    Ok(())
}

async fn read_batches(path: &Path) -> Result<Vec<RecordBatch>> {
    let uri = path.to_string_lossy().into_owned();
    let dataset = Dataset::open(&uri)
        .await
        .with_context(|| format!("open Storyline Lance table {}", path.display()))?;
    dataset
        .scan()
        .try_into_stream()
        .await
        .with_context(|| format!("scan Storyline Lance table {}", path.display()))?
        .try_collect()
        .await
        .with_context(|| format!("read Storyline Lance table {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StorylineAgent, StorylineToolCall, StorylineTurn, STORYLINE_SCHEMA_VERSION};

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
        let store = LanceStorylineStore::open(dir.path()).await.unwrap();
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
        let store = LanceStorylineStore::open(dir.path()).await.unwrap();
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
        let store = LanceStorylineStore::open(dir.path()).await.unwrap();
        store.replace_storyline(&story("a")).await.unwrap();
        let first = store
            .current_table_paths()
            .await
            .unwrap()
            .unwrap()
            .generation;
        store.replace_storyline(&story("b")).await.unwrap();
        let second = store
            .current_table_paths()
            .await
            .unwrap()
            .unwrap()
            .generation;
        assert_ne!(first, second);
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
        let store = LanceStorylineStore::open(dir.path()).await.unwrap();
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
    async fn invalid_result_does_not_move_current_generation() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStorylineStore::open(dir.path()).await.unwrap();
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
    async fn empty_store_is_queryable_and_empty_batch_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStorylineStore::open(dir.path()).await.unwrap();
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
        let error = LanceStorylineStore::open(invalid.path()).await.unwrap_err();
        assert!(error.to_string().contains("invalid Storyline generation"));

        let incomplete = tempfile::tempdir().unwrap();
        tokio::fs::write(incomplete.path().join(CURRENT_FILE), "gen-missing\n")
            .await
            .unwrap();
        let error = LanceStorylineStore::open(incomplete.path())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("is incomplete"));
    }
}

//! Rebuildable browse index. Accurate queries never use this index to establish
//! source membership or revisions: an old index may omit newly created sources.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures::TryStreamExt;
use lance::Dataset;
use lance::dataset::{InsertBuilder, MergeInsertBuilder, WhenMatched, WhenNotMatched};
use lance::deps::arrow_array::{Array, RecordBatch, RecordBatchIterator, StringArray};
use lance::deps::arrow_schema::{DataType, Field, Schema};
use persisting_pchronicle::storage::{DatasetLocation, DatasetMount};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, mpsc, oneshot};

use super::explorer::{CatalogTree, catalog_tree_from_mount_specs, catalog_tree_from_path_list};

const CACHE_SCHEMA_VERSION: &str = "ui-tree-v2";
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const QUEUE_CAPACITY: usize = 128;
// ponytail: one process-wide browse scan at a time; per-backend budgets if
// multiple independent stores need more throughput. This does not gate SQL.
static BROWSE_IO: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct TreeKey {
    dataset: String,
    uri_fingerprint: String,
    prefix: String,
}

impl TreeKey {
    fn new(mount: &DatasetMount, prefix: &str) -> Result<Self> {
        let prefix = prefix.trim().trim_matches('/');
        anyhow::ensure!(
            !prefix
                .split('/')
                .any(|part| matches!(part, "." | "..") || part.contains('\\')),
            "invalid browse prefix"
        );
        Ok(Self {
            dataset: mount.name.clone(),
            uri_fingerprint: mount_fingerprint(mount),
            prefix: prefix.to_owned(),
        })
    }
}

fn mount_fingerprint(mount: &DatasetMount) -> String {
    let location = DatasetLocation::parse(&mount.uri).ok();
    let identity = location
        .as_ref()
        .and_then(|l| l.local_path())
        .map(|path| {
            std::path::absolute(path)
                .unwrap_or_else(|_| path.to_path_buf())
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| mount.uri.trim_end_matches('/').to_owned());
    // S3-compatible endpoints can expose different data under the same URI.
    let endpoint = if mount.uri.starts_with("s3://") {
        std::env::var("AWS_ENDPOINT_URL_S3")
            .or_else(|_| std::env::var("AWS_ENDPOINT_URL"))
            .or_else(|_| std::env::var("AWS_ENDPOINT"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    blake3::hash(&serde_json::to_vec(&(identity, endpoint)).unwrap())
        .to_hex()
        .to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct IndexEntry {
    tree: CatalogTree,
    /// Digest of the observed directory view, NOT a pinned source revision.
    generation: String,
    observed_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BrowseSnapshot {
    // Preserve existing Tree wire fields for old Web clients.
    #[serde(flatten)]
    tree: CatalogTree,
    browse: BrowseStatus,
}

#[derive(Clone, Debug, Serialize)]
struct BrowseStatus {
    consistency: &'static str,
    generation: String,
    observed_at: i64,
    stale: bool,
    refreshing: bool,
    last_error: Option<String>,
}

#[derive(Default)]
struct RefreshState {
    failures: u32,
    retry_at: Option<Instant>,
    error: Option<String>,
}

type Reply = oneshot::Sender<std::result::Result<(), String>>;
type Pending = Arc<Mutex<HashMap<TreeKey, Vec<Reply>>>>;

/// One bounded worker per serve instance, shared by timer and HTTP requests.
/// The task owns the index, not the coordinator; dropping the last AppState
/// aborts it, including an in-flight list. No permanent process singleton.
pub(crate) struct BrowseCoordinator {
    index: Arc<CatalogIndex>,
    pending: Pending,
    refresh: Arc<Mutex<HashMap<TreeKey, RefreshState>>>,
    sender: mpsc::Sender<TreeKey>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for BrowseCoordinator {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl BrowseCoordinator {
    pub(crate) async fn start(mounts: Vec<DatasetMount>) -> Self {
        let root = std::env::var_os("PCHRONICLE_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::cache_dir().map(|path| path.join("pchronicle")))
            .unwrap_or_else(|| std::env::temp_dir().join("pchronicle"));
        Self::start_at(mounts, root).await
    }

    pub(super) async fn start_at(mounts: Vec<DatasetMount>, root: PathBuf) -> Self {
        // Stable across mount order; isolate same-name mounts with different URIs.
        let mut identities: Vec<_> = mounts
            .iter()
            .map(|m| (m.name.clone(), mount_fingerprint(m)))
            .collect();
        identities.sort();
        let namespace = blake3::hash(&serde_json::to_vec(&identities).unwrap()).to_hex();
        let index =
            Arc::new(CatalogIndex::open(root.join(format!("catalog-{namespace}.lance"))).await);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let refresh = Arc::new(Mutex::new(HashMap::new()));
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let task = tokio::spawn(run_worker(
            mounts,
            index.clone(),
            pending.clone(),
            refresh.clone(),
            receiver,
        ));
        Self {
            index,
            pending,
            refresh,
            sender,
            task,
        }
    }

    pub(crate) async fn cached_dataset(&self, mount: &DatasetMount) -> Option<CatalogTree> {
        let key = TreeKey::new(mount, "").ok()?;
        self.index
            .values
            .read()
            .await
            .get(&key)
            .map(|entry| entry.tree.clone())
    }

    pub(crate) async fn cached_source_paths(&self, mount: &DatasetMount) -> Vec<String> {
        let values = self.index.values.read().await;
        let fingerprint = mount_fingerprint(mount);
        let mut paths = values
            .iter()
            .filter(|(key, _)| key.dataset == mount.name && key.uri_fingerprint == fingerprint)
            .flat_map(|(_, entry)| entry.tree.children.iter())
            .filter(|child| child.kind == "file")
            .map(|child| child.path.clone())
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }

    pub(crate) async fn roots(&self, mounts: &[DatasetMount]) -> BrowseSnapshot {
        let mut tree = catalog_tree_from_mount_specs(mounts);
        let values = self.index.values.read().await;
        let mut observed_at = now();
        let mut complete = true;
        let mut refreshing = false;
        let mut error = None;
        for mount in mounts {
            let key = TreeKey::new(mount, "").expect("root prefix");
            let _ = self.enqueue(&key, None);
            refreshing |= self.pending.lock().unwrap().contains_key(&key);
            if let Some(failure) = self
                .refresh
                .lock()
                .unwrap()
                .get(&key)
                .and_then(|s| s.error.clone())
            {
                error = Some(failure);
            }
            if let Some(entry) = values.get(&key) {
                observed_at = observed_at.min(entry.observed_at);
                if let Some(child) = tree.children.iter_mut().find(|c| c.name == mount.name) {
                    child.run_count = entry.tree.run_count;
                    child.failed_count = entry.tree.failed_count;
                }
            } else {
                complete = false;
            }
        }
        tree.run_count = tree
            .children
            .iter()
            .fold(0usize, |sum, c| sum.saturating_add(c.run_count));
        tree.failed_count = tree
            .children
            .iter()
            .fold(0usize, |sum, c| sum.saturating_add(c.failed_count));
        BrowseSnapshot {
            browse: BrowseStatus {
                consistency: "best_effort",
                generation: blake3::hash(&serde_json::to_vec(&tree).unwrap())
                    .to_hex()
                    .to_string(),
                observed_at: if complete { observed_at } else { 0 },
                stale: !complete || error.is_some() || now() - observed_at >= 30,
                refreshing,
                last_error: error,
            },
            tree,
        }
    }

    pub(crate) async fn tree(&self, mount: &DatasetMount, prefix: &str) -> Result<BrowseSnapshot> {
        let key = TreeKey::new(mount, prefix)?;
        let existing = self.index.values.read().await.get(&key).cloned();
        if let Some(entry) = existing {
            let _ = self.enqueue(&key, None);
            return Ok(self.snapshot(&key, entry));
        }
        let (reply, wait) = oneshot::channel();
        self.enqueue(&key, Some(reply))?;
        // Bound cold requests even if many prefixes precede them in the queue.
        tokio::time::timeout(Duration::from_secs(30), wait)
            .await
            .context("browse refresh timed out")?
            .context("browse worker stopped")?
            .map_err(anyhow::Error::msg)?;
        let entry = self
            .index
            .values
            .read()
            .await
            .get(&key)
            .cloned()
            .context("browse refresh produced no view")?;
        Ok(self.snapshot(&key, entry))
    }

    fn enqueue(&self, key: &TreeKey, reply: Option<Reply>) -> Result<()> {
        if let Some(state) = self.refresh.lock().unwrap().get(key)
            && state
                .retry_at
                .is_some_and(|deadline| deadline > Instant::now())
        {
            if let Some(error) = &state.error {
                anyhow::bail!("{error}");
            }
            return Ok(());
        }
        let mut pending = self.pending.lock().unwrap();
        if let Some(waiters) = pending.get_mut(key) {
            waiters.retain(|reply| !reply.is_closed());
            if let Some(reply) = reply {
                anyhow::ensure!(waiters.len() < QUEUE_CAPACITY, "too many browse waiters");
                waiters.push(reply);
            }
        } else {
            self.sender
                .try_send(key.clone())
                .context("browse refresh queue full or stopped")?;
            pending.insert(key.clone(), reply.into_iter().collect());
        }
        Ok(())
    }

    fn snapshot(&self, key: &TreeKey, entry: IndexEntry) -> BrowseSnapshot {
        let error = self
            .refresh
            .lock()
            .unwrap()
            .get(key)
            .and_then(|s| s.error.clone());
        BrowseSnapshot {
            tree: entry.tree,
            browse: BrowseStatus {
                consistency: "best_effort",
                generation: entry.generation,
                observed_at: entry.observed_at,
                stale: error.is_some()
                    || now() - entry.observed_at >= REFRESH_INTERVAL.as_secs() as i64,
                refreshing: self.pending.lock().unwrap().contains_key(key),
                last_error: error,
            },
        }
    }
}

async fn run_worker(
    mounts: Vec<DatasetMount>,
    index: Arc<CatalogIndex>,
    pending: Pending,
    states: Arc<Mutex<HashMap<TreeKey, RefreshState>>>,
    mut receiver: mpsc::Receiver<TreeKey>,
) {
    let mut interval = tokio::time::interval(REFRESH_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Incremental round over roots and previously browsed prefixes. Foreground
    // requests are checked between each scan, not after the entire mount set.
    let mut background = Vec::new();
    let mut visited = HashSet::new();
    loop {
        let key = tokio::select! {
            biased;
            request = receiver.recv() => match request { Some(key) => key, None => break },
            _ = interval.tick() => {
                if background.is_empty() {
                    visited.clear();
                    background.extend(mounts.iter().filter_map(|m| TreeKey::new(m, "").ok()));
                    visited.extend(background.iter().cloned());
                }
                continue;
            }
            _ = std::future::ready(()), if !background.is_empty() => background.pop().unwrap(),
        };
        let Some(mount) = mounts
            .iter()
            .find(|m| TreeKey::new(m, &key.prefix).ok().as_ref() == Some(&key))
        else {
            continue;
        };
        if states
            .lock()
            .unwrap()
            .get(&key)
            .is_some_and(|s| s.retry_at.is_some_and(|deadline| deadline > Instant::now()))
        {
            finish(&pending, &key, Ok(()));
            continue;
        }
        pending.lock().unwrap().entry(key.clone()).or_default();
        let result = async {
            let _permit = BROWSE_IO.acquire().await?;
            // Bound prefix-list start rate as well as concurrency. One list may
            // contain several storage requests; backend I/O gates still apply.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let location = DatasetLocation::parse(&mount.uri)?;
            if let Some(root) = location.local_path() {
                // Navigation's list API treats a missing path as empty. For a
                // cached view that could erase an offline mount's descendants.
                // Deletions are instead established by a successful parent list.
                tokio::fs::metadata(root.join(&key.prefix))
                    .await
                    .context("browse path unavailable")?;
            }
            let entries = tokio::time::timeout(Duration::from_secs(20), location.list(&key.prefix))
                .await
                .context("browse list timed out")??;
            let tree = catalog_tree_from_path_list(&mount.name, &key.prefix, &entries);
            // Walk only navigational directories; a Dataset leaf is opaque.
            // A bounded frontier prevents the background walk growing without limit.
            visited.insert(key.clone());
            for child in &tree.children {
                if child.kind == "dir" && visited.len() < 10_000 {
                    let next = TreeKey::new(mount, &child.path)?;
                    if visited.insert(next.clone()) {
                        background.push(next);
                    }
                }
            }
            index.put(key.clone(), tree).await;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        let outcome = match result {
            Ok(()) => {
                states.lock().unwrap().insert(
                    key.clone(),
                    RefreshState {
                        retry_at: Some(Instant::now() + Duration::from_secs(2)),
                        ..Default::default()
                    },
                );
                Ok(())
            }
            Err(error) => {
                let error = format!("{error:#}");
                let cached_view = index.values.read().await.contains_key(&key);
                let mut states = states.lock().unwrap();
                let state = states.entry(key.clone()).or_default();
                state.failures = state.failures.saturating_add(1);
                state.retry_at = Some(
                    Instant::now()
                        + Duration::from_secs((30u64 * (1u64 << state.failures.min(4))).min(300)),
                );
                state.error = Some(error.clone());
                tracing::warn!(target: "pchronicle.serve", dataset = %key.dataset, prefix = %key.prefix, cached_view, error = %error, "browse refresh failed");
                Err(error)
            }
        };
        finish(&pending, &key, outcome);
    }
}

fn finish(pending: &Pending, key: &TreeKey, outcome: std::result::Result<(), String>) {
    if let Some(waiters) = pending.lock().unwrap().remove(key) {
        for waiter in waiters {
            let _ = waiter.send(outcome.clone());
        }
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Disk is optional: corruption or a competing process never prevents browsing.
struct CatalogIndex {
    path: PathBuf,
    values: RwLock<HashMap<TreeKey, IndexEntry>>,
    // Hold an advisory lock for the writer lifetime, including corruption repair.
    // A second process uses memory only instead of deleting/writing active data.
    _disk_lock: Option<std::fs::File>,
}

impl CatalogIndex {
    async fn open(path: PathBuf) -> Self {
        let lock = (|| -> Result<std::fs::File> {
            std::fs::create_dir_all(path.parent().context("cache parent")?)?;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path.with_extension("lock"))?;
            fs2::FileExt::try_lock_exclusive(&file)?;
            Ok(file)
        })();
        let mut index = Self {
            path,
            values: RwLock::new(HashMap::new()),
            _disk_lock: lock.ok(),
        };
        if index._disk_lock.is_none() {
            return index;
        }
        match index.load().await {
            Ok(values) => *index.values.get_mut() = values,
            Err(error) => {
                tracing::warn!(target: "pchronicle.serve", error = %error, "browse cache unreadable; rebuilding");
                let removed = if index.path.is_dir() {
                    tokio::fs::remove_dir_all(&index.path).await
                } else {
                    tokio::fs::remove_file(&index.path).await
                };
                if removed.is_err() && index.path.exists() {
                    // No repeated broken-dataset writes if repair is impossible.
                    index._disk_lock = None;
                }
            }
        }
        index
    }

    async fn put(&self, key: TreeKey, tree: CatalogTree) {
        let generation = blake3::hash(&serde_json::to_vec(&tree).unwrap())
            .to_hex()
            .to_string();
        let entry = IndexEntry {
            tree,
            generation,
            observed_at: now(),
        };
        let mut values = self.values.write().await;
        // Only a successful complete list can remove descendants. A failed or
        // timed-out list never calls put, so it cannot erase a previous view.
        let prefix = if key.prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", key.prefix)
        };
        let removed: Vec<_> = values
            .keys()
            .filter(|other| {
                other.dataset == key.dataset
                    && other.uri_fingerprint == key.uri_fingerprint
                    && *other != &key
                    && other.prefix.starts_with(&prefix)
                    && !entry.tree.children.iter().any(|child| {
                        child.path != "."
                            && (other.prefix == child.path
                                || other.prefix.starts_with(&format!("{}/", child.path)))
                    })
            })
            .cloned()
            .collect();
        for removed_key in &removed {
            values.remove(removed_key);
        }
        let changed = values
            .get(&key)
            .is_none_or(|previous| previous.generation != entry.generation);
        values.insert(key.clone(), entry.clone());
        drop(values);
        // Unchanged directory observations update memory without creating a
        // new Lance version every 30 seconds. On restart the older timestamp
        // conservatively marks the persisted view stale until revalidated.
        if self._disk_lock.is_some()
            && (changed || !removed.is_empty())
            && let Err(error) = self.persist(&key, &entry, &removed).await
        {
            tracing::warn!(target: "pchronicle.serve", error = %error, "browse cache persistence failed; using memory");
        }
    }

    async fn load(&self) -> Result<HashMap<TreeKey, IndexEntry>> {
        if !self.path.exists() {
            return Ok(HashMap::new());
        }
        let dataset = Dataset::open(self.path.to_string_lossy().as_ref()).await?;
        let batches: Vec<RecordBatch> = dataset
            .scan()
            .try_into_stream()
            .await?
            .try_collect()
            .await?;
        let mut values = HashMap::new();
        for batch in batches {
            let keys = text_column(&batch, "key")?;
            let payloads = text_column(&batch, "payload")?;
            let versions = text_column(&batch, "schema_version")?;
            for row in 0..batch.num_rows() {
                anyhow::ensure!(
                    versions[row] == CACHE_SCHEMA_VERSION,
                    "unsupported browse cache schema"
                );
                values.insert(
                    serde_json::from_str(&keys[row])?,
                    serde_json::from_str(&payloads[row])?,
                );
            }
        }
        Ok(values)
    }

    async fn persist(&self, key: &TreeKey, entry: &IndexEntry, removed: &[TreeKey]) -> Result<()> {
        let schema = Arc::new(Schema::new(vec![
            Field::new("key", DataType::Utf8, false),
            Field::new("payload", DataType::Utf8, false),
            Field::new("schema_version", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![serde_json::to_string(key)?])) as _,
                Arc::new(StringArray::from(vec![serde_json::to_string(entry)?])) as _,
                Arc::new(StringArray::from(vec![CACHE_SCHEMA_VERSION])) as _,
            ],
        )?;
        if self.path.exists() {
            let mut dataset = Dataset::open(self.path.to_string_lossy().as_ref()).await?;
            for chunk in removed.chunks(128) {
                let keys = chunk
                    .iter()
                    .map(|key| {
                        Ok(format!(
                            "'{}'",
                            serde_json::to_string(key)?.replace("'", "''")
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?
                    .join(",");
                dataset.delete(&format!("key IN ({keys})")).await?;
            }
            let reader = Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
            MergeInsertBuilder::try_new(Arc::new(dataset), vec!["key".into()])?
                .when_matched(WhenMatched::UpdateAll)
                .when_not_matched(WhenNotMatched::InsertAll)
                .try_build()?
                .execute_reader(reader)
                .await?;
        } else {
            InsertBuilder::new(self.path.to_string_lossy().as_ref())
                .execute(vec![batch])
                .await?;
        }
        Ok(())
    }
}

fn text_column(batch: &RecordBatch, name: &str) -> Result<Vec<String>> {
    let array = batch
        .column(batch.schema().index_of(name)?)
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("browse cache column {name} must be Utf8"))?;
    anyhow::ensure!(array.null_count() == 0, "null browse cache field");
    Ok((0..array.len())
        .map(|index| array.value(index).to_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(path: &std::path::Path) -> DatasetMount {
        DatasetMount::new("test", path.to_string_lossy()).unwrap()
    }

    #[tokio::test]
    async fn corrupt_cache_is_rebuilt_and_persisted() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.lance");
        std::fs::write(&path, "broken cache").unwrap();
        let index = CatalogIndex::open(path.clone()).await;
        let key = TreeKey::new(&mount(temp.path()), "").unwrap();
        index.put(key.clone(), CatalogTree::default()).await;
        assert!(path.is_dir());
        assert_eq!(index.values.read().await.len(), 1);
        drop(index);
        let reloaded = CatalogIndex::open(path).await;
        assert!(reloaded.values.read().await.contains_key(&key));
    }

    #[tokio::test]
    async fn unsupported_schema_is_rebuilt_without_partial_rows() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.lance");
        let schema = Arc::new(Schema::new(vec![Field::new(
            "unrecognized",
            DataType::Utf8,
            false,
        )]));
        let batch =
            RecordBatch::try_new(schema, vec![Arc::new(StringArray::from(vec!["old"]))]).unwrap();
        InsertBuilder::new(path.to_string_lossy().as_ref())
            .execute(vec![batch])
            .await
            .unwrap();
        let index = CatalogIndex::open(path.clone()).await;
        assert!(index.values.read().await.is_empty());
        assert!(!path.exists());
        index
            .put(
                TreeKey::new(&mount(temp.path()), "").unwrap(),
                CatalogTree::default(),
            )
            .await;
        assert!(Dataset::open(path.to_string_lossy().as_ref()).await.is_ok());
    }

    #[tokio::test]
    async fn cache_namespace_isolates_uris_and_drop_stops_worker() {
        let temp = tempfile::tempdir().unwrap();
        let first =
            BrowseCoordinator::start_at(vec![mount(&temp.path().join("a"))], temp.path().into())
                .await;
        let second =
            BrowseCoordinator::start_at(vec![mount(&temp.path().join("b"))], temp.path().into())
                .await;
        assert_ne!(first.index.path, second.index.path);
        let weak = Arc::downgrade(&first.index);
        drop(first);
        tokio::time::timeout(Duration::from_secs(2), async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!second.task.is_finished());
    }

    #[tokio::test]
    async fn repeated_requests_share_one_refresh_and_error_retains_view() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        let mount = mount(&source);
        let coordinator =
            BrowseCoordinator::start_at(vec![mount.clone()], temp.path().join("cache")).await;
        // Hold the global I/O permit to make duplicate cold requests observable.
        let gate = BROWSE_IO.acquire().await.unwrap();
        let key = TreeKey::new(&mount, "").unwrap();
        let (a, ar) = oneshot::channel();
        let (b, br) = oneshot::channel();
        coordinator.enqueue(&key, Some(a)).unwrap();
        coordinator.enqueue(&key, Some(b)).unwrap();
        assert_eq!(coordinator.pending.lock().unwrap().len(), 1);
        assert_eq!(coordinator.pending.lock().unwrap()[&key].len(), 2);
        drop(gate);
        ar.await.unwrap().unwrap();
        br.await.unwrap().unwrap();
        let old = coordinator.tree(&mount, "").await.unwrap();
        assert_eq!(old.tree.children[0].name, "nested");
        std::fs::remove_dir_all(&source).unwrap();
        coordinator.refresh.lock().unwrap().remove(&key);
        let (reply, wait) = oneshot::channel();
        coordinator.enqueue(&key, Some(reply)).unwrap();
        assert!(wait.await.unwrap().is_err());
        let stale = coordinator.tree(&mount, "").await.unwrap();
        assert_eq!(stale.tree, old.tree);
        assert!(stale.browse.stale);
        assert!(stale.browse.last_error.is_some());
    }

    #[tokio::test]
    async fn successful_parent_refresh_removes_deleted_descendants_on_disk() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.lance");
        let index = CatalogIndex::open(path.clone()).await;
        let mount = mount(temp.path());
        let child = TreeKey::new(&mount, "gone/nested").unwrap();
        index.put(child.clone(), CatalogTree::default()).await;
        index
            .put(TreeKey::new(&mount, "").unwrap(), CatalogTree::default())
            .await;
        assert!(!index.values.read().await.contains_key(&child));
        drop(index);
        let reloaded = CatalogIndex::open(path).await;
        assert!(!reloaded.values.read().await.contains_key(&child));
    }

    #[tokio::test]
    async fn background_discovers_unvisited_directories() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(source.join("a/b")).unwrap();
        let mount = mount(&source);
        let coordinator =
            BrowseCoordinator::start_at(vec![mount.clone()], temp.path().join("cache")).await;
        let key = TreeKey::new(&mount, "a/b").unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !coordinator.index.values.read().await.contains_key(&key) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn unchanged_observations_do_not_create_lance_versions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.lance");
        let index = CatalogIndex::open(path.clone()).await;
        let key = TreeKey::new(&mount(temp.path()), "").unwrap();
        index.put(key.clone(), CatalogTree::default()).await;
        let version = Dataset::open(path.to_string_lossy().as_ref())
            .await
            .unwrap()
            .version()
            .version;
        index.put(key, CatalogTree::default()).await;
        assert_eq!(
            Dataset::open(path.to_string_lossy().as_ref())
                .await
                .unwrap()
                .version()
                .version,
            version
        );
    }

    #[tokio::test]
    async fn second_writer_does_not_remove_or_mutate_owned_cache() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.lance");
        let first = CatalogIndex::open(path.clone()).await;
        let second = CatalogIndex::open(path.clone()).await;
        assert!(first._disk_lock.is_some());
        assert!(second._disk_lock.is_none());
        second
            .put(
                TreeKey::new(&mount(temp.path()), "").unwrap(),
                CatalogTree::default(),
            )
            .await;
        assert!(!path.exists());
    }
}

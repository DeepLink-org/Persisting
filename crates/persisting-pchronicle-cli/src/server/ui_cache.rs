//! Rebuildable browse index. Accurate queries never use this index to establish
//! source membership or revisions: an old index may omit newly created sources.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use persisting_pchronicle::storage::{
    CatalogConsistency, CatalogState, DatasetLocation, DatasetMount, ManifestCache,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, mpsc, oneshot};

use super::explorer::{CatalogTree, catalog_tree_from_mount_specs, catalog_tree_from_path_list};

const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const FOREGROUND_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const QUEUE_CAPACITY: usize = 128;
// Keep browse work bounded while allowing a foreground request to run beside
// one background walk.
static BROWSE_IO: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
static BACKGROUND_IO: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

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
    pub(super) tree: CatalogTree,
    pub(super) browse: BrowseStatus,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct BrowseStatus {
    partial: bool,
    consistency: CatalogConsistency,
    state: CatalogState,
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

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One bounded worker per serve instance, shared by timer and HTTP requests.
/// The task owns the index, not the coordinator; dropping the last AppState
/// aborts it, including an in-flight list. No permanent process singleton.
pub(crate) struct BrowseCoordinator {
    index: Arc<BrowseTreeProjection>,
    manifests: Arc<ManifestCache>,
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
        let index = Arc::new(
            BrowseTreeProjection::open(root.join(format!("catalog-{namespace}.lance"))).await,
        );
        let manifests =
            Arc::new(ManifestCache::open(root.join(format!("manifest-{namespace}.lance"))).await);
        tracing::info!(
            target: "pchronicle.serve",
            mounts = mounts.len(),
            cache_root = %root.display(),
            "catalog cache worker started"
        );
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let refresh = Arc::new(Mutex::new(HashMap::new()));
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let task = tokio::spawn(run_worker(
            mounts,
            index.clone(),
            manifests.clone(),
            pending.clone(),
            refresh.clone(),
            receiver,
        ));
        Self {
            index,
            manifests,
            pending,
            refresh,
            sender,
            task,
        }
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

    pub(crate) async fn cached_dataset(&self, mount: &DatasetMount) -> Option<CatalogTree> {
        let key = TreeKey::new(mount, "").ok()?;
        self.index
            .values
            .read()
            .await
            .get(&key)
            .map(|entry| entry.tree.clone())
    }

    pub(crate) async fn roots(&self, mounts: &[DatasetMount]) -> BrowseSnapshot {
        let mut tree = catalog_tree_from_mount_specs(mounts);
        let values = self.index.values.read().await;
        let mut observed_at = now();
        let mut complete = true;
        let mut partial = false;
        let mut refreshing = false;
        let mut error = None;
        for mount in mounts {
            let key = TreeKey::new(mount, "").expect("root prefix");
            let _ = self.enqueue(&key, None);
            refreshing |= lock_recover(&self.pending).contains_key(&key);
            if let Some(failure) = lock_recover(&self.refresh)
                .get(&key)
                .and_then(|s| s.error.clone())
            {
                error = Some(failure);
            }
            partial |= projection_is_partial(&values, &key);
            if let Some(entry) = values.get(&key) {
                observed_at = observed_at.min(entry.observed_at);
                if let Some(child) = tree.children.iter_mut().find(|c| c.name == mount.name) {
                    child.run_count = entry.tree.run_count;
                    child.failed_count = entry.tree.failed_count;
                }
            } else {
                complete = false;
            }
            let summary = self
                .manifests
                .summary(&format!("{}\0{}", mount.name, mount_fingerprint(mount)))
                .await;
            partial |= summary.partial;
            if let Some(child) = tree.children.iter_mut().find(|c| c.name == mount.name) {
                child.run_count = summary.trajectories as usize;
                child.dataset_count = Some(summary.datasets as usize);
                child.trajectory_count = Some(summary.trajectories as usize);
            }
        }
        tree.run_count = tree
            .children
            .iter()
            .fold(0usize, |sum, c| sum.saturating_add(c.run_count));
        tree.dataset_count = Some(
            tree.children
                .iter()
                .map(|child| child.dataset_count.unwrap_or_default())
                .sum(),
        );
        tree.trajectory_count = Some(tree.run_count);
        tree.failed_count = tree
            .children
            .iter()
            .fold(0usize, |sum, c| sum.saturating_add(c.failed_count));
        BrowseSnapshot {
            browse: BrowseStatus {
                partial,
                consistency: CatalogConsistency::BestEffort,
                state: if refreshing {
                    CatalogState::Refreshing
                } else if partial {
                    CatalogState::Partial
                } else if !complete && error.is_some() {
                    CatalogState::Unavailable
                } else if !complete || error.is_some() || now() - observed_at >= 30 {
                    CatalogState::Stale
                } else {
                    CatalogState::Ready
                },
                generation: blake3::hash(&serde_json::to_vec(&tree).unwrap())
                    .to_hex()
                    .to_string(),
                observed_at: if complete { observed_at } else { 0 },
                stale: complete && (error.is_some() || now() - observed_at >= 30),
                refreshing,
                last_error: error,
            },
            tree,
        }
    }

    pub(crate) async fn tree(&self, mount: &DatasetMount, prefix: &str) -> Result<BrowseSnapshot> {
        let key = TreeKey::new(mount, prefix)?;
        tracing::info!(
            target: "pchronicle.serve",
            dataset = %key.dataset,
            prefix = %key.prefix,
            "browse tree request"
        );
        let existing = self.index.values.read().await.get(&key).cloned();
        if let Some(entry) = existing {
            let _ = self.enqueue(&key, None);
            return Ok(self.snapshot_with_summary(&key, entry).await);
        }
        // A cold page is user-visible work: attach to the same single-flight
        // refresh as the background walker and return once this prefix exists.
        let (reply, wait) = oneshot::channel();
        self.enqueue(&key, Some(reply))?;
        match tokio::time::timeout(FOREGROUND_REFRESH_TIMEOUT, wait).await {
            Ok(result) => result
                .context("browse refresh task stopped")?
                .map_err(anyhow::Error::msg)?,
            Err(_) => {
                let tree = catalog_tree_from_path_list(&key.dataset, &key.prefix, &[]);
                return Ok(BrowseSnapshot {
                    tree,
                    browse: BrowseStatus {
                        partial: true,
                        consistency: CatalogConsistency::BestEffort,
                        state: CatalogState::Refreshing,
                        generation: String::new(),
                        observed_at: 0,
                        stale: true,
                        refreshing: true,
                        last_error: Some("remote browse is still loading".into()),
                    },
                });
            }
        }
        let entry = self
            .index
            .values
            .read()
            .await
            .get(&key)
            .cloned()
            .context("browse refresh completed without a cached view")?;
        Ok(self.snapshot_with_summary(&key, entry).await)
    }

    fn enqueue(&self, key: &TreeKey, reply: Option<Reply>) -> Result<()> {
        if let Some(state) = lock_recover(&self.refresh).get(key)
            && state
                .retry_at
                .is_some_and(|deadline| deadline > Instant::now())
        {
            if let Some(error) = &state.error {
                anyhow::bail!("{error}");
            }
            return Ok(());
        }
        let mut pending = lock_recover(&self.pending);
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
        let error = lock_recover(&self.refresh)
            .get(key)
            .and_then(|s| s.error.clone());
        BrowseSnapshot {
            tree: entry.tree,
            browse: BrowseStatus {
                partial: false,
                consistency: CatalogConsistency::BestEffort,
                state: if lock_recover(&self.pending).contains_key(key) {
                    CatalogState::Refreshing
                } else if error.is_some()
                    || now() - entry.observed_at >= REFRESH_INTERVAL.as_secs() as i64
                {
                    CatalogState::Stale
                } else {
                    CatalogState::Ready
                },
                generation: entry.generation,
                observed_at: entry.observed_at,
                stale: error.is_some()
                    || now() - entry.observed_at >= REFRESH_INTERVAL.as_secs() as i64,
                refreshing: lock_recover(&self.pending).contains_key(key),
                last_error: error,
            },
        }
    }

    async fn snapshot_with_summary(&self, key: &TreeKey, mut entry: IndexEntry) -> BrowseSnapshot {
        let current_manifest_key = manifest_key(key, None);
        let summary = self.manifests.summary_under(&current_manifest_key).await;
        entry.tree.dataset_count = Some(summary.datasets as usize);
        entry.tree.trajectory_count = Some(summary.trajectories as usize);
        entry.tree.run_count = summary.trajectories as usize;
        let cached_trees = self
            .index
            .values
            .read()
            .await
            .values()
            .map(|entry| entry.tree.clone())
            .collect::<Vec<_>>();
        let (cached_datasets, cached_trajectories) = cached_leaf_summary(&cached_trees, "");
        if cached_datasets > 0 {
            entry.tree.dataset_count = Some(cached_datasets);
            entry.tree.trajectory_count = Some(cached_trajectories);
            entry.tree.run_count = cached_trajectories;
        }
        for child in &mut entry.tree.children {
            if child.kind != "dir" {
                continue;
            }
            let child_prefix = if key.prefix.is_empty()
                || child.path == key.prefix
                || child.path.starts_with(&format!("{}/", key.prefix))
            {
                child.path.clone()
            } else {
                format!("{}/{}", key.prefix, child.path)
            };
            let summary = self
                .manifests
                .summary_under(&manifest_key(key, Some(&child_prefix)))
                .await;
            child.dataset_count = (summary.datasets > 0).then_some(summary.datasets as usize);
            child.trajectory_count =
                (summary.trajectories > 0).then_some(summary.trajectories as usize);
            let child_key = TreeKey {
                dataset: key.dataset.clone(),
                uri_fingerprint: key.uri_fingerprint.clone(),
                prefix: child_prefix.trim_matches('/').to_owned(),
            };
            // Child discovery belongs to the bounded background walk. Rendering
            // a wide directory must not promote every child to foreground work.
            let cached = self.index.values.read().await.get(&child_key).cloned();
            tracing::info!(
                target: "pchronicle.serve",
                dataset = %key.dataset,
                parent_prefix = %key.prefix,
                child_prefix = %child.path,
                manifest_datasets = summary.datasets,
                manifest_trajectories = summary.trajectories,
                projection_hit = cached.is_some(),
                "catalog directory summary"
            );
            if let Some(cached) = cached {
                // Remote parents initially contain names only. Reuse the child's
                // own leaf observation without another request to S3.
                if let Some(leaf) = cached
                    .tree
                    .children
                    .iter()
                    .find(|leaf| leaf.kind == "file" && leaf.path == child_prefix)
                {
                    let name = child.name.clone();
                    *child = leaf.clone();
                    child.name = name;
                    continue;
                }
                if let Some(count) = cached.tree.dataset_count.filter(|count| *count > 0) {
                    child.dataset_count = Some(count);
                }
                if let Some(count) = cached
                    .tree
                    .trajectory_count
                    .or(Some(cached.tree.run_count))
                    .filter(|count| *count > 0)
                {
                    child.trajectory_count = Some(count);
                }
            }
            let (datasets, trajectories) = cached_leaf_summary(&cached_trees, &child_prefix);
            if datasets > 0 {
                child.dataset_count = Some(datasets);
                child.trajectory_count = Some(trajectories);
            }
        }
        let child_summary =
            entry
                .tree
                .children
                .iter()
                .fold((0usize, 0usize), |(datasets, trajectories), child| {
                    let datasets = datasets.saturating_add(
                        child
                            .dataset_count
                            .unwrap_or_else(|| (child.kind == "dataset") as usize),
                    );
                    let trajectories = trajectories
                        .saturating_add(child.trajectory_count.unwrap_or(child.run_count));
                    (datasets, trajectories)
                });
        entry.tree.dataset_count = Some((summary.datasets as usize).max(child_summary.0));
        entry.tree.trajectory_count = Some((summary.trajectories as usize).max(child_summary.1));
        entry.tree.run_count = entry.tree.trajectory_count.unwrap_or_default();
        let partial =
            summary.partial || projection_is_partial(&*self.index.values.read().await, key);
        let mut snapshot = self.snapshot(key, entry);
        snapshot.browse.partial = partial;
        if partial && !snapshot.browse.refreshing {
            snapshot.browse.state = CatalogState::Partial;
        }
        snapshot
    }
}

// A root observation is not a complete descendant inventory. This also
// exposes the worker's traversal limit without treating unseen directories as empty.
fn projection_is_partial(values: &HashMap<TreeKey, IndexEntry>, key: &TreeKey) -> bool {
    values
        .iter()
        .filter(|(other, _)| {
            other.dataset == key.dataset
                && other.uri_fingerprint == key.uri_fingerprint
                && (key.prefix.is_empty()
                    || other.prefix == key.prefix
                    || other.prefix.starts_with(&format!("{}/", key.prefix)))
        })
        .any(|(_, entry)| {
            entry
                .tree
                .children
                .iter()
                .filter(|child| child.kind == "dir")
                .any(|child| {
                    !values.contains_key(&TreeKey {
                        prefix: child.path.clone(),
                        ..key.clone()
                    })
                })
        })
}

fn manifest_key(key: &TreeKey, child_prefix: Option<&str>) -> String {
    let prefix = child_prefix.unwrap_or(&key.prefix);
    if prefix.is_empty() {
        format!("{}\0{}", key.dataset, key.uri_fingerprint)
    } else {
        format!("{}\0{}\0{}", key.dataset, key.uri_fingerprint, prefix)
    }
}

fn cached_leaf_summary(trees: &[CatalogTree], prefix: &str) -> (usize, usize) {
    let mut leaves = HashMap::<String, usize>::new();
    for tree in trees {
        for child in &tree.children {
            if child.kind == "file"
                && child.data_type != "other"
                && (child.path == prefix || child.path.starts_with(&format!("{prefix}/")))
            {
                leaves.insert(child.path.clone(), child.run_count);
            }
        }
    }
    (
        leaves.len(),
        leaves.values().copied().fold(0usize, usize::saturating_add),
    )
}

async fn run_worker(
    mounts: Vec<DatasetMount>,
    index: Arc<BrowseTreeProjection>,
    manifests: Arc<ManifestCache>,
    pending: Pending,
    states: Arc<Mutex<HashMap<TreeKey, RefreshState>>>,
    mut receiver: mpsc::Receiver<TreeKey>,
) {
    let mut interval = tokio::time::interval(REFRESH_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut background = VecDeque::new();
    let mut visited = HashSet::new();
    let mut background_budget = 0usize;
    let mut background_running = false;
    // FIFO is intentional: children are appended only after their parent
    // completes, so the background walk is breadth-first (shallow to deep).
    // JoinSet aborts outstanding work when the coordinator is dropped.
    let mut jobs = tokio::task::JoinSet::<(TreeKey, bool, Result<CatalogTree>)>::new();
    loop {
        let (key, is_background) = tokio::select! {
            biased;
            completed = jobs.join_next(), if !jobs.is_empty() => {
                let (key, was_background, result) = match completed.unwrap() {
                    Ok(completed) => completed,
                    Err(error) => {
                        tracing::error!(target: "pchronicle.serve", %error, "browse refresh task stopped");
                        for (_, waiters) in lock_recover(&pending).drain() {
                            for waiter in waiters {
                                let _ = waiter.send(Err(format!("browse refresh task stopped: {error}")));
                            }
                        }
                        return;
                    }
                };
                if was_background { background_running = false; }
                let outcome = match result {
                    Ok(tree) => {
                        visited.insert(key.clone());
                        for child in &tree.children {
                            if child.kind == "dir" && visited.len() < 10_000 {
                                let next = TreeKey { prefix: child.path.clone(), ..key.clone() };
                                if visited.insert(next.clone()) { background.push_back(next); }
                            }
                        }
                        lock_recover(&states).insert(key.clone(), RefreshState {
                            retry_at: Some(Instant::now() + Duration::from_secs(2)),
                            ..Default::default()
                        });
                        Ok(())
                    }
                    Err(error) => {
                        let error = format!("{error:#}");
                        let cached_view = index.values.read().await.contains_key(&key);
                        let mut states = lock_recover(&states);
                        let state = states.entry(key.clone()).or_default();
                        state.failures = state.failures.saturating_add(1);
                        state.retry_at = Some(Instant::now() + Duration::from_secs(
                            (30u64 * (1u64 << state.failures.min(4))).min(300)));
                        state.error = Some(error.clone());
                        tracing::warn!(target: "pchronicle.serve", dataset = %key.dataset,
                            prefix = %key.prefix, cached_view, %error, "browse refresh failed");
                        Err(error)
                    }
                };
                finish(&pending, &key, outcome);
                continue;
            }
            request = receiver.recv(), if jobs.len() < 2 => match request {
                Some(key) => (key, false), None => break
            },
            _ = std::future::ready(()), if !background.is_empty()
                && background_budget > 0 && !background_running && jobs.len() < 2 => {
                background_budget -= 1;
                (background.pop_front().unwrap(), true)
            },
            _ = interval.tick() => {
                if background.is_empty() && !background_running {
                    visited.clear();
                    background.extend(mounts.iter().filter_map(|m| TreeKey::new(m, "").ok()));
                    visited.extend(background.iter().cloned());
                }
                background_budget = 32;
                continue;
            }
        };
        let Some(mount) = mounts
            .iter()
            .find(|m| TreeKey::new(m, &key.prefix).ok().as_ref() == Some(&key))
            .cloned()
        else {
            continue;
        };
        // A queued foreground request or an active job already owns this key.
        // Leave its waiters attached; never start a duplicate background scan.
        if is_background && lock_recover(&pending).contains_key(&key) {
            continue;
        }
        if lock_recover(&states)
            .get(&key)
            .is_some_and(|s| s.retry_at.is_some_and(|deadline| deadline > Instant::now()))
        {
            finish(&pending, &key, Ok(()));
            continue;
        }
        lock_recover(&pending).entry(key.clone()).or_default();
        if is_background {
            background_running = true;
        }
        let index = index.clone();
        let manifests = manifests.clone();
        jobs.spawn(async move {
            // Reserve at least one global browse slot for foreground work.
            let _background = if is_background {
                BACKGROUND_IO.acquire().await.ok()
            } else {
                None
            };
            let result = refresh_tree(&mount, &key, &index, &manifests).await;
            (key, is_background, result)
        });
    }
}

async fn refresh_tree(
    mount: &DatasetMount,
    key: &TreeKey,
    index: &BrowseTreeProjection,
    manifests: &ManifestCache,
) -> Result<CatalogTree> {
    let _permit = BROWSE_IO.acquire().await?;
    #[cfg(test)]
    let block = lock_recover(&index.refresh_blocks).get(key).cloned();
    #[cfg(test)]
    let _block = if let Some(block) = block {
        Some(block.acquire_owned().await?)
    } else {
        None
    };
    let location = DatasetLocation::parse(&mount.uri)?;
    if let Some(root) = location.local_path() {
        tokio::fs::metadata(root.join(&key.prefix))
            .await
            .context("browse path unavailable")?;
    }
    let manifest_key = if key.prefix.is_empty() {
        format!("{}\0{}", key.dataset, key.uri_fingerprint)
    } else {
        format!("{}\0{}\0{}", key.dataset, key.uri_fingerprint, key.prefix)
    };
    // Only inspect this prefix: child marker probes and statistics are deferred
    // to the bounded background walk, so a wide directory can be cached promptly.
    let listing = tokio::time::timeout(
        Duration::from_secs(60),
        manifests.refresh_for_browse(manifest_key, &location, &key.prefix),
    )
    .await
    .context("browse list timed out")??;
    let tree = catalog_tree_from_path_list(&mount.name, &key.prefix, &listing.entries);
    index.put(key.clone(), tree.clone()).await;
    Ok(tree)
}

fn finish(pending: &Pending, key: &TreeKey, outcome: std::result::Result<(), String>) {
    if let Some(waiters) = lock_recover(&pending).remove(key) {
        for waiter in waiters {
            let _ = waiter.send(outcome.clone());
        }
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// UI-only tree projection over the core ManifestCache. It preserves the legacy tree wire format.
struct BrowseTreeProjection {
    #[cfg(test)]
    refresh_blocks: Mutex<HashMap<TreeKey, Arc<tokio::sync::Semaphore>>>,
    disk: Arc<ManifestCache>,
    values: RwLock<HashMap<TreeKey, IndexEntry>>,
}

impl BrowseTreeProjection {
    async fn open(path: PathBuf) -> Self {
        let disk = Arc::new(ManifestCache::open(path).await);
        let values = disk
            .projection_values()
            .await
            .into_iter()
            .filter_map(|(key, value)| {
                Some((
                    serde_json::from_str(&key).ok()?,
                    serde_json::from_value(value).ok()?,
                ))
            })
            .collect();
        Self {
            disk,
            values: RwLock::new(values),
            #[cfg(test)]
            refresh_blocks: Mutex::new(HashMap::new()),
        }
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
        if self.disk.writable()
            && (changed || !removed.is_empty())
            && let Err(error) = async {
                let removed_keys = removed
                    .iter()
                    .map(|key| serde_json::to_string(key).unwrap())
                    .collect::<Vec<_>>();
                self.disk.remove_projections(&removed_keys).await?;
                self.disk
                    .put_projection(
                        serde_json::to_string(&key).unwrap(),
                        &serde_json::to_value(&entry).unwrap(),
                    )
                    .await
            }
            .await
        {
            tracing::warn!(target: "pchronicle.serve", error = %error, "browse cache persistence failed; using memory");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lance::Dataset;
    use lance::dataset::InsertBuilder;
    use lance::deps::arrow_array::{RecordBatch, StringArray};
    use lance::deps::arrow_schema::{DataType, Field, Schema};

    fn mount(path: &std::path::Path) -> DatasetMount {
        DatasetMount::new("test", path.to_string_lossy()).unwrap()
    }

    #[tokio::test]
    async fn cold_tree_returns_loading_and_incomplete_descendants_are_partial() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        let mount = mount(&source);
        let coordinator =
            BrowseCoordinator::start_at(vec![mount.clone()], temp.path().join("cache")).await;
        let cold = tokio::time::timeout(Duration::from_secs(1), coordinator.tree(&mount, ""))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cold.tree.dataset.as_deref(), Some("test"));
        assert!(matches!(
            cold.browse.state,
            CatalogState::Ready | CatalogState::Partial
        ));
        assert!(cold.browse.observed_at > 0);
        let key = TreeKey::new(&mount, "").unwrap();
        let mut entry = IndexEntry {
            tree: CatalogTree::default(),
            generation: String::new(),
            observed_at: now(),
        };
        entry
            .tree
            .children
            .push(super::super::explorer::CatalogTreeChild {
                kind: "dir".into(),
                path: "nested".into(),
                ..Default::default()
            });
        let mut values = HashMap::from([(key.clone(), entry.clone())]);
        assert!(projection_is_partial(&values, &key));
        entry.tree.children.clear();
        values.insert(
            TreeKey {
                prefix: "nested".into(),
                ..key.clone()
            },
            entry,
        );
        assert!(!projection_is_partial(&values, &key));
    }

    #[tokio::test]
    async fn foreground_finishes_while_background_is_blocked_and_drop_cancels_work() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        let mount = mount(&source);
        // Construct before starting the worker so the root background scan is
        // deterministically blocked while a child foreground request arrives.
        let index = Arc::new(BrowseTreeProjection::open(temp.path().join("tree.lance")).await);
        let root_key = TreeKey::new(&mount, "").unwrap();
        let blocker = Arc::new(tokio::sync::Semaphore::new(0));
        lock_recover(&index.refresh_blocks).insert(root_key.clone(), blocker.clone());
        let manifests = Arc::new(ManifestCache::open(temp.path().join("manifest.lance")).await);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let states = Arc::new(Mutex::new(HashMap::new()));
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let task = tokio::spawn(run_worker(
            vec![mount.clone()],
            index,
            manifests,
            pending.clone(),
            states,
            receiver,
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            while !lock_recover(&pending).contains_key(&root_key) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let child = TreeKey::new(&mount, "nested").unwrap();
        let (reply, wait) = oneshot::channel();
        lock_recover(&pending).insert(child.clone(), vec![reply]);
        sender.send(child).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            lock_recover(&pending).contains_key(&root_key),
            "background must still be blocked"
        );
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        // Aborting the scheduler drops its JoinSet and releases active I/O.
        let permits = tokio::time::timeout(Duration::from_secs(5), BROWSE_IO.acquire_many(2))
            .await
            .unwrap()
            .unwrap();
        drop(permits);
    }

    #[tokio::test]
    async fn directory_statistics_use_cached_descendants_without_child_projections() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        for (path, count) in [
            ("codex2", 0),
            ("codex3", 0),
            ("nested/nested1/codex2", 354),
            ("nested/nested2/codex2", 354),
        ] {
            let leaf = source.join(path);
            std::fs::create_dir_all(&leaf).unwrap();
            persisting_pchronicle::storage::write_compact_jsonl_manifest(&leaf, 1, count).unwrap();
        }
        let mount = mount(&source);
        // Keep the scheduler idle: only manifest observations supply the response.
        let browse = BrowseCoordinator::start_at(Vec::new(), temp.path().join("cache")).await;
        browse.task.abort();
        let root_key = TreeKey::new(&mount, "").unwrap();
        browse
            .manifests
            .refresh_mount(
                &manifest_key(&root_key, None),
                &DatasetLocation::parse(&mount.uri).unwrap(),
            )
            .await
            .unwrap();
        std::fs::remove_dir_all(&source).unwrap();
        assert!(browse.index.values.read().await.is_empty());
        for (prefix, datasets) in [("", 4), ("nested", 2)] {
            let key = TreeKey::new(&mount, prefix).unwrap();
            let listing = browse
                .manifests
                .get(&manifest_key(&key, None))
                .await
                .unwrap();
            let view = browse
                .snapshot_with_summary(
                    &key,
                    IndexEntry {
                        tree: catalog_tree_from_path_list(&mount.name, prefix, &listing.entries),
                        generation: String::new(),
                        observed_at: now(),
                    },
                )
                .await;
            let json = serde_json::to_value(view).unwrap();
            assert_eq!(json["dataset_count"], datasets);
            assert_eq!(json["trajectory_count"], 708);
            for child in json["children"].as_array().unwrap() {
                if child["kind"] == "dir" {
                    assert_eq!(
                        child["dataset_count"],
                        if prefix.is_empty() { 2 } else { 1 }
                    );
                    assert_eq!(
                        child["trajectory_count"],
                        if prefix.is_empty() { 708 } else { 354 }
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn corrupt_cache_is_rebuilt_and_persisted() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.lance");
        std::fs::write(&path, "broken cache").unwrap();
        let index = BrowseTreeProjection::open(path.clone()).await;
        let key = TreeKey::new(&mount(temp.path()), "").unwrap();
        index.put(key.clone(), CatalogTree::default()).await;
        assert!(path.is_dir());
        assert_eq!(index.values.read().await.len(), 1);
        drop(index);
        let reloaded = BrowseTreeProjection::open(path).await;
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
        let index = BrowseTreeProjection::open(path.clone()).await;
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
        assert_ne!(first.index.disk.path(), second.index.disk.path());
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
        let gate = BROWSE_IO.acquire_many(2).await.unwrap();
        let key = TreeKey::new(&mount, "").unwrap();
        let (a, ar) = oneshot::channel();
        let (b, br) = oneshot::channel();
        coordinator.enqueue(&key, Some(a)).unwrap();
        coordinator.enqueue(&key, Some(b)).unwrap();
        assert_eq!(lock_recover(&coordinator.pending).len(), 1);
        assert_eq!(lock_recover(&coordinator.pending)[&key].len(), 2);
        drop(gate);
        ar.await.unwrap().unwrap();
        br.await.unwrap().unwrap();
        let old = coordinator.tree(&mount, "").await.unwrap();
        assert_eq!(old.tree.children[0].name, "nested");
        std::fs::remove_dir_all(&source).unwrap();
        lock_recover(&coordinator.refresh).remove(&key);
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
        let index = BrowseTreeProjection::open(path.clone()).await;
        let mount = mount(temp.path());
        let child = TreeKey::new(&mount, "gone/nested").unwrap();
        index.put(child.clone(), CatalogTree::default()).await;
        index
            .put(TreeKey::new(&mount, "").unwrap(), CatalogTree::default())
            .await;
        assert!(!index.values.read().await.contains_key(&child));
        drop(index);
        let reloaded = BrowseTreeProjection::open(path).await;
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
        let index = BrowseTreeProjection::open(path.clone()).await;
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
        let first = BrowseTreeProjection::open(path.clone()).await;
        let second = BrowseTreeProjection::open(path.clone()).await;
        assert!(first.disk.writable());
        assert!(!second.disk.writable());
        second
            .put(
                TreeKey::new(&mount(temp.path()), "").unwrap(),
                CatalogTree::default(),
            )
            .await;
        assert!(!path.exists());
    }
}

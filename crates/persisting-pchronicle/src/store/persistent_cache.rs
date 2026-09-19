//! Rebuildable Lance-backed key/value cache. It is never authoritative.
use anyhow::{Context, Result};
use futures::TryStreamExt;
use lance::Dataset;
use lance::dataset::optimize::{CompactionOptions, compact_files};
use lance::dataset::{InsertBuilder, MergeInsertBuilder, WhenMatched, WhenNotMatched};
use lance::deps::arrow_array::{Array, RecordBatch, RecordBatchIterator, StringArray};
use lance::deps::arrow_schema::{DataType, Field, Schema};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
const SCHEMA_VERSION: &str = "persistent-cache-v1";
/// Fragments tolerated before a write folds them back together.
const MAX_FRAGMENTS: usize = 32;
const COMPACTION_TARGET_ROWS: usize = 16 * 1024;
pub struct PersistentCache<K, V> {
    path: PathBuf,
    values: RwLock<HashMap<K, V>>,
    disk_lock: Option<std::fs::File>,
    write_gate: tokio::sync::Mutex<()>,
}
impl<K, V> PersistentCache<K, V>
where
    K: Eq + std::hash::Hash + Serialize + DeserializeOwned,
    V: Serialize + DeserializeOwned,
{
    pub async fn open(path: PathBuf) -> Self {
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
        let mut cache = Self {
            path,
            values: RwLock::new(HashMap::new()),
            disk_lock: lock.ok(),
            write_gate: tokio::sync::Mutex::new(()),
        };
        let writable = cache.writable();
        let attempts = if writable { 1 } else { 5 };
        let mut loaded = None;
        let mut last_error = None;
        for attempt in 0..attempts {
            match cache.load().await {
                Ok(values) => {
                    loaded = Some(values);
                    break;
                }
                Err(error) => {
                    last_error = Some(error);
                    if attempt + 1 < attempts {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            50 * (1u64 << attempt),
                        ))
                        .await;
                    }
                }
            }
        }
        if let Some(values) = loaded {
            *cache.values.get_mut() = values;
            if let Err(error) = cache.rewrite_if_fragmented().await {
                tracing::warn!(target: "pchronicle.serve", error = %error,
                    "persistent cache compaction failed");
            }
        } else if let Some(error) = last_error {
            tracing::warn!(target: "pchronicle.serve", error = %error, writable, attempts,
                "persistent cache unreadable");
            if writable {
                let _ = if cache.path.is_dir() {
                    std::fs::remove_dir_all(&cache.path)
                } else {
                    std::fs::remove_file(&cache.path)
                };
            }
        }
        cache
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn writable(&self) -> bool {
        self.disk_lock.is_some()
    }
    pub async fn values(&self) -> HashMap<K, V>
    where
        K: Clone,
        V: Clone,
    {
        self.values.read().await.clone()
    }
    pub async fn upsert(&self, key: &K, value: &V, removed: &[K]) -> Result<()> {
        self.upsert_many([(key, value)], removed).await
    }

    /// Persist many entries in one merge-insert.
    ///
    /// Every merge-insert is a Lance commit that re-reads the target to find
    /// matches, so writing a bulk observation one entry at a time costs
    /// `entries × cache size`. Callers that produce a whole observation should
    /// hand it over together.
    pub async fn upsert_many<'a, I>(&self, entries: I, removed: &[K]) -> Result<()>
    where
        I: IntoIterator<Item = (&'a K, &'a V)>,
        K: 'a,
        V: 'a,
    {
        if !self.writable() {
            return Ok(());
        }
        let mut keys = Vec::new();
        let mut payloads = Vec::new();
        for (key, value) in entries {
            keys.push(serde_json::to_string(key)?);
            payloads.push(serde_json::to_string(value)?);
        }
        // Merge-insert rejects a source that repeats a key, and a later
        // observation supersedes an earlier one in the same batch.
        let mut seen = std::collections::HashSet::with_capacity(keys.len());
        for index in (0..keys.len()).rev() {
            if !seen.insert(keys[index].clone()) {
                keys.remove(index);
                payloads.remove(index);
            }
        }
        // An entry present in this batch was just observed, so a retirement
        // recorded earlier in the same batch must not delete it.
        let removed = removed
            .iter()
            .map(|key| serde_json::to_string(key))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|key| !seen.contains(key))
            .collect::<Vec<_>>();
        if keys.is_empty() && removed.is_empty() {
            return Ok(());
        }
        let _write = self.write_gate.lock().await;
        let batch = cache_batch(keys, payloads)?;
        let schema = batch.schema();
        if self.path.exists() {
            let mut dataset = Dataset::open(self.path.to_string_lossy().as_ref()).await?;
            for chunk in removed.chunks(128) {
                let keys = chunk
                    .iter()
                    .map(|key| format!("'{}'", key.replace("'", "''")))
                    .collect::<Vec<_>>()
                    .join(",");
                dataset.delete(&format!("key IN ({keys})")).await?;
            }
            if batch.num_rows() > 0 {
                MergeInsertBuilder::try_new(Arc::new(dataset), vec!["key".into()])?
                    .when_matched(WhenMatched::UpdateAll)
                    .when_not_matched(WhenNotMatched::InsertAll)
                    .try_build()?
                    .execute_reader(Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema)))
                    .await?;
            }
            self.compact_if_fragmented().await?;
        } else if batch.num_rows() > 0 {
            InsertBuilder::new(self.path.to_string_lossy().as_ref())
                .execute(vec![batch])
                .await?;
        }
        Ok(())
    }

    /// Fold accumulated fragments back together.
    ///
    /// A merge-insert reads every fragment of the target to find matches, so a
    /// cache grown one write at a time makes each later write scan every row
    /// ever written. Compaction keeps that drift bounded between restarts.
    async fn compact_if_fragmented(&self) -> Result<()> {
        let mut dataset = Dataset::open(self.path.to_string_lossy().as_ref()).await?;
        if dataset.get_fragments().len() <= MAX_FRAGMENTS {
            return Ok(());
        }
        compact_files(
            &mut dataset,
            CompactionOptions {
                target_rows_per_fragment: COMPACTION_TARGET_ROWS,
                max_rows_per_group: 1024,
                num_threads: Some(1),
                ..Default::default()
            },
            None,
        )
        .await?;
        Ok(())
    }

    /// Replace a badly fragmented cache with one file holding what we just read.
    ///
    /// Caches written one entry at a time before batching landed hold a
    /// fragment and a version per entry — tens of thousands of them, gigabytes
    /// for a directory listing. Compacting that many fragments is far slower
    /// than rewriting the contents we already have in memory, and only a
    /// rewrite reclaims the superseded files. The cache is rebuildable, so
    /// losing it to a crash mid-rewrite costs a refresh, nothing more.
    async fn rewrite_if_fragmented(&self) -> Result<()> {
        if !self.writable() || !self.path.exists() {
            return Ok(());
        }
        let fragments = Dataset::open(self.path.to_string_lossy().as_ref())
            .await?
            .get_fragments()
            .len();
        if fragments <= MAX_FRAGMENTS {
            return Ok(());
        }
        let values = self.values.read().await;
        let mut keys = Vec::with_capacity(values.len());
        let mut payloads = Vec::with_capacity(values.len());
        for (key, value) in values.iter() {
            keys.push(serde_json::to_string(key)?);
            payloads.push(serde_json::to_string(value)?);
        }
        let entries = keys.len();
        let batch = cache_batch(keys, payloads)?;
        std::fs::remove_dir_all(&self.path)?;
        if batch.num_rows() > 0 {
            InsertBuilder::new(self.path.to_string_lossy().as_ref())
                .execute(vec![batch])
                .await?;
        }
        tracing::info!(target: "pchronicle.serve", fragments, entries,
            path = %self.path.display(), "rewrote fragmented persistent cache");
        Ok(())
    }
    async fn load(&self) -> Result<HashMap<K, V>> {
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
                    versions[row] == SCHEMA_VERSION,
                    "unsupported persistent cache schema"
                );
                values.insert(
                    serde_json::from_str(&keys[row])?,
                    serde_json::from_str(&payloads[row])?,
                );
            }
        }
        Ok(values)
    }
}

fn cache_batch(keys: Vec<String>, payloads: Vec<String>) -> Result<RecordBatch> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("key", DataType::Utf8, false),
        Field::new("payload", DataType::Utf8, false),
        Field::new("schema_version", DataType::Utf8, false),
    ]));
    let versions = vec![SCHEMA_VERSION; keys.len()];
    Ok(RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(keys)) as _,
            Arc::new(StringArray::from(payloads)) as _,
            Arc::new(StringArray::from(versions)) as _,
        ],
    )?)
}

fn text_column(batch: &RecordBatch, name: &str) -> Result<Vec<String>> {
    let array = batch
        .column(batch.schema().index_of(name)?)
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("persistent cache column {name} must be Utf8"))?;
    anyhow::ensure!(array.null_count() == 0, "null persistent cache field");
    Ok((0..array.len())
        .map(|index| array.value(index).to_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn repeated_single_writes_stay_compact_and_readable() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("cache.lance");
        let cache = PersistentCache::<String, String>::open(path.clone()).await;
        assert!(cache.writable());
        for index in 0..80 {
            cache
                .upsert(&format!("key-{index:03}"), &format!("value-{index}"), &[])
                .await?;
        }
        let fragments = Dataset::open(path.to_str().context("cache path")?)
            .await?
            .get_fragments()
            .len();
        assert!(fragments <= MAX_FRAGMENTS, "{fragments} fragments");

        // Compaction rewrites fragments, so every entry must survive it.
        cache
            .upsert(&"key-007".into(), &"rewritten".into(), &[])
            .await?;
        cache.upsert(&"gone".into(), &String::new(), &[]).await?;
        cache
            .upsert(&"key-000".into(), &"kept".into(), &["gone".into()])
            .await?;
        drop(cache);
        let values = PersistentCache::<String, String>::open(path)
            .await
            .values()
            .await;
        assert_eq!(values.len(), 80);
        assert_eq!(values["key-007"], "rewritten");
        assert_eq!(values["key-000"], "kept");
        assert!(!values.contains_key("gone"));
        Ok(())
    }

    #[tokio::test]
    async fn opening_rewrites_a_cache_fragmented_by_earlier_writes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("cache.lance");
        // Reproduce the pre-batching shape: one fragment and one version per
        // entry, written without ever folding them back together.
        let cache = PersistentCache::<String, String>::open(path.clone()).await;
        let schema = cache_batch(vec!["seed".into()], vec!["\"seed\"".into()])?.schema();
        for index in 0..(MAX_FRAGMENTS * 2) {
            let batch = cache_batch(
                vec![format!("\"key-{index:03}\"")],
                vec![format!("\"value-{index}\"")],
            )?;
            if index == 0 {
                InsertBuilder::new(path.to_str().context("cache path")?)
                    .execute(vec![batch])
                    .await?;
            } else {
                let dataset = Dataset::open(path.to_str().context("cache path")?).await?;
                MergeInsertBuilder::try_new(Arc::new(dataset), vec!["key".into()])?
                    .when_matched(WhenMatched::UpdateAll)
                    .when_not_matched(WhenNotMatched::InsertAll)
                    .try_build()?
                    .execute_reader(Box::new(RecordBatchIterator::new(
                        vec![Ok(batch)],
                        schema.clone(),
                    )))
                    .await?;
            }
        }
        drop(cache);
        let before = Dataset::open(path.to_str().context("cache path")?)
            .await?
            .get_fragments()
            .len();
        assert!(before > MAX_FRAGMENTS, "{before} fragments");

        let cache = PersistentCache::<String, String>::open(path.clone()).await;
        let after = Dataset::open(path.to_str().context("cache path")?)
            .await?
            .get_fragments()
            .len();
        assert!(after < before, "{before} fragments became {after}");
        assert!(after <= MAX_FRAGMENTS, "{after} fragments");
        let values = cache.values().await;
        assert_eq!(values.len(), MAX_FRAGMENTS * 2);
        assert_eq!(values["key-005"], "value-5");
        // Superseded files must go with the rewrite, not linger as versions.
        let versions = std::fs::read_dir(path.join("_versions"))?.count();
        assert!(versions <= 2, "{versions} versions");
        Ok(())
    }

    #[tokio::test]
    async fn a_batch_supersedes_its_own_repeats_and_keeps_what_it_writes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("cache.lance");
        let cache = PersistentCache::<String, String>::open(path.clone()).await;
        cache.upsert(&"stale".into(), &"old".into(), &[]).await?;
        let entries = [
            ("a".to_owned(), "first".to_owned()),
            ("a".to_owned(), "last".to_owned()),
            ("stale".to_owned(), "revived".to_owned()),
        ];
        cache
            .upsert_many(
                entries.iter().map(|(key, value)| (key, value)),
                &["stale".to_owned(), "absent".to_owned()],
            )
            .await?;
        drop(cache);
        let values = PersistentCache::<String, String>::open(path)
            .await
            .values()
            .await;
        assert_eq!(values["a"], "last");
        // A key this batch writes was just observed; its earlier retirement
        // inside the same batch must not delete it.
        assert_eq!(values["stale"], "revived");
        Ok(())
    }
}

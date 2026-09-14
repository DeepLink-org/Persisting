//! Rebuildable Lance-backed key/value cache. It is never authoritative.
use anyhow::{Context, Result};
use futures::TryStreamExt;
use lance::Dataset;
use lance::dataset::{InsertBuilder, MergeInsertBuilder, WhenMatched, WhenNotMatched};
use lance::deps::arrow_array::{Array, RecordBatch, RecordBatchIterator, StringArray};
use lance::deps::arrow_schema::{DataType, Field, Schema};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
const SCHEMA_VERSION: &str = "persistent-cache-v1";
pub struct PersistentCache<K, V> {
    path: PathBuf,
    values: RwLock<HashMap<K, V>>,
    disk_lock: Option<std::fs::File>,
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
        };
        if cache.disk_lock.is_some() {
            match cache.load().await {
                Ok(values) => *cache.values.get_mut() = values,
                Err(error) => {
                    tracing::warn!(target: "pchronicle.serve", error = %error, "persistent cache unreadable; rebuilding");
                    let _ = if cache.path.is_dir() {
                        std::fs::remove_dir_all(&cache.path)
                    } else {
                        std::fs::remove_file(&cache.path)
                    };
                }
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
        if !self.writable() {
            return Ok(());
        }
        let schema = Arc::new(Schema::new(vec![
            Field::new("key", DataType::Utf8, false),
            Field::new("payload", DataType::Utf8, false),
            Field::new("schema_version", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![serde_json::to_string(key)?])) as _,
                Arc::new(StringArray::from(vec![serde_json::to_string(value)?])) as _,
                Arc::new(StringArray::from(vec![SCHEMA_VERSION])) as _,
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
            MergeInsertBuilder::try_new(Arc::new(dataset), vec!["key".into()])?
                .when_matched(WhenMatched::UpdateAll)
                .when_not_matched(WhenNotMatched::InsertAll)
                .try_build()?
                .execute_reader(Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema)))
                .await?;
        } else {
            InsertBuilder::new(self.path.to_string_lossy().as_ref())
                .execute(vec![batch])
                .await?;
        }
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

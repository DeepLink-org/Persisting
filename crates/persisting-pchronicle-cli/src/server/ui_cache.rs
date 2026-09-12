//! Persistent, UI-only catalog tree cache.
//!
//! This cache contains directory metadata and manifest statistics only. Query
//! and analysis paths deliberately do not depend on it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use lance::Dataset;
use lance::dataset::{InsertBuilder, MergeInsertBuilder, WhenMatched, WhenNotMatched};
use lance::deps::arrow_array::{Array, RecordBatch, RecordBatchIterator, StringArray};
use lance::deps::arrow_schema::{DataType, Field, Schema};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::explorer::CatalogTree;

const CACHE_SCHEMA_VERSION: &str = "ui-tree-v1";

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct TreeKey {
    pub dataset: String,
    pub prefix: String,
}

#[derive(Clone)]
pub(crate) struct UiTreeCache {
    path: Arc<PathBuf>,
    values: Arc<RwLock<HashMap<TreeKey, CatalogTree>>>,
    write_lock: Arc<tokio::sync::Mutex<()>>,
}

impl UiTreeCache {
    pub(crate) fn new() -> Self {
        let root = std::env::var_os("PCHRONICLE_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::cache_dir().map(|path| path.join("pchronicle")))
            .unwrap_or_else(|| std::env::temp_dir().join("pchronicle"));
        let path = root.join("catalog-ui.lance");
        Self {
            path: Arc::new(path),
            values: Arc::new(RwLock::new(HashMap::new())),
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) async fn load_safely(&self) {
        if let Err(error) = self.load().await {
            tracing::debug!(target: "pchronicle.serve", error = %error, "UI catalog cache unavailable; rebuilding");
            // The cache is disposable UI metadata. Remove an unreadable Lance
            // directory so the next write can recreate it cleanly.
            if let Err(remove_error) = tokio::fs::remove_dir_all(self.path.as_ref()).await {
                if remove_error.kind() != std::io::ErrorKind::NotFound {
                    if let Err(file_error) = tokio::fs::remove_file(self.path.as_ref()).await {
                        if file_error.kind() != std::io::ErrorKind::NotFound {
                            tracing::debug!(target: "pchronicle.serve", error = %remove_error, file_error = %file_error, "failed to remove corrupt UI catalog cache");
                        }
                    }
                }
            }
        }
    }

    pub(crate) async fn get(&self, key: &TreeKey) -> Option<CatalogTree> {
        self.values.read().await.get(key).cloned()
    }

    pub(crate) async fn put(&self, key: TreeKey, tree: CatalogTree) {
        self.values.write().await.insert(key.clone(), tree.clone());
        if let Err(error) = self.persist(&key, &tree).await {
            tracing::debug!(target: "pchronicle.serve", error = %error, "UI catalog cache write failed");
        }
    }

    pub(crate) async fn keys(&self) -> Vec<TreeKey> {
        self.values.read().await.keys().cloned().collect()
    }

    async fn load(&self) -> Result<()> {
        let dataset = match Dataset::open(self.path.to_string_lossy().as_ref()).await {
            Ok(dataset) => dataset,
            Err(lance::Error::DatasetNotFound { .. }) => return Ok(()),
            Err(error) => return Err(anyhow::anyhow!(error)).context("open UI catalog cache"),
        };
        let batches: Vec<RecordBatch> = dataset
            .scan()
            .try_into_stream()
            .await?
            .try_collect()
            .await?;
        let mut values = self.values.write().await;
        for batch in batches {
            let keys = text_column(&batch, "key")?;
            let payloads = text_column(&batch, "payload")?;
            for row in 0..batch.num_rows() {
                let key: TreeKey = serde_json::from_str(&keys[row])?;
                let tree: CatalogTree = serde_json::from_str(&payloads[row])?;
                values.insert(key, tree);
            }
        }
        Ok(())
    }

    async fn persist(&self, key: &TreeKey, tree: &CatalogTree) -> Result<()> {
        let _lock = self.write_lock.lock().await;
        let parent = self.path.parent().context("UI cache has no parent")?;
        tokio::fs::create_dir_all(parent).await?;
        let key_json = serde_json::to_string(key)?;
        let payload = serde_json::to_string(tree)?;
        let schema = schema();
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![key_json])) as _,
                Arc::new(StringArray::from(vec![payload])) as _,
                Arc::new(StringArray::from(vec![CACHE_SCHEMA_VERSION])) as _,
            ],
        )?;
        match Dataset::open(self.path.to_string_lossy().as_ref()).await {
            Ok(dataset) => {
                let reader = Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
                MergeInsertBuilder::try_new(Arc::new(dataset), vec!["key".into()])?
                    .when_matched(WhenMatched::UpdateAll)
                    .when_not_matched(WhenNotMatched::InsertAll)
                    .try_build()?
                    .execute_reader(reader)
                    .await?;
            }
            Err(lance::Error::DatasetNotFound { .. }) => {
                InsertBuilder::new(self.path.to_string_lossy().as_ref())
                    .execute(vec![batch])
                    .await?;
            }
            Err(error) => {
                return Err(anyhow::anyhow!(error)).context("open UI catalog cache for write");
            }
        }
        Ok(())
    }
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("key", DataType::Utf8, false),
        Field::new("payload", DataType::Utf8, false),
        Field::new("schema_version", DataType::Utf8, false),
    ]))
}

fn text_column(batch: &RecordBatch, name: &str) -> Result<Vec<String>> {
    let array = batch
        .column(batch.schema().index_of(name)?)
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("UI cache column {name} must be Utf8"))?;
    Ok((0..array.len())
        .map(|index| array.value(index).to_owned())
        .collect())
}

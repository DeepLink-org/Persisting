//! Small OpenDAL facade for pChronicle's control-plane objects.
//!
//! Lance still owns its internal storage bridge for opening datasets. This
//! module keeps pChronicle's own reads, listings and conditional writes on
//! OpenDAL so backend differences are handled in one place.

use anyhow::{Context, Result, anyhow};
use futures::TryStreamExt;
use opendal::{EntryMode, ErrorKind, Metadata, Operator};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Version {
    pub(crate) etag: Option<String>,
    pub(crate) version: Option<String>,
}

impl Version {
    pub(crate) fn condition(&self) -> Option<&str> {
        self.etag.as_deref().or(self.version.as_deref())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Store {
    operator: Operator,
    fallback_lock: Option<Arc<tokio::sync::Mutex<()>>>,
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) path: String,
    pub(crate) metadata: Metadata,
}

static SHARED_MEMORY: OnceLock<Mutex<HashMap<String, Operator>>> = OnceLock::new();
static SHARED_LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

impl Store {
    pub(crate) async fn from_uri(uri: &str) -> Result<Self> {
        let uri = uri.trim();
        let normalized = normalize_uri(uri)?;
        let shared_memory = uri.contains("://")
            && Url::parse(uri)
                .map(|parsed| parsed.scheme() == "shared-memory")
                .unwrap_or(false);
        let operator = if shared_memory {
            let map = SHARED_MEMORY.get_or_init(|| Mutex::new(HashMap::new()));
            let mut map = map
                .lock()
                .map_err(|_| anyhow!("shared-memory operator registry poisoned"))?;
            if let Some(operator) = map.get(uri) {
                operator.clone()
            } else {
                let operator = Operator::from_uri(normalized.as_str())
                    .with_context(|| format!("open OpenDAL store {uri}"))?;
                map.insert(uri.to_string(), operator.clone());
                operator
            }
        } else {
            Operator::from_uri(normalized.as_str())
                .with_context(|| format!("open OpenDAL store {uri}"))?
        };
        let fallback_lock = if shared_memory {
            let locks = SHARED_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
            let mut locks = locks
                .lock()
                .map_err(|_| anyhow!("shared-memory lock registry poisoned"))?;
            Some(
                locks
                    .entry(uri.to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                    .clone(),
            )
        } else {
            None
        };
        Ok(Self {
            operator,
            fallback_lock,
        })
    }

    pub(crate) async fn read(&self, path: &str) -> Result<Option<(Vec<u8>, Version)>> {
        let metadata = match self.operator.stat(path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let bytes = self.operator.read(path).await?.to_vec();
        Ok(Some((bytes, version(&metadata))))
    }

    pub(crate) async fn write_create(&self, path: &str, bytes: Vec<u8>) -> Result<()> {
        self.operator
            .write_with(path, bytes)
            .if_not_exists(true)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    pub(crate) async fn write_match(
        &self,
        path: &str,
        bytes: Vec<u8>,
        expected: &Version,
    ) -> Result<()> {
        if self.fallback_lock.is_some() {
            // ponytail: the in-process shared-memory test backend has no CAS
            // primitive; callers hold its per-root mutex for the full mutation.
            return self.write_overwrite(path, bytes).await;
        }
        let condition = expected.condition().ok_or_else(|| {
            anyhow!("OpenDAL backend did not return an ETag/version for conditional write")
        })?;
        self.operator
            .write_with(path, bytes)
            .if_match(condition)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    pub(crate) async fn write_overwrite(&self, path: &str, bytes: Vec<u8>) -> Result<()> {
        self.operator
            .write(path, bytes)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    pub(crate) async fn list(&self, prefix: &str) -> Result<Vec<Entry>> {
        let mut lister = self.operator.lister_with(prefix).recursive(true).await?;
        let mut entries = Vec::new();
        while let Some(entry) = lister.try_next().await? {
            if entry.metadata().mode() == EntryMode::FILE {
                entries.push(Entry {
                    path: entry.path().to_string(),
                    metadata: entry.metadata().clone(),
                });
            }
        }
        Ok(entries)
    }

    pub(crate) async fn exists(&self) -> Result<bool> {
        Ok(self
            .operator
            .lister_with("")
            .recursive(true)
            .await?
            .try_next()
            .await?
            .is_some())
    }

    pub(crate) async fn remove_all(&self) -> Result<()> {
        self.operator
            .delete_with("")
            .recursive(true)
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn remove(&self, path: &str) -> Result<()> {
        self.operator
            .delete_with(path)
            .recursive(true)
            .await
            .map_err(Into::into)
    }

    pub(crate) fn fallback_lock(&self) -> Option<Arc<tokio::sync::Mutex<()>>> {
        self.fallback_lock.clone()
    }
}

pub(crate) fn is_conflict(error: &opendal::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::AlreadyExists | ErrorKind::ConditionNotMatch
    )
}

pub(crate) fn version(metadata: &Metadata) -> Version {
    Version {
        etag: metadata.etag().map(ToOwned::to_owned),
        version: metadata.version().map(ToOwned::to_owned),
    }
}

fn normalize_uri(uri: &str) -> Result<String> {
    if !uri.contains("://") {
        let path = std::path::Path::new(uri);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        return Ok(format!("fs://{}", path.to_string_lossy()));
    }
    let mut parsed = Url::parse(uri).context("parse object-store URI")?;
    let scheme = match parsed.scheme() {
        "gs" => "gcs",
        "az" => "azblob",
        "shared-memory" => "memory",
        other => other,
    }
    .to_string();
    if scheme != parsed.scheme() {
        parsed
            .set_scheme(&scheme)
            .map_err(|_| anyhow!("invalid URI scheme"))?;
    }
    Ok(parsed.to_string())
}

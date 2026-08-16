//! Dataset-scoped write serialization for the single-writer storage engine.

use std::fs::{File, OpenOptions};
use std::path::Path;

use anyhow::{Context, Result};
use fs2::FileExt;

use super::root_write_lock;

pub(crate) struct DatasetWriteGuard {
    _process: tokio::sync::OwnedMutexGuard<()>,
    local_file: Option<File>,
}

impl Drop for DatasetWriteGuard {
    fn drop(&mut self) {
        if let Some(file) = &self.local_file {
            let _ = FileExt::unlock(file);
        }
    }
}

/// Serialize writers within the process and, for local datasets, across
/// processes. Object-store deployments retain the documented single-writer
/// contract; Lance transactions provide atomic publication but not a global
/// distributed mutex.
pub(crate) async fn acquire(uri: &str) -> Result<DatasetWriteGuard> {
    let process = root_write_lock::for_root(uri).lock_owned().await;
    let local_file = if super::events::is_object_store_uri(uri) {
        None
    } else {
        let lock_path = Path::new(uri).with_extension("lance.write.lock");
        Some(
            tokio::task::spawn_blocking(move || -> Result<File> {
                if let Some(parent) = lock_path.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("create write-lock root {}", parent.display()))?;
                }
                let file = OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(&lock_path)
                    .with_context(|| format!("open dataset write lock {}", lock_path.display()))?;
                file.lock_exclusive()
                    .with_context(|| format!("lock dataset {}", lock_path.display()))?;
                Ok(file)
            })
            .await
            .context("join dataset write-lock task")??,
        )
    };
    Ok(DatasetWriteGuard {
        _process: process,
        local_file,
    })
}

//! Shared helpers for pVisor.

use anyhow::Context;
pub use persisting_events::unix_now_ms;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub fn now_rfc3339_and_unix_ms() -> (String, u64) {
    let now = chrono::Utc::now();
    (now.to_rfc3339(), now.timestamp_millis().max(0) as u64)
}

pub(crate) fn sync_directory(path: &Path) -> anyhow::Result<()> {
    fs::File::open(path)?
        .sync_all()
        .with_context(|| format!("sync directory {}", path.display()))
}

/// Create a directory tree and sync every newly created directory entry.
pub(crate) fn create_dir_all_durable(path: &Path) -> anyhow::Result<()> {
    let mut missing = Vec::new();
    let mut cursor = path;
    while !cursor.exists() {
        missing.push(cursor.to_path_buf());
        let Some(parent) = cursor.parent() else {
            break;
        };
        if parent.as_os_str().is_empty() {
            break;
        }
        cursor = parent;
    }
    fs::create_dir_all(path)
        .with_context(|| format!("create directory tree {}", path.display()))?;
    for directory in missing.iter().rev() {
        let parent = directory
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        sync_directory(parent)?;
        sync_directory(directory)?;
    }
    Ok(())
}

/// Atomically replace a file after syncing both its contents and parent directory.
pub(crate) fn atomic_write(path: &Path, contents: &[u8], mode: u32) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
    create_dir_all_durable(parent)?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("persisting");
    let temporary = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .with_context(|| format!("create temporary file {}", temporary.display()))?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.write_all(contents)?;
        file.sync_all()
            .with_context(|| format!("sync temporary file {}", temporary.display()))?;
        fs::rename(&temporary, path)
            .with_context(|| format!("replace {} with {}", path.display(), temporary.display()))?;
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn atomic_write_replaces_private_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("record.json");
        atomic_write(&path, b"first", 0o600).unwrap();
        atomic_write(&path, b"second", 0o600).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn durable_directory_creation_handles_nested_paths() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("one/two/three");

        create_dir_all_durable(&path).unwrap();
        assert!(path.is_dir());
    }
}

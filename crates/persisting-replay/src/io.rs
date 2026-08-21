use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{ReplayError, ReplayErrorKind, ResultExt};

pub fn read_regular_file(path: &Path) -> Result<Vec<u8>, ReplayError> {
    let metadata = fs::symlink_metadata(path).replay_context(
        ReplayErrorKind::Configuration,
        format!("inspect {}", path.display()),
    )?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(ReplayError::configuration(format!(
            "input must be a regular file: {}",
            path.display()
        )));
    }
    const MAX_BYTES: u64 = 256 * 1024 * 1024;
    if metadata.len() > MAX_BYTES {
        return Err(ReplayError::configuration(format!(
            "input exceeds {MAX_BYTES} bytes: {}",
            path.display()
        )));
    }
    let mut file = File::open(path).replay_context(
        ReplayErrorKind::Configuration,
        format!("open {}", path.display()),
    )?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes).replay_context(
        ReplayErrorKind::Configuration,
        format!("read {}", path.display()),
    )?;
    if bytes.len() as u64 != metadata.len() {
        return Err(ReplayError::configuration(format!(
            "input changed while being read: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ReplayError> {
    let parent = path
        .parent()
        .ok_or_else(|| ReplayError::configuration("output path has no parent"))?;
    fs::create_dir_all(parent).replay_context(
        ReplayErrorKind::Executor,
        format!("create {}", parent.display()),
    )?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("output"),
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).replay_context(
        ReplayErrorKind::Executor,
        format!("create {}", temporary.display()),
    )?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .replay_context(
            ReplayErrorKind::Executor,
            format!("write {}", temporary.display()),
        )?;
    fs::rename(&temporary, path).replay_context(
        ReplayErrorKind::Executor,
        format!("replace {}", path.display()),
    )?;
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

pub fn atomic_write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), ReplayError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .replay_context(ReplayErrorKind::Internal, "serialize replay artifact")?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

pub fn canonicalize(
    path: &Path,
    kind: ReplayErrorKind,
    label: &str,
) -> Result<PathBuf, ReplayError> {
    fs::canonicalize(path).replay_context(kind, format!("resolve {label} {}", path.display()))
}

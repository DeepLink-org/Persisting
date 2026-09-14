//! Local staging and atomic publish helpers.

use super::super::*;
use super::progress::CliProgress;
use anyhow::{Context, Result, anyhow};
use std::ffi::CString;
use std::path::{Path, PathBuf};

pub(crate) struct StagingPathGuard {
    path: Option<PathBuf>,
}

impl StagingPathGuard {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    pub(crate) fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for StagingPathGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

pub(crate) async fn publish_staged_dataset(
    staging: &Path,
    output: &Path,
    replace_existing: bool,
    progress: Option<&mut CliProgress>,
) -> Result<()> {
    let parent = output
        .parent()
        .context("Dataset output must have a parent directory")?;
    if !replace_existing {
        rename_noreplace(staging, output)
            .with_context(|| format!("publish new Dataset {}", output.display()))?;
        sync_dataset_parent(parent)?;
        return Ok(());
    }

    let backup = parent.join(format!(
        ".pchronicle-replace-{}-{}",
        output
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_else(|| std::borrow::Cow::Borrowed("dataset")),
        uuid::Uuid::new_v4().simple()
    ));
    rename_noreplace(output, &backup)
        .with_context(|| format!("move existing Dataset to {}", backup.display()))?;
    if let Err(error) = sync_dataset_parent(parent) {
        return Err(rollback_replacement(output, &backup, error));
    }
    if let Err(error) = rename_noreplace(staging, output)
        .with_context(|| format!("publish replacement Dataset {}", output.display()))
    {
        return Err(rollback_replacement(output, &backup, error));
    }
    sync_dataset_parent(parent).with_context(|| {
        format!(
            "sync replacement Dataset parent {}; old Dataset remains at {}",
            parent.display(),
            backup.display()
        )
    })?;
    let backup_location = DatasetLocation::parse(
        backup
            .to_str()
            .context("replaced Dataset backup path is not valid UTF-8")?,
    )?;
    if let Some(progress) = progress {
        backup_location
            .remove_all_with_progress(|deleted, total, path| {
                progress.note_deleted(deleted, total, path)
            })
            .await
            .with_context(|| format!("delete replaced Dataset backup {}", backup.display()))?;
        progress.finish()?;
    } else {
        backup_location
            .remove_all()
            .await
            .with_context(|| format!("delete replaced Dataset backup {}", backup.display()))?;
    }
    sync_dataset_parent(parent)?;
    Ok(())
}

pub(crate) fn rollback_replacement(
    output: &Path,
    backup: &Path,
    error: anyhow::Error,
) -> anyhow::Error {
    match rename_noreplace(backup, output) {
        Ok(()) => error,
        Err(rollback_error) => anyhow!(
            "{error}; failed to restore old Dataset from {} to {}: {rollback_error}",
            backup.display(),
            output.display()
        ),
    }
}

pub(crate) fn sync_dataset_parent(parent: &Path) -> Result<()> {
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync Dataset parent {}", parent.display()))?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    #[cfg(target_os = "linux")]
    // SAFETY: both pointers come from live CString values and are NUL-terminated.
    // Call SYS_renameat2 directly so the binary still links on manylinux2014
    // (glibc 2.17). The renameat2() wrapper only exists in glibc 2.28+.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: both pointers come from live CString values and are NUL-terminated.
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn rename_noreplace(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic create-only Dataset publish is unsupported on this platform",
    ))
}

//! Lightweight build stub for the optional Jujutsu upper backend.

use std::io;
use std::path::{Path, PathBuf};

const WORKSPACES_DIR: &str = "workspaces";
const UPPER_DIR: &str = "upper";

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "the Jujutsu OverlayFS backend is not compiled in; enable the `jujutsu` feature",
    )
}

fn validate_fork(fork: &str) -> io::Result<()> {
    if fork.is_empty()
        || fork == "."
        || fork == ".."
        || fork.contains('/')
        || fork.contains('\\')
        || fork.as_bytes().contains(&0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid Jujutsu overlay workspace name: {fork:?}"),
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct JujutsuWorkspace {
    upper_dir: PathBuf,
}

impl JujutsuWorkspace {
    pub(crate) fn open(_store_path: PathBuf, _fork: String, _read_only: bool) -> io::Result<Self> {
        Err(unsupported())
    }

    pub(crate) fn upper_dir(&self) -> &Path {
        &self.upper_dir
    }

    pub(crate) fn snapshot(&self) -> io::Result<Option<String>> {
        Err(unsupported())
    }
}

pub fn snapshot_jujutsu_upper(_store_path: &Path, _fork: &str) -> io::Result<Option<String>> {
    Err(unsupported())
}

/// Return the deterministic upper path without initializing a Jujutsu store.
/// Actual mounting still fails with an explicit feature error in this build.
pub fn jujutsu_upper_dir(store_path: &Path, fork: &str) -> io::Result<PathBuf> {
    validate_fork(fork)?;
    Ok(store_path.join(WORKSPACES_DIR).join(fork).join(UPPER_DIR))
}

pub fn prepare_jujutsu_upper(_store_path: &Path, _fork: &str) -> io::Result<PathBuf> {
    Err(unsupported())
}

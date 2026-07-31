//! Embeddable cross-platform FUSE overlay implementation.
//!
//! pVisor owns [`OverlaySession`] directly, so the pVisor process is also the
//! FUSE userspace server. The `persisting-overlayfs` binary is only a debugging
//! and fuse-overlayfs-compatible CLI wrapper around this library.

mod core;
mod db_apply;
mod db_store;
mod fs;
mod snapshot;
mod sys;

use anyhow::{bail, Context, Result};
pub use db_apply::{apply_redb_upper, discard_redb_upper, redb_upper_status, RedbUpperStatus};
use fs::OverlayFs;
use fuser::{BackgroundSession, MountOption, Session};
use snapshot::SnapshotFilesystem;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum UpperBackend {
    Directory {
        upper_dir: PathBuf,
        work_dir: Option<PathBuf>,
    },
    Redb {
        database_path: PathBuf,
    },
}

#[derive(Clone, Debug)]
pub struct OverlayMountConfig {
    pub lower_dirs: Vec<PathBuf>,
    pub upper: UpperBackend,
    pub mountpoint: PathBuf,
    pub allow_other: bool,
    pub allow_root: bool,
    pub default_permissions: bool,
    pub read_only: bool,
    pub fsname: String,
    /// macFUSE backend (`kernel` or `fskit`). Ignored on non-macOS hosts.
    pub backend: Option<String>,
    pub debug: bool,
}

impl OverlayMountConfig {
    pub fn new(
        lower_dirs: Vec<PathBuf>,
        upper_dir: PathBuf,
        work_dir: Option<PathBuf>,
        mountpoint: PathBuf,
    ) -> Self {
        Self {
            lower_dirs,
            upper: UpperBackend::Directory {
                upper_dir,
                work_dir,
            },
            mountpoint,
            allow_other: false,
            allow_root: false,
            default_permissions: true,
            read_only: false,
            fsname: "persisting-overlayfs".into(),
            backend: None,
            debug: false,
        }
    }

    pub fn new_redb(lower_dirs: Vec<PathBuf>, database_path: PathBuf, mountpoint: PathBuf) -> Self {
        let mut config = Self::new(lower_dirs, PathBuf::new(), None, mountpoint);
        config.upper = UpperBackend::Redb { database_path };
        config
    }
}

#[derive(Debug)]
pub struct OverlaySession {
    background: Option<BackgroundSession>,
    mountpoint: PathBuf,
}

impl OverlaySession {
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    pub fn has_exited(&self) -> bool {
        self.background
            .as_ref()
            .is_none_or(|session| session.guard.is_finished())
    }

    /// Unmount by dropping the libfuse mount owned by this process.
    pub fn unmount(mut self) -> Result<()> {
        self.unmount_inner()
    }

    fn unmount_inner(&mut self) -> Result<()> {
        let Some(background) = self.background.take() else {
            return Ok(());
        };
        background
            .unmount()
            .context("unmount FUSE session and stop request loop")?;
        for _ in 0..250 {
            if !is_mountpoint(&self.mountpoint) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if is_mountpoint(&self.mountpoint) {
            bail!("FUSE mount did not detach: {}", self.mountpoint.display());
        }
        Ok(())
    }
}

impl Drop for OverlaySession {
    fn drop(&mut self) {
        let _ = self.unmount_inner();
    }
}

pub fn mount(config: OverlayMountConfig) -> Result<OverlaySession> {
    let (filesystem, mountpoint, options) = prepare(config)?;
    let background = match filesystem {
        FilesystemBackend::Directory(filesystem) => {
            let session = Session::new(filesystem, &mountpoint, &options)
                .with_context(|| format!("mount {}", mountpoint.display()))?;
            BackgroundSession::new(session)
        }
        FilesystemBackend::Redb(filesystem) => {
            let session = Session::new(filesystem, &mountpoint, &options)
                .with_context(|| format!("mount {}", mountpoint.display()))?;
            BackgroundSession::new(session)
        }
    }
    .context("start FUSE request loop")?;
    log::info!("persisting-overlayfs mounted at {}", mountpoint.display());
    Ok(OverlaySession {
        background: Some(background),
        mountpoint,
    })
}

pub fn run_foreground(config: OverlayMountConfig) -> Result<()> {
    let (filesystem, mountpoint, options) = prepare(config)?;
    log::info!("persisting-overlayfs mounted at {}", mountpoint.display());
    match filesystem {
        FilesystemBackend::Directory(filesystem) => {
            let mut session = Session::new(filesystem, &mountpoint, &options)
                .with_context(|| format!("mount {}", mountpoint.display()))?;
            session.run().context("FUSE session")
        }
        FilesystemBackend::Redb(filesystem) => {
            let mut session = Session::new(filesystem, &mountpoint, &options)
                .with_context(|| format!("mount {}", mountpoint.display()))?;
            session.run().context("FUSE session")
        }
    }
}

enum FilesystemBackend {
    Directory(OverlayFs),
    Redb(SnapshotFilesystem),
}

fn prepare(
    mut config: OverlayMountConfig,
) -> Result<(FilesystemBackend, PathBuf, Vec<MountOption>)> {
    if config.lower_dirs.is_empty() {
        bail!("lowerdir must list at least one path");
    }
    match &config.upper {
        UpperBackend::Directory {
            upper_dir,
            work_dir,
        } => {
            std::fs::create_dir_all(upper_dir)
                .with_context(|| format!("create upperdir {}", upper_dir.display()))?;
            if let Some(work) = work_dir {
                std::fs::create_dir_all(work)
                    .with_context(|| format!("create workdir {}", work.display()))?;
            }
        }
        UpperBackend::Redb { database_path } => {
            if let Some(parent) = database_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create database parent {}", parent.display()))?;
            }
        }
    }
    let fskit = config.backend.as_deref() == Some("fskit");
    if let Some(backend) = &config.backend {
        if !matches!(backend.as_str(), "kernel" | "fskit") {
            bail!("unsupported macFUSE backend: {backend}");
        }
    }
    if !fskit {
        std::fs::create_dir_all(&config.mountpoint)
            .with_context(|| format!("create mountpoint {}", config.mountpoint.display()))?;
    }

    config.upper = match config.upper {
        UpperBackend::Directory {
            upper_dir,
            work_dir,
        } => UpperBackend::Directory {
            upper_dir: std::fs::canonicalize(upper_dir)?,
            work_dir: work_dir
                .map(std::fs::canonicalize)
                .transpose()
                .context("canonicalize workdir")?,
        },
        UpperBackend::Redb { database_path } => {
            let database_path = if database_path.exists() {
                std::fs::canonicalize(database_path)?
            } else {
                let parent = database_path
                    .parent()
                    .context("database path must have a parent")?;
                let name = database_path
                    .file_name()
                    .context("database path must have a file name")?;
                std::fs::canonicalize(parent)?.join(name)
            };
            UpperBackend::Redb { database_path }
        }
    };
    config.lower_dirs = config
        .lower_dirs
        .into_iter()
        .map(std::fs::canonicalize)
        .collect::<std::io::Result<Vec<_>>>()
        .context("canonicalize lowerdir")?;
    let mountpoint = if fskit && !config.mountpoint.exists() {
        let parent = config
            .mountpoint
            .parent()
            .context("FSKit mountpoint must have a parent")?;
        let name = config
            .mountpoint
            .file_name()
            .context("FSKit mountpoint must have a final component")?;
        std::fs::canonicalize(parent)?.join(name)
    } else {
        std::fs::canonicalize(&config.mountpoint)?
    };
    if fskit && !mountpoint.starts_with("/Volumes") {
        bail!("macFUSE FSKit mountpoints must be under /Volumes");
    }

    for lower in &config.lower_dirs {
        if !lower.is_dir() {
            bail!("lowerdir is not a directory: {}", lower.display());
        }
        let upper_overlaps = match &config.upper {
            UpperBackend::Directory { upper_dir, .. } => {
                upper_dir.starts_with(lower) || lower.starts_with(upper_dir)
            }
            UpperBackend::Redb { database_path } => database_path.starts_with(lower),
        };
        if upper_overlaps || mountpoint.starts_with(lower) || lower.starts_with(&mountpoint) {
            bail!(
                "lowerdir must not overlap upperdir or mountpoint: {}",
                lower.display()
            );
        }
    }
    if let UpperBackend::Directory {
        upper_dir,
        work_dir,
    } = &config.upper
    {
        if mountpoint.starts_with(upper_dir) || upper_dir.starts_with(&mountpoint) {
            bail!("upperdir and mountpoint must not overlap");
        }
        if let Some(work) = work_dir {
            if std::fs::metadata(upper_dir)?.dev() != std::fs::metadata(work)?.dev() {
                bail!(
                    "upperdir and workdir must be on the same filesystem: {} and {}",
                    upper_dir.display(),
                    work.display()
                );
            }
            if upper_dir == work {
                bail!("upperdir and workdir must be different directories");
            }
            if mountpoint.starts_with(work)
                || work.starts_with(&mountpoint)
                || config
                    .lower_dirs
                    .iter()
                    .any(|lower| work.starts_with(lower) || lower.starts_with(work))
            {
                bail!("workdir must not overlap lowerdir or mountpoint");
            }
        }
    }

    let filesystem = match config.upper {
        UpperBackend::Directory {
            upper_dir,
            work_dir,
        } => FilesystemBackend::Directory(OverlayFs::new(config.lower_dirs, upper_dir, work_dir)?),
        UpperBackend::Redb { database_path } => {
            FilesystemBackend::Redb(SnapshotFilesystem::new(config.lower_dirs, database_path)?)
        }
    };
    let mut options = vec![MountOption::FSName(config.fsname)];
    if config.debug {
        options.push(MountOption::CUSTOM("debug".into()));
    }
    if let Some(backend) = config.backend {
        options.push(MountOption::CUSTOM(format!("backend={backend}")));
    }
    if config.default_permissions {
        options.push(MountOption::DefaultPermissions);
    }
    if config.allow_other {
        options.push(MountOption::AllowOther);
    }
    if config.allow_root {
        options.push(MountOption::AllowRoot);
    }
    if config.read_only {
        options.push(MountOption::RO);
    }
    Ok((filesystem, mountpoint, options))
}

fn is_mountpoint(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return true;
    };
    let Ok(parent_metadata) = std::fs::metadata(parent) else {
        return false;
    };
    metadata.dev() != parent_metadata.dev()
        || (metadata.dev() == parent_metadata.dev() && metadata.ino() == parent_metadata.ino())
}

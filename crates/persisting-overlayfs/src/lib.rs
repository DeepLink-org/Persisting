//! Embeddable cross-platform FUSE overlay implementation.
//!
//! pVisor owns [`OverlaySession`] directly, so the pVisor process is also the
//! FUSE userspace server. The `persisting-overlayfs` binary is only a debugging
//! and manual-mount CLI wrapper around this library.

mod fs;
#[cfg(feature = "jujutsu")]
mod jj_backend;
#[cfg(not(feature = "jujutsu"))]
#[path = "jj_disabled.rs"]
mod jj_backend;

use anyhow::{Context, Result, bail};
use fs::OverlayFs;
use fuser::{BackgroundSession, MountOption, Session};
use jj_backend::JujutsuWorkspace;
pub use jj_backend::{jujutsu_upper_dir, prepare_jujutsu_upper, snapshot_jujutsu_upper};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum UpperBackend {
    Directory {
        upper_dir: PathBuf,
        work_dir: Option<PathBuf>,
    },
    Jujutsu {
        store_path: PathBuf,
        workspace: String,
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
    /// Optional durable first-touch journal used to reject apply conflicts.
    pub preimage_dir: Option<PathBuf>,
    /// Paths relative to the overlay root that are absent from the mounted
    /// namespace. Exclusions apply to every lower and the writable upper and
    /// cannot be recreated from inside the mount.
    pub excluded_paths: Vec<PathBuf>,
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
            preimage_dir: None,
            excluded_paths: Vec::new(),
        }
    }

    pub fn new_jujutsu(
        lower_dirs: Vec<PathBuf>,
        store_path: PathBuf,
        workspace: String,
        mountpoint: PathBuf,
    ) -> Self {
        let mut config = Self::new(lower_dirs, PathBuf::new(), None, mountpoint);
        config.upper = UpperBackend::Jujutsu {
            store_path,
            workspace,
        };
        config
    }
}

#[derive(Debug)]
pub struct OverlaySession {
    background: Option<BackgroundSession>,
    jujutsu: Option<JujutsuWorkspace>,
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
        if let Some(background) = self.background.take() {
            background
                .unmount()
                .context("unmount FUSE session and stop request loop")?;
            for _ in 0..250 {
                if !is_mountpoint(&self.mountpoint) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if is_mountpoint(&self.mountpoint) {
                bail!("FUSE mount did not detach: {}", self.mountpoint.display());
            }
        }
        if let Some(workspace) = self.jujutsu.take() {
            workspace
                .snapshot()
                .context("snapshot Jujutsu overlay workspace")?;
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
    let (filesystem, mountpoint, options, jujutsu) = prepare(config)?;
    let session = Session::new(filesystem, &mountpoint, &options)
        .with_context(|| format!("mount {}", mountpoint.display()))?;
    let background = BackgroundSession::new(session).context("start FUSE request loop")?;
    log::info!("persisting-overlayfs mounted at {}", mountpoint.display());
    Ok(OverlaySession {
        background: Some(background),
        jujutsu,
        mountpoint,
    })
}

pub fn run_foreground(config: OverlayMountConfig) -> Result<()> {
    let (filesystem, mountpoint, options, jujutsu) = prepare(config)?;
    log::info!("persisting-overlayfs mounted at {}", mountpoint.display());
    let mut session = Session::new(filesystem, &mountpoint, &options)
        .with_context(|| format!("mount {}", mountpoint.display()))?;
    session.run().context("FUSE session")?;
    if let Some(workspace) = jujutsu {
        workspace
            .snapshot()
            .context("snapshot Jujutsu overlay workspace")?;
    }
    Ok(())
}

fn prepare(
    mut config: OverlayMountConfig,
) -> Result<(
    OverlayFs,
    PathBuf,
    Vec<MountOption>,
    Option<JujutsuWorkspace>,
)> {
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
        UpperBackend::Jujutsu { store_path, .. } => {
            std::fs::create_dir_all(store_path)
                .with_context(|| format!("create Jujutsu store {}", store_path.display()))?;
        }
    }
    let fskit = config.backend.as_deref() == Some("fskit");
    if let Some(backend) = &config.backend
        && !matches!(backend.as_str(), "kernel" | "fskit")
    {
        bail!("unsupported macFUSE backend: {backend}");
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
        UpperBackend::Jujutsu {
            store_path,
            workspace,
        } => UpperBackend::Jujutsu {
            store_path: std::fs::canonicalize(store_path)?,
            workspace,
        },
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

    let hidden_from_lower = |lower: &Path, candidate: &Path| {
        candidate.strip_prefix(lower).is_ok_and(|relative| {
            !relative.as_os_str().is_empty()
                && config
                    .excluded_paths
                    .iter()
                    .any(|hidden| relative == hidden || relative.starts_with(hidden))
        })
    };
    for lower in &config.lower_dirs {
        if !lower.is_dir() {
            bail!("lowerdir is not a directory: {}", lower.display());
        }
        let upper_overlaps = match &config.upper {
            UpperBackend::Directory { upper_dir, .. } => {
                (upper_dir.starts_with(lower) && !hidden_from_lower(lower, upper_dir))
                    || lower.starts_with(upper_dir)
            }
            UpperBackend::Jujutsu { store_path, .. } => {
                (store_path.starts_with(lower) && !hidden_from_lower(lower, store_path))
                    || lower.starts_with(store_path)
            }
        };
        let mount_overlaps = (mountpoint.starts_with(lower)
            && !hidden_from_lower(lower, &mountpoint))
            || lower.starts_with(&mountpoint);
        if upper_overlaps || mount_overlaps {
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
                || config.lower_dirs.iter().any(|lower| {
                    (work.starts_with(lower) && !hidden_from_lower(lower, work))
                        || lower.starts_with(work)
                })
            {
                bail!("workdir must not overlap lowerdir or mountpoint");
            }
        }
    }

    let mut jujutsu = None;
    let preimage_dir = config.preimage_dir;
    let filesystem = match config.upper {
        UpperBackend::Directory {
            upper_dir,
            work_dir,
        } => {
            if config.excluded_paths.is_empty() && preimage_dir.is_none() {
                OverlayFs::new(config.lower_dirs, upper_dir, work_dir)?
            } else {
                OverlayFs::new_with_exclusions_and_preimages(
                    config.lower_dirs,
                    upper_dir,
                    work_dir,
                    config.excluded_paths,
                    preimage_dir,
                )?
            }
        }
        UpperBackend::Jujutsu {
            store_path,
            workspace,
        } => {
            let workspace = JujutsuWorkspace::open(store_path, workspace, config.read_only)?;
            let upper_dir = workspace.upper_dir().to_path_buf();
            let filesystem = if config.excluded_paths.is_empty() && preimage_dir.is_none() {
                OverlayFs::new(config.lower_dirs, upper_dir, None)?
            } else {
                OverlayFs::new_with_exclusions_and_preimages(
                    config.lower_dirs,
                    upper_dir,
                    None,
                    config.excluded_paths,
                    preimage_dir,
                )?
            };
            if !config.read_only {
                jujutsu = Some(workspace);
            }
            filesystem
        }
    };
    // Access time is not part of a pVisor changeset. Disabling it also avoids
    // macFUSE issuing read-induced SETATTR requests that would otherwise force
    // lower files into the writable upper.
    let mut options = vec![MountOption::FSName(config.fsname), MountOption::NoAtime];
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
    Ok((filesystem, mountpoint, options, jujutsu))
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

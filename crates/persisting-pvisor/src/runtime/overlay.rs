//! In-process FUSE overlay mount + staging apply/discard.
//!
//! ```text
//! target (RO lower / apply destination)
//!    +
//! staging/upper (writable deltas)
//!    →
//! staging/merged  (Agent cwd)
//!
//! After the Attempt: unmount, keep staging.
//! Review → apply_overlay (upper → target) | discard_overlay
//! ```
//!
use super::implant::OverlayHint;
use persisting_gateway::config::{OverlayBackend, OverlayConfig};
use persisting_overlayfs::{
    apply_redb_upper, discard_redb_upper, mount as mount_embedded_overlay, redb_upper_status,
    OverlayMountConfig, OverlaySession,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{CString, OsStr};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

const META_FILENAME: &str = "overlay.json";
const WHITEOUT_PREFIX: &str = ".wh.";
const OPAQUE_WHITEOUT: &str = ".wh..wh..opq";
const OPAQUE_XATTRS: [&[u8]; 3] = [
    b"trusted.overlay.opaque",
    b"user.overlay.opaque",
    b"user.fuseoverlayfs.opaque",
];

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("overlay enabled but no target / lower_dirs configured")]
    MissingTarget,
    #[error("invalid overlay upper configuration: {0}")]
    InvalidConfig(String),
    #[error("overlay meta missing or invalid at {0}")]
    Meta(String),
    #[error("failed to prepare overlay directories: {0}")]
    Prepare(#[source] std::io::Error),
    #[error("embedded FUSE mount failed: {0}")]
    Mount(String),
    #[error("merged mount point not ready: {0}")]
    NotReady(String),
    #[error("overlay apply failed: {0}")]
    Apply(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Durable record of one overlay staging workspace (survives Attempt teardown).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayRecord {
    pub id: String,
    /// Target filesystem (apply destination + primary lower).
    pub target: PathBuf,
    pub upper: OverlayUpper,
    pub merged_dir: PathBuf,
    pub stage_dir: PathBuf,
    pub auto_apply: bool,
    #[serde(default)]
    pub auto_discard: bool,
    pub state: OverlayState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlayUpper {
    Directory {
        upper_dir: PathBuf,
        work_dir: PathBuf,
    },
    Redb {
        database_path: PathBuf,
    },
}

impl OverlayUpper {
    pub fn path(&self) -> &Path {
        match self {
            Self::Directory { upper_dir, .. } => upper_dir,
            Self::Redb { database_path } => database_path,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverlayState {
    /// Mounted / Agent may write.
    Active,
    /// Unmounted; upper retained for review.
    Staged,
    /// Upper applied onto target.
    Applied,
    /// Upper discarded.
    Discarded,
}

/// Summary of files present in upper (not a full recursive diff vs target).
#[derive(Debug, Clone)]
pub struct OverlayStatus {
    pub changed_files: usize,
    pub whiteouts: usize,
    pub sample_paths: Vec<String>,
}

/// Live in-process FUSE mount; unmounted on [`Self::unmount`] / Drop.
/// Staging directories are **not** deleted on unmount.
pub struct OverlayMount {
    record: OverlayRecord,
    session: Option<OverlaySession>,
}

/// Independent kernel-enforced read-only view used by `pvisor inspect`.
pub struct ReadOnlyOverlayMount {
    session: Option<OverlaySession>,
    mountpoint: PathBuf,
}

impl ReadOnlyOverlayMount {
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    pub fn unmount(mut self) -> anyhow::Result<()> {
        self.unmount_inner()
    }

    fn unmount_inner(&mut self) -> anyhow::Result<()> {
        if let Some(session) = self.session.take() {
            session.unmount()?;
        }
        if self.mountpoint.is_dir() {
            let _ = fs::remove_dir(&self.mountpoint);
        }
        Ok(())
    }
}

impl Drop for ReadOnlyOverlayMount {
    fn drop(&mut self) {
        let _ = self.unmount_inner();
    }
}

impl OverlayMount {
    pub fn record(&self) -> &OverlayRecord {
        &self.record
    }

    /// Unmount and mark staging as [`OverlayState::Staged`] (keep upper).
    pub fn unmount(mut self) -> anyhow::Result<OverlayRecord> {
        self.unmount_inner()?;
        self.record.state = OverlayState::Staged;
        write_overlay_record(&self.record)?;
        Ok(self.record.clone())
    }

    fn unmount_inner(&mut self) -> anyhow::Result<()> {
        if let Some(session) = self.session.take() {
            session.unmount()?;
        }
        Ok(())
    }
}

impl Drop for OverlayMount {
    fn drop(&mut self) {
        let _ = self.unmount_inner();
        if self.record.state == OverlayState::Active {
            self.record.state = OverlayState::Staged;
            let _ = write_overlay_record(&self.record);
        }
    }
}

/// Resolve config into concrete paths (target + staging layout).
pub fn resolve_overlay_workspace(
    cfg: &OverlayConfig,
    storage: &Path,
    session_id: &str,
) -> Result<Option<OverlayRecord>, OverlayError> {
    if !cfg.enabled && cfg.target.is_none() && cfg.lower_dirs.is_empty() {
        return Ok(None);
    }
    match cfg.backend {
        OverlayBackend::Redb if cfg.upper_dir.is_some() || cfg.work_dir.is_some() => {
            return Err(OverlayError::InvalidConfig(
                "redb cannot be combined with upper_dir or work_dir".into(),
            ));
        }
        OverlayBackend::Directory if cfg.database_path.is_some() => {
            return Err(OverlayError::InvalidConfig(
                "directory cannot be combined with database_path".into(),
            ));
        }
        _ => {}
    }

    let resolve = |p: &str| -> PathBuf {
        let path = PathBuf::from(p);
        if path.is_absolute() {
            path
        } else {
            storage.join(path)
        }
    };

    let target = if let Some(t) = &cfg.target {
        resolve(t)
    } else if let Some(first) = cfg.lower_dirs.first() {
        resolve(first)
    } else {
        return Err(OverlayError::MissingTarget);
    };

    let stage_dir = cfg
        .stage_dir
        .as_deref()
        .map(resolve)
        .unwrap_or_else(|| storage.join(".overlay").join(session_id));

    let upper = match cfg.backend {
        OverlayBackend::Directory => OverlayUpper::Directory {
            upper_dir: cfg
                .upper_dir
                .as_deref()
                .map(resolve)
                .unwrap_or_else(|| stage_dir.join("upper")),
            work_dir: cfg
                .work_dir
                .as_deref()
                .map(resolve)
                .unwrap_or_else(|| stage_dir.join("work")),
        },
        OverlayBackend::Redb => OverlayUpper::Redb {
            database_path: cfg
                .database_path
                .as_deref()
                .map(resolve)
                .unwrap_or_else(|| stage_dir.join("upper.redb")),
        },
    };
    let merged = cfg
        .merged_dir
        .as_deref()
        .map(resolve)
        .unwrap_or_else(|| stage_dir.join("merged"));

    Ok(Some(OverlayRecord {
        id: session_id.to_string(),
        target,
        upper,
        merged_dir: merged,
        stage_dir,
        auto_apply: cfg.auto_apply,
        auto_discard: cfg.auto_discard,
        state: OverlayState::Active,
    }))
}

/// Build an [`OverlayHint`] from a resolved record + full lower stack.
pub fn hint_from_record(record: &OverlayRecord, lower_dirs: Vec<PathBuf>) -> OverlayHint {
    let (upper_dir, work_dir) = match &record.upper {
        OverlayUpper::Directory {
            upper_dir,
            work_dir,
        } => (Some(upper_dir.clone()), Some(work_dir.clone())),
        OverlayUpper::Redb { .. } => (None, None),
    };
    OverlayHint {
        lower_dirs,
        stage_dir: Some(record.stage_dir.clone()),
        upper_dir,
        work_dir,
        merged_dir: Some(record.merged_dir.clone()),
        backend: match &record.upper {
            OverlayUpper::Directory { .. } => OverlayBackend::Directory,
            OverlayUpper::Redb { .. } => OverlayBackend::Redb,
        },
        auto_apply: record.auto_apply,
        auto_discard: record.auto_discard,
    }
}

/// Lower stack for mount: target first (top), then additional lower layers.
pub fn lower_stack_from_config(cfg: &OverlayConfig, storage: &Path, target: &Path) -> Vec<PathBuf> {
    let resolve = |p: &str| -> PathBuf {
        let path = PathBuf::from(p);
        if path.is_absolute() {
            path
        } else {
            storage.join(path)
        }
    };
    let mut lowers: Vec<PathBuf> = cfg.lower_dirs.iter().map(|p| resolve(p)).collect();
    lowers.retain(|p| p != target);
    lowers.insert(0, target.to_path_buf());
    lowers
}

/// Mount the overlay in-process; pVisor becomes the FUSE userspace server.
pub fn mount_overlay_record(
    record: &OverlayRecord,
    lower_dirs: &[PathBuf],
) -> Result<OverlayMount, OverlayError> {
    if lower_dirs.is_empty() {
        return Err(OverlayError::MissingTarget);
    }
    for dir in lower_dirs
        .iter()
        .chain([&record.merged_dir, &record.stage_dir])
    {
        fs::create_dir_all(dir).map_err(OverlayError::Prepare)?;
    }
    match &record.upper {
        OverlayUpper::Directory {
            upper_dir,
            work_dir,
        } => {
            fs::create_dir_all(upper_dir).map_err(OverlayError::Prepare)?;
            fs::create_dir_all(work_dir).map_err(OverlayError::Prepare)?;
        }
        OverlayUpper::Redb { database_path } => {
            if let Some(parent) = database_path.parent() {
                fs::create_dir_all(parent).map_err(OverlayError::Prepare)?;
            }
        }
    }

    let mut config = match &record.upper {
        OverlayUpper::Directory {
            upper_dir,
            work_dir,
        } => OverlayMountConfig::new(
            lower_dirs.to_vec(),
            upper_dir.clone(),
            Some(work_dir.clone()),
            record.merged_dir.clone(),
        ),
        OverlayUpper::Redb { database_path } => OverlayMountConfig::new_redb(
            lower_dirs.to_vec(),
            database_path.clone(),
            record.merged_dir.clone(),
        ),
    };
    config.fsname = format!("pvisor-{}", record.id);
    let session =
        mount_embedded_overlay(config).map_err(|error| OverlayError::Mount(error.to_string()))?;
    wait_merged_ready(&record.merged_dir, &session)?;

    let mut record = record.clone();
    record.state = OverlayState::Active;
    write_overlay_record(&record)?;

    Ok(OverlayMount {
        record,
        session: Some(session),
    })
}

/// Mount the same lower/upper projection without permitting any mutation.
/// The kernel's read-only FUSE mount rejects writes before they reach the
/// writable overlay implementation.
pub fn mount_overlay_record_read_only(
    record: &OverlayRecord,
    lower_dirs: &[PathBuf],
    mountpoint: &Path,
) -> Result<ReadOnlyOverlayMount, OverlayError> {
    if lower_dirs.is_empty() {
        return Err(OverlayError::MissingTarget);
    }
    fs::create_dir_all(mountpoint).map_err(OverlayError::Prepare)?;
    let mut config = match &record.upper {
        OverlayUpper::Directory {
            upper_dir,
            work_dir,
        } => OverlayMountConfig::new(
            lower_dirs.to_vec(),
            upper_dir.clone(),
            Some(work_dir.clone()),
            mountpoint.to_path_buf(),
        ),
        OverlayUpper::Redb { database_path } => OverlayMountConfig::new_redb(
            lower_dirs.to_vec(),
            database_path.clone(),
            mountpoint.to_path_buf(),
        ),
    };
    config.fsname = format!("pvisor-inspect-{}", record.id);
    config.read_only = true;
    let session =
        mount_embedded_overlay(config).map_err(|error| OverlayError::Mount(error.to_string()))?;
    wait_merged_ready(mountpoint, &session)?;
    Ok(ReadOnlyOverlayMount {
        session: Some(session),
        mountpoint: mountpoint.to_path_buf(),
    })
}

pub fn overlay_meta_path(stage_dir: &Path) -> PathBuf {
    stage_dir.join(META_FILENAME)
}

pub fn write_overlay_record(record: &OverlayRecord) -> Result<(), OverlayError> {
    fs::create_dir_all(&record.stage_dir)?;
    let path = overlay_meta_path(&record.stage_dir);
    let body = serde_json::to_string_pretty(record)
        .map_err(|e| OverlayError::Apply(format!("serialize meta: {e}")))?;
    fs::write(path, body)?;
    Ok(())
}

pub fn load_overlay_record(stage_dir: &Path) -> Result<OverlayRecord, OverlayError> {
    let path = overlay_meta_path(stage_dir);
    let raw =
        fs::read_to_string(&path).map_err(|_| OverlayError::Meta(path.display().to_string()))?;
    serde_json::from_str(&raw).map_err(|e| OverlayError::Meta(format!("{}: {e}", path.display())))
}

pub fn overlay_status(record: &OverlayRecord) -> Result<OverlayStatus, OverlayError> {
    if let OverlayUpper::Redb { database_path } = &record.upper {
        let status = redb_upper_status(database_path)
            .map_err(|error| OverlayError::Apply(error.to_string()))?;
        return Ok(OverlayStatus {
            changed_files: status.changed_paths,
            whiteouts: status.whiteouts,
            sample_paths: status
                .sample_paths
                .into_iter()
                .map(|path| path.display().to_string())
                .collect(),
        });
    }
    let OverlayUpper::Directory { upper_dir, .. } = &record.upper else {
        unreachable!()
    };
    let mut changed = 0usize;
    let mut whiteouts = 0usize;
    let mut sample = Vec::new();
    if upper_dir.is_dir() {
        walk_upper(upper_dir, upper_dir, &mut |rel, is_wh| {
            if is_wh {
                whiteouts += 1;
            } else {
                changed += 1;
            }
            if sample.len() < 32 {
                sample.push(rel.display().to_string());
            }
            Ok(())
        })?;
    }
    Ok(OverlayStatus {
        changed_files: changed,
        whiteouts,
        sample_paths: sample,
    })
}

/// Merge staging upper onto `target` (handles fuse-overlayfs `.wh.` whiteouts).
pub fn apply_overlay(record: &mut OverlayRecord) -> Result<(), OverlayError> {
    if matches!(
        record.state,
        OverlayState::Applied | OverlayState::Discarded
    ) {
        return Err(OverlayError::Apply(format!(
            "overlay {} is already {:?}",
            record.id, record.state
        )));
    }
    match &record.upper {
        OverlayUpper::Redb { database_path } => {
            apply_redb_upper(database_path, &record.target)
                .map_err(|error| OverlayError::Apply(error.to_string()))?;
            discard_redb_upper(database_path)
                .map_err(|error| OverlayError::Apply(error.to_string()))?;
        }
        OverlayUpper::Directory { upper_dir, .. } => {
            if upper_dir.is_dir() {
                apply_upper_onto_target(upper_dir, &record.target)?;
                fs::remove_dir_all(upper_dir)?;
                fs::create_dir_all(upper_dir)?;
            }
        }
    }
    record.state = OverlayState::Applied;
    write_overlay_record(record)?;
    Ok(())
}

/// Drop staging upper (and optionally the whole stage dir contents except meta).
pub fn discard_overlay(record: &mut OverlayRecord) -> Result<(), OverlayError> {
    if record.state == OverlayState::Applied {
        return Err(OverlayError::Apply(format!(
            "overlay {} already applied; nothing to discard",
            record.id
        )));
    }
    match &record.upper {
        OverlayUpper::Directory {
            upper_dir,
            work_dir,
        } => {
            if upper_dir.exists() {
                fs::remove_dir_all(upper_dir)?;
            }
            if work_dir.exists() {
                let _ = fs::remove_dir_all(work_dir);
            }
        }
        OverlayUpper::Redb { database_path } => {
            discard_redb_upper(database_path)?;
        }
    }
    record.state = OverlayState::Discarded;
    write_overlay_record(record)?;
    Ok(())
}

fn apply_upper_onto_target(upper: &Path, target: &Path) -> Result<(), OverlayError> {
    ensure_directory(target)?;
    let mut hard_links = HashMap::new();
    apply_directory(upper, target, &mut hard_links, false)
}

fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => {
            remove_path(path)?;
            fs::create_dir(path)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(path),
        Err(error) => Err(error),
    }
}

fn whiteout_target(name: &OsStr) -> Option<&OsStr> {
    let bytes = name.as_bytes();
    bytes
        .strip_prefix(WHITEOUT_PREFIX.as_bytes())
        .filter(|stripped| !stripped.is_empty())
        .map(OsStr::from_bytes)
}

fn apply_directory(
    source: &Path,
    destination: &Path,
    hard_links: &mut HashMap<(u64, u64), PathBuf>,
    preserve_metadata: bool,
) -> Result<(), OverlayError> {
    ensure_directory(destination)?;
    let opaque = path_exists(&source.join(OPAQUE_WHITEOUT)) || has_opaque_xattr(source);
    if opaque {
        for entry in fs::read_dir(destination)? {
            remove_path(&entry?.path())?;
        }
    }

    let entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    // Whiteouts are processed first, independent of host readdir order.
    for entry in &entries {
        let name = entry.file_name();
        if name == OPAQUE_WHITEOUT {
            continue;
        }
        if let Some(victim) = whiteout_target(&name) {
            remove_path(&destination.join(victim))?;
        }
    }
    for entry in entries {
        let name = entry.file_name();
        if name.as_bytes().starts_with(WHITEOUT_PREFIX.as_bytes()) {
            continue;
        }
        copy_upper_entry(&entry.path(), &destination.join(name), hard_links)?;
    }
    if preserve_metadata {
        copy_host_metadata(source, destination)?;
    }
    Ok(())
}

fn copy_upper_entry(
    source: &Path,
    destination: &Path,
    hard_links: &mut HashMap<(u64, u64), PathBuf>,
) -> Result<(), OverlayError> {
    let metadata = fs::symlink_metadata(source)?;
    let kind = metadata.file_type();
    if kind.is_dir() {
        ensure_directory(destination)?;
        return apply_directory(source, destination, hard_links, true);
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_path(destination)?;
    if kind.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(source)?, destination)?;
    } else if kind.is_file() {
        let identity = (metadata.dev(), metadata.ino());
        if metadata.nlink() > 1 {
            if let Some(existing) = hard_links.get(&identity) {
                fs::hard_link(existing, destination)?;
                return Ok(());
            }
        }
        fs::copy(source, destination)?;
        if metadata.nlink() > 1 {
            hard_links.insert(identity, destination.to_path_buf());
        }
    } else {
        let path = c_path(destination)?;
        // SAFETY: path is NUL terminated and points to valid storage for this call.
        let rc = unsafe {
            libc::mknod(
                path.as_ptr(),
                metadata.mode() as libc::mode_t,
                metadata.rdev() as libc::dev_t,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    copy_host_metadata(source, destination)?;
    Ok(())
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn copy_host_metadata(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    let nofollow = metadata.file_type().is_symlink();
    let source = c_path(source)?;
    let destination_c = c_path(destination)?;

    // Preserve ownership where permitted. An unprivileged apply still preserves
    // all metadata it is allowed to own instead of failing the whole transaction.
    let flags = if nofollow {
        libc::AT_SYMLINK_NOFOLLOW
    } else {
        0
    };
    // SAFETY: both C strings and syscall arguments remain valid for each call.
    let chown_rc = unsafe {
        libc::fchownat(
            libc::AT_FDCWD,
            destination_c.as_ptr(),
            metadata.uid(),
            metadata.gid(),
            flags,
        )
    };
    if chown_rc != 0 {
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EPERM) | Some(libc::EACCES)) {
            return Err(error);
        }
    }
    if !nofollow {
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.mode() & 0o7777),
        )?;
    }
    copy_host_xattrs(&source, &destination_c)?;

    let times = [
        libc::timespec {
            tv_sec: metadata.atime(),
            tv_nsec: metadata.atime_nsec(),
        },
        libc::timespec {
            tv_sec: metadata.mtime(),
            tv_nsec: metadata.mtime_nsec(),
        },
    ];
    // SAFETY: destination and times are valid for the duration of the call.
    let rc = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            destination_c.as_ptr(),
            times.as_ptr(),
            flags,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if nofollow
            && matches!(
                error.raw_os_error(),
                Some(libc::ENOTSUP) | Some(libc::EPERM)
            )
        {
            Ok(())
        } else {
            Err(error)
        }
    }
}

fn copy_host_xattrs(source: &CString, destination: &CString) -> io::Result<()> {
    let names = list_host_xattrs(source)?;
    let destination_names = list_host_xattrs(destination)?;
    for name in destination_names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        if OPAQUE_XATTRS.contains(&name)
            || !names
                .split(|byte| *byte == 0)
                .any(|source_name| source_name == name)
        {
            let name =
                CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
            if let Err(error) = remove_host_xattr(destination, &name) {
                if !matches!(
                    error.raw_os_error(),
                    Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::ENOTSUP)
                ) {
                    return Err(error);
                }
            }
        }
    }
    for name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty() && !OPAQUE_XATTRS.contains(name))
    {
        let name = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let value = get_host_xattr(source, &name)?;
        if let Err(error) = set_host_xattr(destination, &name, &value) {
            if !matches!(
                error.raw_os_error(),
                Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::ENOTSUP)
            ) {
                return Err(error);
            }
        }
    }
    Ok(())
}

fn has_opaque_xattr(path: &Path) -> bool {
    let Ok(path) = c_path(path) else {
        return false;
    };
    OPAQUE_XATTRS.iter().any(|name| {
        let Ok(name) = CString::new(*name) else {
            return false;
        };
        get_host_xattr(&path, &name).is_ok_and(|value| value == b"y")
    })
}

fn remove_host_xattr(path: &CString, name: &CString) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    // SAFETY: both strings are NUL terminated and valid for this call.
    let rc = unsafe { libc::removexattr(path.as_ptr(), name.as_ptr(), libc::XATTR_NOFOLLOW) };
    #[cfg(not(target_os = "macos"))]
    // SAFETY: both strings are NUL terminated and valid for this call.
    let rc = unsafe { libc::lremovexattr(path.as_ptr(), name.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn list_host_xattrs(path: &CString) -> io::Result<Vec<u8>> {
    #[cfg(target_os = "macos")]
    let list = |buffer: *mut libc::c_char, size| unsafe {
        libc::listxattr(path.as_ptr(), buffer, size, libc::XATTR_NOFOLLOW)
    };
    #[cfg(not(target_os = "macos"))]
    let list =
        |buffer: *mut libc::c_char, size| unsafe { libc::llistxattr(path.as_ptr(), buffer, size) };
    let needed = list(std::ptr::null_mut(), 0);
    if needed < 0 {
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::ENOTSUP)) {
            return Ok(Vec::new());
        }
        return Err(error);
    }
    let mut names = vec![0; needed as usize];
    if !names.is_empty() {
        let actual = list(names.as_mut_ptr().cast(), names.len());
        if actual < 0 {
            return Err(io::Error::last_os_error());
        }
        names.truncate(actual as usize);
    }
    Ok(names)
}

fn get_host_xattr(path: &CString, name: &CString) -> io::Result<Vec<u8>> {
    #[cfg(target_os = "macos")]
    let get = |buffer: *mut libc::c_void, size| unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            buffer,
            size,
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let get = |buffer: *mut libc::c_void, size| unsafe {
        libc::lgetxattr(path.as_ptr(), name.as_ptr(), buffer, size)
    };
    let needed = get(std::ptr::null_mut(), 0);
    if needed < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut value = vec![0; needed as usize];
    if !value.is_empty() {
        let actual = get(value.as_mut_ptr().cast(), value.len());
        if actual < 0 {
            return Err(io::Error::last_os_error());
        }
        value.truncate(actual as usize);
    }
    Ok(value)
}

fn set_host_xattr(path: &CString, name: &CString, value: &[u8]) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let rc = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let rc = unsafe {
        libc::lsetxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn walk_upper(
    root: &Path,
    dir: &Path,
    visit: &mut dyn FnMut(PathBuf, bool) -> Result<(), OverlayError>,
) -> Result<(), OverlayError> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map_err(|e| OverlayError::Apply(e.to_string()))?
            .to_path_buf();
        let is_wh = path
            .file_name()
            .is_some_and(|name| name.as_bytes().starts_with(WHITEOUT_PREFIX.as_bytes()));
        if fs::symlink_metadata(&path)?.is_dir() && !is_wh {
            visit(rel.clone(), false)?;
            walk_upper(root, &path, visit)?;
        } else {
            visit(rel, is_wh)?;
        }
    }
    Ok(())
}

fn wait_merged_ready(merged: &Path, session: &OverlaySession) -> Result<(), OverlayError> {
    for _ in 0..50 {
        if session.has_exited() {
            return Err(OverlayError::Mount(
                "embedded FUSE request loop exited before mount became ready".into(),
            ));
        }
        if merged_root_is_ready(merged) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if merged_root_is_ready(merged) {
        return Ok(());
    }
    Err(OverlayError::NotReady(merged.display().to_string()))
}

fn merged_root_is_ready(path: &Path) -> bool {
    if !is_mountpoint(path) {
        return false;
    }
    // macFUSE may publish the mountpoint before its request loop can serve the
    // root directory. Probe opendir/readdir so the Agent never races the mount.
    match fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none_or(|entry| entry.is_ok()),
        Err(_) => false,
    }
}

fn is_mountpoint(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return true;
    };
    let Ok(parent_metadata) = fs::metadata(parent) else {
        return false;
    };
    if metadata.dev() != parent_metadata.dev()
        || (metadata.dev() == parent_metadata.dev() && metadata.ino() == parent_metadata.ino())
    {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        let target = path.display().to_string();
        if let Ok(mounts) = fs::read_to_string("/proc/self/mounts") {
            return mounts.lines().any(|line| {
                line.split_whitespace()
                    .nth(1)
                    .is_some_and(|mount| mount == target)
            });
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn resolve_uses_target_and_default_stage() {
        let cfg = OverlayConfig {
            enabled: true,
            target: Some("/proj".into()),
            ..OverlayConfig::default()
        };
        let rec = resolve_overlay_workspace(&cfg, Path::new("/tmp/store"), "run-1")
            .unwrap()
            .unwrap();
        assert_eq!(rec.target, PathBuf::from("/proj"));
        assert_eq!(rec.stage_dir, PathBuf::from("/tmp/store/.overlay/run-1"));
        assert_eq!(
            rec.upper,
            OverlayUpper::Redb {
                database_path: PathBuf::from("/tmp/store/.overlay/run-1/upper.redb")
            }
        );
    }

    #[test]
    fn resolve_rejects_parallel_upper_backends() {
        let cfg = OverlayConfig {
            enabled: true,
            target: Some("/proj".into()),
            upper_dir: Some("/upper".into()),
            ..OverlayConfig::default()
        };
        assert!(matches!(
            resolve_overlay_workspace(&cfg, Path::new("/tmp/store"), "run-1"),
            Err(OverlayError::InvalidConfig(_))
        ));
    }

    #[test]
    fn lower_stack_keeps_target_as_highest_priority_layer() {
        let cfg = OverlayConfig {
            lower_dirs: vec!["extra-a".into(), "extra-b".into()],
            ..OverlayConfig::default()
        };
        assert_eq!(
            lower_stack_from_config(&cfg, Path::new("/store"), Path::new("/target")),
            vec![
                PathBuf::from("/target"),
                PathBuf::from("/store/extra-a"),
                PathBuf::from("/store/extra-b"),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires an enabled macFUSE kernel extension"]
    fn embedded_mount_roundtrip() {
        let tmp = tempdir().unwrap();
        let lower = tmp.path().join("lower");
        let stage = tmp.path().join("stage");
        let database = stage.join("upper.redb");
        let merged = stage.join("merged");
        fs::create_dir_all(&lower).unwrap();
        fs::write(lower.join("lower-file"), b"lower").unwrap();
        fs::write(lower.join("deleted-file"), b"delete me").unwrap();
        let record = OverlayRecord {
            id: "embedded-e2e".into(),
            target: lower.clone(),
            upper: OverlayUpper::Redb {
                database_path: database.clone(),
            },
            merged_dir: merged.clone(),
            stage_dir: stage.clone(),
            auto_apply: false,
            auto_discard: false,
            state: OverlayState::Staged,
        };

        let mount = mount_overlay_record(&record, std::slice::from_ref(&lower)).unwrap();
        assert_eq!(fs::read(merged.join("lower-file")).unwrap(), b"lower");
        fs::write(merged.join("lower-file"), b"copied-up").unwrap();
        fs::remove_file(merged.join("deleted-file")).unwrap();
        fs::write(merged.join("created"), b"upper").unwrap();
        fs::hard_link(merged.join("created"), merged.join("created-link")).unwrap();
        fs::create_dir(merged.join("new-dir")).unwrap();
        fs::write(merged.join("new-dir/before-rename"), b"nested").unwrap();
        fs::rename(
            merged.join("new-dir/before-rename"),
            merged.join("new-dir/after-rename"),
        )
        .unwrap();
        std::os::unix::fs::symlink("created", merged.join("created-symlink")).unwrap();
        assert!(database.is_file());
        assert!(!stage.join("upper").exists());
        let mut record = mount.unmount().unwrap();
        assert_eq!(record.state, OverlayState::Staged);
        assert!(!is_mountpoint(&merged));
        let status = overlay_status(&record).unwrap();
        assert!(status.changed_files >= 5);
        assert_eq!(status.whiteouts, 1);
        apply_overlay(&mut record).unwrap();
        assert_eq!(record.state, OverlayState::Applied);
        assert_eq!(fs::read(lower.join("lower-file")).unwrap(), b"copied-up");
        assert!(!lower.join("deleted-file").exists());
        assert_eq!(fs::read(lower.join("created")).unwrap(), b"upper");
        assert_eq!(
            fs::read(lower.join("new-dir/after-rename")).unwrap(),
            b"nested"
        );
        assert_eq!(
            fs::read_link(lower.join("created-symlink")).unwrap(),
            PathBuf::from("created")
        );
        assert_eq!(
            fs::metadata(lower.join("created")).unwrap().ino(),
            fs::metadata(lower.join("created-link")).unwrap().ino()
        );
        assert!(!database.exists());
    }

    #[test]
    fn apply_copies_and_honors_whiteout() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let upper = tmp.path().join("upper");
        fs::create_dir_all(target.join("keep")).unwrap();
        fs::write(target.join("keep/a.txt"), b"old").unwrap();
        fs::write(target.join("gone.txt"), b"x").unwrap();
        fs::create_dir_all(upper.join("keep")).unwrap();
        fs::write(upper.join("keep/a.txt"), b"new").unwrap();
        fs::write(upper.join("keep/b.txt"), b"added").unwrap();
        fs::write(upper.join(".wh.gone.txt"), b"").unwrap();

        let mut rec = OverlayRecord {
            id: "t".into(),
            target: target.clone(),
            upper: OverlayUpper::Directory {
                upper_dir: upper,
                work_dir: tmp.path().join("work"),
            },
            merged_dir: tmp.path().join("merged"),
            stage_dir: tmp.path().to_path_buf(),
            auto_apply: false,
            auto_discard: false,
            state: OverlayState::Staged,
        };
        apply_overlay(&mut rec).unwrap();
        assert_eq!(
            fs::read_to_string(target.join("keep/a.txt")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(target.join("keep/b.txt")).unwrap(),
            "added"
        );
        assert!(!target.join("gone.txt").exists());
        assert_eq!(rec.state, OverlayState::Applied);
    }

    #[test]
    fn apply_honors_opaque_before_children_and_preserves_posix_types() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let upper = tmp.path().join("upper");
        fs::create_dir_all(target.join("replaced")).unwrap();
        fs::write(target.join("replaced/old"), b"old").unwrap();
        fs::create_dir_all(upper.join("replaced")).unwrap();
        fs::write(upper.join("replaced").join(OPAQUE_WHITEOUT), b"").unwrap();
        fs::write(upper.join("replaced/new"), b"new").unwrap();
        fs::set_permissions(
            upper.join("replaced/new"),
            fs::Permissions::from_mode(0o751),
        )
        .unwrap();
        std::os::unix::fs::symlink("new", upper.join("replaced/link")).unwrap();
        fs::hard_link(upper.join("replaced/new"), upper.join("replaced/hard-link")).unwrap();

        apply_upper_onto_target(&upper, &target).unwrap();

        assert!(!target.join("replaced/old").exists());
        assert_eq!(fs::read(target.join("replaced/new")).unwrap(), b"new");
        assert_eq!(
            fs::read_link(target.join("replaced/link")).unwrap(),
            PathBuf::from("new")
        );
        let original = fs::metadata(target.join("replaced/new")).unwrap();
        let linked = fs::metadata(target.join("replaced/hard-link")).unwrap();
        assert_eq!(original.ino(), linked.ino());
        assert_eq!(original.mode() & 0o777, 0o751);
    }

    #[test]
    fn status_does_not_follow_symlinked_directories() {
        let tmp = tempdir().unwrap();
        let upper = tmp.path().join("upper");
        fs::create_dir_all(&upper).unwrap();
        std::os::unix::fs::symlink(tmp.path(), upper.join("loop")).unwrap();
        let record = OverlayRecord {
            id: "t".into(),
            target: tmp.path().join("target"),
            upper: OverlayUpper::Directory {
                upper_dir: upper,
                work_dir: tmp.path().join("work"),
            },
            merged_dir: tmp.path().join("merged"),
            stage_dir: tmp.path().to_path_buf(),
            auto_apply: false,
            auto_discard: false,
            state: OverlayState::Staged,
        };
        let status = overlay_status(&record).unwrap();
        assert_eq!(status.changed_files, 1);
    }

    #[test]
    fn discard_clears_upper() {
        let tmp = tempdir().unwrap();
        let upper = tmp.path().join("upper");
        fs::create_dir_all(&upper).unwrap();
        fs::write(upper.join("x"), b"1").unwrap();
        let mut rec = OverlayRecord {
            id: "t".into(),
            target: tmp.path().join("target"),
            upper: OverlayUpper::Directory {
                upper_dir: upper.clone(),
                work_dir: tmp.path().join("work"),
            },
            merged_dir: tmp.path().join("merged"),
            stage_dir: tmp.path().to_path_buf(),
            auto_apply: false,
            auto_discard: false,
            state: OverlayState::Staged,
        };
        discard_overlay(&mut rec).unwrap();
        assert!(!upper.exists());
        assert_eq!(rec.state, OverlayState::Discarded);
    }
}

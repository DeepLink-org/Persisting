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
use crate::util::{atomic_write, create_dir_all_durable};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use persisting_gateway::config::{OverlayBackend, OverlayConfig};
use persisting_overlayfs::{
    jujutsu_upper_dir, mount as mount_embedded_overlay, snapshot_jujutsu_upper, OverlayMountConfig,
    OverlaySession,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::ffi::{CString, OsStr};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const META_FILENAME: &str = "overlay.json";
const APPLY_LEDGER_FILENAME: &str = "apply-ledger.json";
const APPLY_LEDGER_SCHEMA_VERSION: u32 = 1;
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
    #[error("{0}")]
    InvalidState(String),
    #[error("overlay metadata update failed: {0}")]
    Persist(String),
    #[error("overlay finalization failed: {0}")]
    Finalize(String),
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
    /// Paths relative to the overlay root that are inaccessible through the
    /// merged view. Root overlays use this to hide their own backing state.
    #[serde(default)]
    pub excluded_paths: Vec<PathBuf>,
    pub auto_apply: bool,
    #[serde(default)]
    pub auto_discard: bool,
    /// Immutable lower targets (for example OCI cache entries) reject apply.
    #[serde(default)]
    pub protect_target: bool,
    pub state: OverlayState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlayUpper {
    Directory {
        upper_dir: PathBuf,
        work_dir: PathBuf,
    },
    Jujutsu {
        store_path: PathBuf,
        workspace: String,
        upper_dir: PathBuf,
    },
}

impl OverlayUpper {
    pub fn path(&self) -> &Path {
        match self {
            Self::Directory { upper_dir, .. } => upper_dir,
            Self::Jujutsu { upper_dir, .. } => upper_dir,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    TypeChanged,
    Opaque,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeEntryType {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeEntry {
    pub path: String,
    pub kind: ChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_type: Option<ChangeEntryType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_type: Option<ChangeEntryType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

/// User-facing selection for one staged apply operation. Exact paths select
/// the path and its descendants; include/exclude values use git-style glob
/// matching against slash-separated paths relative to the overlay root.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplySelection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub includes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excludes: Vec<String>,
}

impl ApplySelection {
    pub fn is_all(&self) -> bool {
        self.paths.is_empty() && self.includes.is_empty() && self.excludes.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyRecord {
    pub schema_version: u32,
    pub apply_id: String,
    pub created_at_unix_ms: u64,
    pub overlay_id: String,
    pub target: PathBuf,
    pub selection: ApplySelection,
    pub changes: Vec<ChangeEntry>,
    pub remaining_changes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub apply_id: String,
    pub applied: Vec<ChangeEntry>,
    pub remaining: Vec<ChangeEntry>,
}

#[derive(Debug, Clone)]
pub struct ApplyPlan {
    pub selected: Vec<ChangeEntry>,
    selected_paths: BTreeSet<PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ApplyLedger {
    #[serde(default = "apply_ledger_schema_version")]
    schema_version: u32,
    #[serde(default)]
    records: Vec<ApplyRecord>,
}

fn apply_ledger_schema_version() -> u32 {
    APPLY_LEDGER_SCHEMA_VERSION
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
    pub fn mountpoint(&self) -> &Path {
        &self.record.merged_dir
    }

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
        if !self.record.merged_dir.starts_with(&self.record.stage_dir)
            && self.record.merged_dir.is_dir()
        {
            fs::remove_dir(&self.record.merged_dir)?;
            if let Some(parent) = self.record.merged_dir.parent() {
                let _ = fs::remove_dir(parent);
            }
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
        OverlayBackend::Directory
            if cfg.jujutsu_store_path.is_some() || cfg.jujutsu_workspace.is_some() =>
        {
            return Err(OverlayError::InvalidConfig(
                "directory cannot be combined with Jujutsu options".into(),
            ));
        }
        OverlayBackend::Jujutsu if cfg.upper_dir.is_some() || cfg.work_dir.is_some() => {
            return Err(OverlayError::InvalidConfig(
                "jujutsu cannot be combined with upper_dir or work_dir".into(),
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
        OverlayBackend::Jujutsu => {
            let store_path = cfg
                .jujutsu_store_path
                .as_deref()
                .map(resolve)
                .unwrap_or_else(|| storage.join(".overlay").join("jujutsu"));
            let workspace = cfg
                .jujutsu_workspace
                .clone()
                .unwrap_or_else(|| session_id.to_owned());
            let upper_dir = jujutsu_upper_dir(&store_path, &workspace)
                .map_err(|error| OverlayError::InvalidConfig(error.to_string()))?;
            OverlayUpper::Jujutsu {
                store_path,
                workspace,
                upper_dir,
            }
        }
    };
    let resolved_lowers = cfg
        .lower_dirs
        .iter()
        .map(|path| resolve(path))
        .collect::<Vec<_>>();
    let stage_is_nested = target != Path::new("/")
        && std::iter::once(&target)
            .chain(resolved_lowers.iter())
            .any(|lower| stage_dir.as_path() != lower.as_path() && stage_dir.starts_with(lower));
    let merged = cfg.merged_dir.as_deref().map(resolve).unwrap_or_else(|| {
        if stage_is_nested {
            storage
                .join(".overlay-mounts")
                .join(session_id)
                .join("merged")
        } else {
            stage_dir.join("merged")
        }
    });

    let mut backing_paths = vec![stage_dir.clone(), merged.clone()];
    match &upper {
        OverlayUpper::Directory {
            upper_dir,
            work_dir,
        } => {
            backing_paths.push(upper_dir.clone());
            backing_paths.push(work_dir.clone());
        }
        OverlayUpper::Jujutsu { store_path, .. } => backing_paths.push(store_path.clone()),
    }
    backing_paths.extend(resolved_lowers);
    let mut excluded_paths = backing_paths
        .into_iter()
        .filter_map(|path| {
            path.strip_prefix(&target)
                .ok()
                .filter(|relative| !relative.as_os_str().is_empty())
                .map(Path::to_path_buf)
        })
        .collect::<Vec<_>>();
    excluded_paths.sort_by_key(|path| path.components().count());
    let mut minimal_exclusions = Vec::<PathBuf>::new();
    for path in excluded_paths {
        if !minimal_exclusions
            .iter()
            .any(|parent| path.starts_with(parent))
        {
            minimal_exclusions.push(path);
        }
    }
    let excluded_paths = minimal_exclusions;

    Ok(Some(OverlayRecord {
        id: session_id.to_string(),
        target,
        upper,
        merged_dir: merged,
        stage_dir,
        excluded_paths,
        auto_apply: cfg.auto_apply,
        auto_discard: cfg.auto_discard,
        protect_target: cfg.protect_target,
        state: OverlayState::Active,
    }))
}

/// Build an [`OverlayHint`] from a resolved record + full lower stack.
pub fn hint_from_record(record: &OverlayRecord, lower_dirs: Vec<PathBuf>) -> OverlayHint {
    let (upper_dir, work_dir, jujutsu_store_path, jujutsu_workspace) = match &record.upper {
        OverlayUpper::Directory {
            upper_dir,
            work_dir,
        } => (Some(upper_dir.clone()), Some(work_dir.clone()), None, None),
        OverlayUpper::Jujutsu {
            store_path,
            workspace,
            ..
        } => (
            None,
            None,
            Some(store_path.clone()),
            Some(workspace.clone()),
        ),
    };
    OverlayHint {
        lower_dirs,
        stage_dir: Some(record.stage_dir.clone()),
        upper_dir,
        work_dir,
        jujutsu_store_path,
        jujutsu_workspace,
        merged_dir: Some(record.merged_dir.clone()),
        backend: match &record.upper {
            OverlayUpper::Directory { .. } => OverlayBackend::Directory,
            OverlayUpper::Jujutsu { .. } => OverlayBackend::Jujutsu,
        },
        auto_apply: record.auto_apply,
        auto_discard: record.auto_discard,
        protect_target: record.protect_target,
    }
}

/// Lower stack for mount: compose layers first (top), then the base target.
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
    lowers.push(target.to_path_buf());
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
        create_dir_all_durable(dir)
            .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
    }
    match &record.upper {
        OverlayUpper::Directory {
            upper_dir,
            work_dir,
        } => {
            create_dir_all_durable(upper_dir)
                .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
            create_dir_all_durable(work_dir)
                .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
        }
        OverlayUpper::Jujutsu { store_path, .. } => {
            create_dir_all_durable(store_path)
                .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
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
        OverlayUpper::Jujutsu {
            store_path,
            workspace,
            ..
        } => OverlayMountConfig::new_jujutsu(
            lower_dirs.to_vec(),
            store_path.clone(),
            workspace.clone(),
            record.merged_dir.clone(),
        ),
    };
    config.fsname = format!("pvisor-{}", record.id);
    config.excluded_paths = record.excluded_paths.clone();
    let session = mount_embedded_overlay(config).map_err(embedded_mount_error)?;
    wait_merged_ready(&record.merged_dir, &session)?;

    let mut record = record.clone();
    record.state = OverlayState::Active;
    write_overlay_record(&record)?;

    Ok(OverlayMount {
        record,
        session: Some(session),
    })
}

/// Prepare durable overlay backing directories for a consumer that serves the
/// union itself (currently libkrun virtio-fs), without creating a host mount.
pub(crate) fn prepare_overlay_record_mountless(
    record: &OverlayRecord,
    lower_dirs: &[PathBuf],
) -> Result<OverlayRecord, OverlayError> {
    if lower_dirs.is_empty() {
        return Err(OverlayError::MissingTarget);
    }
    for dir in lower_dirs.iter().chain([&record.stage_dir]) {
        create_dir_all_durable(dir)
            .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
    }
    match &record.upper {
        OverlayUpper::Directory {
            upper_dir,
            work_dir,
        } => {
            create_dir_all_durable(upper_dir)
                .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
            create_dir_all_durable(work_dir)
                .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
        }
        OverlayUpper::Jujutsu {
            store_path,
            workspace,
            upper_dir,
        } => {
            create_dir_all_durable(store_path)
                .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
            persisting_overlayfs::prepare_jujutsu_upper(store_path, workspace)
                .map_err(OverlayError::Prepare)?;
            create_dir_all_durable(upper_dir)
                .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
        }
    }
    let mut record = record.clone();
    record.state = OverlayState::Active;
    write_overlay_record(&record)?;
    Ok(record)
}

pub(crate) fn stage_overlay_record(record: &mut OverlayRecord) -> anyhow::Result<()> {
    if record.state == OverlayState::Active {
        record.state = OverlayState::Staged;
        if let OverlayUpper::Jujutsu {
            store_path,
            workspace,
            ..
        } = &record.upper
        {
            snapshot_jujutsu_upper(store_path, workspace)?;
        }
        write_overlay_record(record)?;
    }
    Ok(())
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
        OverlayUpper::Jujutsu {
            store_path,
            workspace,
            ..
        } => OverlayMountConfig::new_jujutsu(
            lower_dirs.to_vec(),
            store_path.clone(),
            workspace.clone(),
            mountpoint.to_path_buf(),
        ),
    };
    config.fsname = format!("pvisor-inspect-{}", record.id);
    config.excluded_paths = record.excluded_paths.clone();
    config.read_only = true;
    let session = mount_embedded_overlay(config).map_err(embedded_mount_error)?;
    wait_merged_ready(mountpoint, &session)?;
    Ok(ReadOnlyOverlayMount {
        session: Some(session),
        mountpoint: mountpoint.to_path_buf(),
    })
}

fn embedded_mount_error(error: anyhow::Error) -> OverlayError {
    #[cfg(target_os = "macos")]
    {
        OverlayError::Mount(format!(
            "{error}; macOS staged workspaces require macFUSE 5 to be installed and enabled (brew install --cask macfuse)"
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        OverlayError::Mount(error.to_string())
    }
}

pub fn overlay_meta_path(stage_dir: &Path) -> PathBuf {
    stage_dir.join(META_FILENAME)
}

pub fn write_overlay_record(record: &OverlayRecord) -> Result<(), OverlayError> {
    let path = overlay_meta_path(&record.stage_dir);
    let body = serde_json::to_string_pretty(record)
        .map_err(|e| OverlayError::Persist(format!("serialize meta: {e}")))?;
    atomic_write(&path, body.as_bytes(), 0o600)
        .map_err(|error| OverlayError::Persist(format!("{}: {error:#}", path.display())))?;
    Ok(())
}

pub fn load_overlay_record(stage_dir: &Path) -> Result<OverlayRecord, OverlayError> {
    let path = overlay_meta_path(stage_dir);
    let raw =
        fs::read_to_string(&path).map_err(|_| OverlayError::Meta(path.display().to_string()))?;
    serde_json::from_str(&raw).map_err(|e| OverlayError::Meta(format!("{}: {e}", path.display())))
}

pub fn overlay_status(record: &OverlayRecord) -> Result<OverlayStatus, OverlayError> {
    let upper_dir = match &record.upper {
        OverlayUpper::Directory { upper_dir, .. } | OverlayUpper::Jujutsu { upper_dir, .. } => {
            upper_dir
        }
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

/// Build the complete classified upper-layer changeset without reading file
/// contents. `lower_dirs` use overlay priority order (highest first).
pub fn overlay_changes(
    record: &OverlayRecord,
    lower_dirs: &[PathBuf],
) -> Result<Vec<ChangeEntry>, OverlayError> {
    let upper_dir = record.upper.path();
    let mut changes = Vec::new();
    if !upper_dir.is_dir() {
        return Ok(changes);
    }
    walk_upper(upper_dir, upper_dir, &mut |rel, is_whiteout| {
        let upper_path = upper_dir.join(&rel);
        if is_whiteout {
            let name = rel.file_name().unwrap_or_default();
            let parent = rel.parent().unwrap_or_else(|| Path::new(""));
            if name == OPAQUE_WHITEOUT {
                changes.push(ChangeEntry {
                    path: parent.display().to_string(),
                    kind: ChangeKind::Opaque,
                    old_type: Some(ChangeEntryType::Directory),
                    new_type: Some(ChangeEntryType::Directory),
                    size_bytes: None,
                    mode: None,
                });
            } else if let Some(victim) = whiteout_target(name) {
                let path = parent.join(victim);
                let old = lower_metadata(lower_dirs, &path);
                changes.push(ChangeEntry {
                    path: path.display().to_string(),
                    kind: ChangeKind::Deleted,
                    old_type: old.as_ref().map(metadata_type),
                    new_type: None,
                    size_bytes: None,
                    mode: None,
                });
            }
            return Ok(());
        }

        let new = fs::symlink_metadata(&upper_path)?;
        let old = lower_metadata(lower_dirs, &rel);
        let old_type = old.as_ref().map(metadata_type);
        let new_type = metadata_type(&new);
        let kind = match old_type {
            None => ChangeKind::Added,
            Some(old_type) if old_type != new_type => ChangeKind::TypeChanged,
            Some(_) => ChangeKind::Modified,
        };
        changes.push(ChangeEntry {
            path: rel.display().to_string(),
            kind,
            old_type,
            new_type: Some(new_type),
            size_bytes: new.is_file().then_some(new.len()),
            mode: Some(new.permissions().mode() & 0o7777),
        });
        Ok(())
    })?;
    changes.sort_by(|left, right| left.path.cmp(&right.path).then(left.kind.cmp(&right.kind)));
    Ok(changes)
}

fn lower_metadata(lower_dirs: &[PathBuf], relative: &Path) -> Option<fs::Metadata> {
    lower_dirs
        .iter()
        .find_map(|lower| fs::symlink_metadata(lower.join(relative)).ok())
}

fn metadata_type(metadata: &fs::Metadata) -> ChangeEntryType {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        ChangeEntryType::File
    } else if file_type.is_dir() {
        ChangeEntryType::Directory
    } else if file_type.is_symlink() {
        ChangeEntryType::Symlink
    } else {
        ChangeEntryType::Other
    }
}

/// Copy the raw upper tree without interpreting whiteouts or opaque markers.
/// The destination is replaced and can later seed another directory upper.
pub fn snapshot_overlay_upper(
    record: &OverlayRecord,
    destination: &Path,
) -> Result<(), OverlayError> {
    restore_overlay_upper(record.upper.path(), destination)
}

/// Restore a raw upper snapshot into a directory upper.
pub fn restore_overlay_upper(source: &Path, destination: &Path) -> Result<(), OverlayError> {
    if path_exists(destination) {
        remove_path(destination)?;
    }
    fs::create_dir_all(destination)?;
    if !source.is_dir() {
        return Ok(());
    }
    let mut hard_links = HashMap::new();
    snapshot_directory_raw(source, destination, &mut hard_links, &|| false)?;
    Ok(())
}

struct CompiledApplySelection {
    paths: Vec<PathBuf>,
    includes: GlobSet,
    excludes: GlobSet,
    has_positive: bool,
}

impl CompiledApplySelection {
    fn compile(selection: &ApplySelection) -> Result<Self, OverlayError> {
        let paths = selection
            .paths
            .iter()
            .map(|path| normalize_selection_path(path))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            has_positive: !paths.is_empty() || !selection.includes.is_empty(),
            paths,
            includes: compile_globs(&selection.includes, "include")?,
            excludes: compile_globs(&selection.excludes, "exclude")?,
        })
    }

    fn requested(&self, path: &Path) -> bool {
        !self.has_positive
            || self
                .paths
                .iter()
                .any(|selected| path == selected || path.starts_with(selected))
            || self.includes.is_match(path)
    }

    fn excluded(&self, path: &Path) -> bool {
        let mut current = Some(path);
        while let Some(candidate) = current {
            if self.excludes.is_match(candidate) {
                return true;
            }
            current = candidate.parent();
        }
        false
    }
}

fn normalize_selection_path(path: &Path) -> Result<PathBuf, OverlayError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(OverlayError::InvalidConfig(format!(
                    "apply path must be relative and cannot contain `..`: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(normalized)
}

fn compile_globs(patterns: &[String], label: &str) -> Result<GlobSet, OverlayError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|error| {
                OverlayError::InvalidConfig(format!(
                    "invalid apply {label} glob `{pattern}`: {error}"
                ))
            })?;
        builder.add(glob);
    }
    builder.build().map_err(|error| {
        OverlayError::InvalidConfig(format!("compile apply {label} globs: {error}"))
    })
}

fn at_or_below(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

/// Resolve a filtered selection into a dependency-closed apply plan.
///
/// Directory ancestors and hard-link siblings are included automatically.
/// Opaque directories remain atomic: callers must select the opaque directory,
/// rather than only a child whose application would implicitly delete siblings.
pub fn plan_overlay_apply(
    record: &OverlayRecord,
    lower_dirs: &[PathBuf],
    selection: &ApplySelection,
) -> Result<ApplyPlan, OverlayError> {
    let changes = overlay_changes(record, lower_dirs)?;
    let compiled = CompiledApplySelection::compile(selection)?;
    let mut selected_paths = changes
        .iter()
        .map(|change| PathBuf::from(&change.path))
        .filter(|path| compiled.requested(path) && !compiled.excluded(path))
        .collect::<BTreeSet<_>>();

    if !selection.is_all() && selected_paths.is_empty() {
        return Err(OverlayError::Apply(
            "no staged changes matched the apply selection".into(),
        ));
    }

    let opaque_dirs = changes
        .iter()
        .filter(|change| change.kind == ChangeKind::Opaque)
        .map(|change| PathBuf::from(&change.path))
        .collect::<Vec<_>>();
    let hard_link_groups = upper_hard_link_groups(record.upper.path())?;

    loop {
        let before = selected_paths.len();

        for opaque in &opaque_dirs {
            let has_selected_child = selected_paths
                .iter()
                .any(|path| path != opaque && at_or_below(path, opaque));
            if has_selected_child && !selected_paths.contains(opaque) {
                return Err(OverlayError::Apply(format!(
                    "{} is inside opaque directory {}; select the directory as one atomic unit",
                    selected_paths
                        .iter()
                        .find(|path| *path != opaque && at_or_below(path, opaque))
                        .map_or_else(|| "selected path".into(), |path| path.display().to_string()),
                    if opaque.as_os_str().is_empty() {
                        ".".into()
                    } else {
                        opaque.display().to_string()
                    }
                )));
            }
            if selected_paths.contains(opaque) {
                for change in &changes {
                    let path = PathBuf::from(&change.path);
                    if at_or_below(&path, opaque) {
                        if compiled.excluded(&path) {
                            return Err(OverlayError::Apply(format!(
                                "cannot exclude {} from atomic opaque directory {}",
                                path.display(),
                                opaque.display()
                            )));
                        }
                        selected_paths.insert(path);
                    }
                }
            }
        }

        for group in &hard_link_groups {
            if group.iter().any(|path| selected_paths.contains(path)) {
                for path in group {
                    if compiled.excluded(path) {
                        return Err(OverlayError::Apply(format!(
                            "cannot exclude hard-link sibling {} from the selected apply batch",
                            path.display()
                        )));
                    }
                    selected_paths.insert(path.clone());
                }
            }
        }

        let selected_snapshot = selected_paths.iter().cloned().collect::<Vec<_>>();
        for path in selected_snapshot {
            let mut parent = path.parent();
            while let Some(ancestor) = parent {
                if changes.iter().any(|change| {
                    Path::new(&change.path) == ancestor
                        && change.new_type == Some(ChangeEntryType::Directory)
                }) {
                    if compiled.excluded(ancestor) {
                        return Err(OverlayError::Apply(format!(
                            "cannot exclude ancestor {} required by selected path {}",
                            ancestor.display(),
                            path.display()
                        )));
                    }
                    selected_paths.insert(ancestor.to_path_buf());
                }
                parent = ancestor.parent();
            }
        }

        if selected_paths.len() == before {
            break;
        }
    }

    let selected = changes
        .iter()
        .filter(|change| selected_paths.contains(Path::new(&change.path)))
        .cloned()
        .collect::<Vec<_>>();
    Ok(ApplyPlan {
        selected,
        selected_paths,
    })
}

fn upper_hard_link_groups(upper: &Path) -> Result<Vec<Vec<PathBuf>>, OverlayError> {
    let mut groups = HashMap::<(u64, u64), Vec<PathBuf>>::new();
    if !upper.is_dir() {
        return Ok(Vec::new());
    }
    walk_upper(upper, upper, &mut |rel, is_whiteout| {
        if !is_whiteout {
            let metadata = fs::symlink_metadata(upper.join(&rel))?;
            if metadata.is_file() && metadata.nlink() > 1 {
                groups
                    .entry((metadata.dev(), metadata.ino()))
                    .or_default()
                    .push(rel);
            }
        }
        Ok(())
    })?;
    Ok(groups
        .into_values()
        .filter(|paths| paths.len() > 1)
        .collect())
}

/// Apply one dependency-closed subset and retain all unselected changes for a
/// later apply or drop decision.
pub fn apply_overlay_selected(
    record: &mut OverlayRecord,
    lower_dirs: &[PathBuf],
    selection: &ApplySelection,
) -> Result<ApplyOutcome, OverlayError> {
    match record.state {
        OverlayState::Applied if selection.is_all() => {
            return Ok(ApplyOutcome {
                apply_id: String::new(),
                applied: Vec::new(),
                remaining: Vec::new(),
            });
        }
        OverlayState::Applied => {
            return Err(OverlayError::InvalidState(format!(
                "overlay {} was already fully applied",
                record.id
            )));
        }
        OverlayState::Discarded => {
            return Err(OverlayError::InvalidState(format!(
                "overlay {} was already dropped; apply cannot recover discarded changes",
                record.id
            )));
        }
        OverlayState::Active | OverlayState::Staged => {}
    }
    if record.protect_target {
        return Err(OverlayError::Apply(format!(
            "target is an immutable image rootfs: {}",
            record.target.display()
        )));
    }

    let plan = plan_overlay_apply(record, lower_dirs, selection)?;
    let upper_dir = record.upper.path().to_path_buf();
    if upper_dir.is_dir() && !plan.selected.is_empty() {
        apply_selected_upper(&upper_dir, &record.target, &plan.selected_paths)?;
        prune_selected_upper(&upper_dir, &plan.selected_paths)?;
    }

    if let OverlayUpper::Jujutsu {
        store_path,
        workspace,
        ..
    } = &record.upper
    {
        snapshot_jujutsu_upper(store_path, workspace)
            .map_err(|error| OverlayError::Finalize(error.to_string()))?;
    }

    let remaining = overlay_changes(record, lower_dirs)?;
    if remaining.is_empty() {
        cleanup_terminal_overlay_data(record)?;
        record.state = OverlayState::Applied;
    } else {
        record.state = OverlayState::Staged;
    }
    write_overlay_record(record)?;

    let apply_id = uuid::Uuid::new_v4().to_string();
    append_apply_record(
        record,
        ApplyRecord {
            schema_version: APPLY_LEDGER_SCHEMA_VERSION,
            apply_id: apply_id.clone(),
            created_at_unix_ms: crate::util::unix_now_ms(),
            overlay_id: record.id.clone(),
            target: record.target.clone(),
            selection: selection.clone(),
            changes: plan.selected.clone(),
            remaining_changes: remaining.len(),
        },
    )?;
    Ok(ApplyOutcome {
        apply_id,
        applied: plan.selected,
        remaining,
    })
}

/// Merge the complete staging upper onto `target`.
pub fn apply_overlay(record: &mut OverlayRecord) -> Result<(), OverlayError> {
    let lower_dirs = vec![record.target.clone()];
    apply_overlay_selected(record, &lower_dirs, &ApplySelection::default()).map(|_| ())
}

/// Drop staging upper (and optionally the whole stage dir contents except meta).
pub fn discard_overlay(record: &mut OverlayRecord) -> Result<(), OverlayError> {
    match record.state {
        OverlayState::Discarded => return Ok(()),
        OverlayState::Applied => {
            return Err(OverlayError::InvalidState(format!(
                "overlay {} was already applied; drop cannot undo applied changes",
                record.id
            )));
        }
        OverlayState::Active | OverlayState::Staged => {}
    }
    match &record.upper {
        OverlayUpper::Directory { .. } => {}
        OverlayUpper::Jujutsu {
            store_path,
            workspace,
            upper_dir,
        } => {
            clear_path(upper_dir)?;
            snapshot_jujutsu_upper(store_path, workspace)
                .map_err(|error| OverlayError::Finalize(error.to_string()))?;
            clear_path(upper_dir)?;
        }
    }
    cleanup_terminal_overlay_data(record)?;
    record.state = OverlayState::Discarded;
    write_overlay_record(record)?;
    Ok(())
}

fn cleanup_terminal_overlay_data(record: &OverlayRecord) -> Result<(), OverlayError> {
    match &record.upper {
        OverlayUpper::Directory {
            upper_dir,
            work_dir,
        } => {
            clear_path(upper_dir)?;
            clear_path(work_dir)?;
        }
        OverlayUpper::Jujutsu { upper_dir, .. } => clear_path(upper_dir)?,
    }

    // Never recursively remove a mountpoint: after a clean teardown this is
    // either absent or an empty placeholder. A non-empty directory is retained
    // for inspection instead of risking traversal into a stale mount.
    if record.merged_dir.starts_with(&record.stage_dir) {
        match fs::remove_dir(&record.merged_dir) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn clear_path(path: &Path) -> Result<(), OverlayError> {
    if path_exists(path) {
        remove_path(path)?;
    }
    Ok(())
}

#[cfg(test)]
fn apply_upper_onto_target(upper: &Path, target: &Path) -> Result<(), OverlayError> {
    ensure_directory(target)?;
    let mut hard_links = HashMap::new();
    apply_directory(upper, target, &mut hard_links, false)
}

fn apply_selected_upper(
    upper: &Path,
    target: &Path,
    selected: &BTreeSet<PathBuf>,
) -> Result<(), OverlayError> {
    ensure_directory(target)?;
    let mut hard_links = HashMap::new();
    apply_selected_directory(upper, target, Path::new(""), selected, &mut hard_links)
}

fn apply_selected_directory(
    source: &Path,
    destination: &Path,
    relative: &Path,
    selected: &BTreeSet<PathBuf>,
    hard_links: &mut HashMap<(u64, u64), PathBuf>,
) -> Result<(), OverlayError> {
    ensure_directory(destination)?;
    let opaque = path_exists(&source.join(OPAQUE_WHITEOUT)) || has_opaque_xattr(source);
    if opaque && selected.contains(relative) {
        for entry in fs::read_dir(destination)? {
            remove_path(&entry?.path())?;
        }
    }

    let entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    for entry in &entries {
        let name = entry.file_name();
        if name == OPAQUE_WHITEOUT {
            continue;
        }
        if let Some(victim) = whiteout_target(&name) {
            let logical = relative.join(victim);
            if selected.contains(&logical) {
                remove_path(&destination.join(victim))?;
            }
        }
    }

    for entry in entries {
        let name = entry.file_name();
        if name.as_bytes().starts_with(WHITEOUT_PREFIX.as_bytes()) {
            continue;
        }
        let logical = relative.join(&name);
        let source_path = entry.path();
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.is_dir() {
            let has_selected_descendant = selected
                .iter()
                .any(|path| path != &logical && path.starts_with(&logical));
            if selected.contains(&logical) || has_selected_descendant {
                let target_path = destination.join(&name);
                ensure_directory(&target_path)?;
                apply_selected_directory(
                    &source_path,
                    &target_path,
                    &logical,
                    selected,
                    hard_links,
                )?;
                if selected.contains(&logical) {
                    copy_host_metadata(&source_path, &target_path)?;
                }
            }
        } else if selected.contains(&logical) {
            copy_upper_entry(&source_path, &destination.join(name), hard_links)?;
        }
    }
    Ok(())
}

fn prune_selected_upper(upper: &Path, selected: &BTreeSet<PathBuf>) -> Result<(), OverlayError> {
    prune_selected_directory(upper, Path::new(""), selected)
}

fn prune_selected_directory(
    directory: &Path,
    relative: &Path,
    selected: &BTreeSet<PathBuf>,
) -> Result<(), OverlayError> {
    if selected.contains(relative) {
        clear_opaque_xattrs(directory)?;
    }
    let entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    for entry in entries {
        let name = entry.file_name();
        if name == OPAQUE_WHITEOUT {
            if selected.contains(relative) {
                remove_path(&entry.path())?;
            }
            continue;
        }
        if let Some(victim) = whiteout_target(&name) {
            if selected.contains(&relative.join(victim)) {
                remove_path(&entry.path())?;
            }
            continue;
        }

        let path = entry.path();
        let logical = relative.join(&name);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            let has_selected_descendant = selected.iter().any(|selected_path| {
                selected_path != &logical && selected_path.starts_with(&logical)
            });
            if selected.contains(&logical) || has_selected_descendant {
                prune_selected_directory(&path, &logical, selected)?;
            }
            if selected.contains(&logical) && fs::read_dir(&path)?.next().is_none() {
                fs::remove_dir(&path)?;
            }
        } else if selected.contains(&logical) {
            remove_path(&path)?;
        }
    }
    Ok(())
}

fn apply_ledger_path(stage_dir: &Path) -> PathBuf {
    stage_dir.join(APPLY_LEDGER_FILENAME)
}

pub fn load_apply_records(stage_dir: &Path) -> Result<Vec<ApplyRecord>, OverlayError> {
    let path = apply_ledger_path(stage_dir);
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let ledger = serde_json::from_slice::<ApplyLedger>(&raw)
        .map_err(|error| OverlayError::Meta(format!("{}: {error}", path.display())))?;
    if ledger.schema_version != APPLY_LEDGER_SCHEMA_VERSION {
        return Err(OverlayError::Meta(format!(
            "{}: unsupported apply ledger schema {}",
            path.display(),
            ledger.schema_version
        )));
    }
    Ok(ledger.records)
}

fn append_apply_record(
    overlay: &OverlayRecord,
    apply_record: ApplyRecord,
) -> Result<(), OverlayError> {
    let mut ledger = ApplyLedger {
        schema_version: APPLY_LEDGER_SCHEMA_VERSION,
        records: load_apply_records(&overlay.stage_dir)?,
    };
    ledger.records.push(apply_record);
    let path = apply_ledger_path(&overlay.stage_dir);
    let body = serde_json::to_vec_pretty(&ledger)
        .map_err(|error| OverlayError::Persist(format!("serialize apply ledger: {error}")))?;
    atomic_write(&path, &body, 0o600)
        .map_err(|error| OverlayError::Persist(format!("{}: {error:#}", path.display())))
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

fn snapshot_directory_raw(
    source: &Path,
    destination: &Path,
    hard_links: &mut HashMap<(u64, u64), PathBuf>,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), OverlayError> {
    if cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "materialization cancelled").into());
    }
    ensure_directory(destination)?;
    for entry in fs::read_dir(source)? {
        if cancelled() {
            return Err(
                io::Error::new(io::ErrorKind::Interrupted, "materialization cancelled").into(),
            );
        }
        let entry = entry?;
        snapshot_entry_raw(
            &entry.path(),
            &destination.join(entry.file_name()),
            hard_links,
            cancelled,
        )?;
    }
    copy_snapshot_metadata(source, destination)?;
    Ok(())
}

fn snapshot_entry_raw(
    source: &Path,
    destination: &Path,
    hard_links: &mut HashMap<(u64, u64), PathBuf>,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), OverlayError> {
    if cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "materialization cancelled").into());
    }
    let metadata = fs::symlink_metadata(source)?;
    let kind = metadata.file_type();
    if kind.is_dir() {
        return snapshot_directory_raw(source, destination, hard_links, cancelled);
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
        // SAFETY: the C path and metadata remain valid for this call.
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
    copy_snapshot_metadata(source, destination)?;
    Ok(())
}

fn copy_snapshot_metadata(source: &Path, destination: &Path) -> io::Result<()> {
    copy_host_metadata(source, destination)?;
    let source_c = c_path(source)?;
    let destination_c = c_path(destination)?;
    for name in OPAQUE_XATTRS {
        let name = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        if let Ok(value) = get_host_xattr(&source_c, &name) {
            if let Err(error) = set_host_xattr(&destination_c, &name, &value) {
                if !matches!(
                    error.raw_os_error(),
                    Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::ENOTSUP)
                ) {
                    return Err(error);
                }
            }
        }
    }
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

fn clear_opaque_xattrs(path: &Path) -> Result<(), OverlayError> {
    let path = c_path(path)?;
    for name in OPAQUE_XATTRS {
        let name = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        if get_host_xattr(&path, &name).is_ok_and(|value| value == b"y") {
            remove_host_xattr(&path, &name)?;
        }
    }
    Ok(())
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
            OverlayUpper::Directory {
                upper_dir: PathBuf::from("/tmp/store/.overlay/run-1/upper"),
                work_dir: PathBuf::from("/tmp/store/.overlay/run-1/work")
            }
        );
    }

    #[test]
    fn resolve_rejects_parallel_upper_backends() {
        let cfg = OverlayConfig {
            enabled: true,
            target: Some("/proj".into()),
            jujutsu_store_path: Some("/shared/jj".into()),
            ..OverlayConfig::default()
        };
        assert!(matches!(
            resolve_overlay_workspace(&cfg, Path::new("/tmp/store"), "run-1"),
            Err(OverlayError::InvalidConfig(_))
        ));
    }

    #[test]
    fn root_overlay_hides_its_stage_and_compose_backing_paths() {
        let cfg = OverlayConfig {
            enabled: true,
            target: Some("/".into()),
            stage_dir: Some("/tmp/persisting-runs/run-one".into()),
            lower_dirs: vec!["/var/lib/persisting/layers/base".into()],
            ..OverlayConfig::default()
        };
        let record = resolve_overlay_workspace(&cfg, Path::new("/unused"), "run-one")
            .unwrap()
            .unwrap();
        assert_eq!(record.target, Path::new("/"));
        assert_eq!(
            record.excluded_paths,
            [
                PathBuf::from("tmp/persisting-runs/run-one"),
                PathBuf::from("var/lib/persisting/layers/base"),
            ]
        );
    }

    #[test]
    fn nested_stage_is_hidden_from_a_non_root_overlay() {
        let cfg = OverlayConfig {
            enabled: true,
            target: Some("/Users/example/workspace".into()),
            stage_dir: Some("/Users/example/workspace/project/tmp".into()),
            ..OverlayConfig::default()
        };
        let record = resolve_overlay_workspace(&cfg, Path::new("/unused"), "run-one")
            .unwrap()
            .unwrap();
        assert_eq!(record.excluded_paths, [PathBuf::from("project/tmp")]);
        assert_eq!(
            record.merged_dir,
            PathBuf::from("/unused/.overlay-mounts/run-one/merged")
        );
        assert!(!record.merged_dir.starts_with(&record.target));
    }

    #[test]
    fn mountless_preparation_creates_backing_state_without_a_merged_mount() {
        let tmp = tempdir().unwrap();
        let lower = tmp.path().join("lower");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&lower).unwrap();
        let record = OverlayRecord {
            id: "mountless".into(),
            target: lower.clone(),
            upper: OverlayUpper::Directory {
                upper_dir: stage.join("upper"),
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage.clone(),
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Active,
        };
        let prepared = prepare_overlay_record_mountless(&record, &[lower]).unwrap();
        assert!(prepared.upper.path().is_dir());
        assert!(stage.join("work").is_dir());
        assert!(!prepared.merged_dir.exists());
        assert_eq!(
            load_overlay_record(&stage).unwrap().state,
            OverlayState::Active
        );
    }

    #[test]
    fn jujutsu_sessions_share_store_but_get_distinct_workspaces() {
        let storage = Path::new("/tmp/store");
        let cfg = OverlayConfig {
            enabled: true,
            target: Some("/proj".into()),
            backend: OverlayBackend::Jujutsu,
            ..OverlayConfig::default()
        };
        let first = resolve_overlay_workspace(&cfg, storage, "fork-a")
            .unwrap()
            .unwrap();
        let second = resolve_overlay_workspace(&cfg, storage, "fork-b")
            .unwrap()
            .unwrap();
        let OverlayUpper::Jujutsu {
            store_path: first_store,
            workspace: first_workspace,
            upper_dir: first_upper,
        } = first.upper
        else {
            panic!("expected Jujutsu upper")
        };
        let OverlayUpper::Jujutsu {
            store_path: second_store,
            workspace: second_workspace,
            upper_dir: second_upper,
        } = second.upper
        else {
            panic!("expected Jujutsu upper")
        };
        assert_eq!(first_store, second_store);
        assert_eq!(first_store, PathBuf::from("/tmp/store/.overlay/jujutsu"));
        assert_eq!(first_workspace, "fork-a");
        assert_eq!(second_workspace, "fork-b");
        assert_ne!(first_upper, second_upper);
    }

    #[test]
    fn lower_stack_keeps_target_as_bottom_base_layer() {
        let cfg = OverlayConfig {
            lower_dirs: vec!["extra-a".into(), "extra-b".into()],
            ..OverlayConfig::default()
        };
        assert_eq!(
            lower_stack_from_config(&cfg, Path::new("/store"), Path::new("/target")),
            vec![
                PathBuf::from("/store/extra-a"),
                PathBuf::from("/store/extra-b"),
                PathBuf::from("/target"),
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
        let upper = stage.join("upper");
        let work = stage.join("work");
        let merged = stage.join("merged");
        fs::create_dir_all(&lower).unwrap();
        fs::write(lower.join("lower-file"), b"lower").unwrap();
        fs::write(lower.join("deleted-file"), b"delete me").unwrap();
        let record = OverlayRecord {
            id: "embedded-e2e".into(),
            target: lower.clone(),
            upper: OverlayUpper::Directory {
                upper_dir: upper.clone(),
                work_dir: work,
            },
            merged_dir: merged.clone(),
            stage_dir: stage.clone(),
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
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
        assert!(upper.is_dir());
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
        assert!(!upper.exists());
        assert!(!stage.join("work").exists());
        assert!(stage.join(META_FILENAME).is_file());
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

        let work = tmp.path().join("work");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("scratch"), b"temporary").unwrap();
        let mut rec = OverlayRecord {
            id: "t".into(),
            target: target.clone(),
            upper: OverlayUpper::Directory {
                upper_dir: upper.clone(),
                work_dir: work.clone(),
            },
            merged_dir: tmp.path().join("merged"),
            stage_dir: tmp.path().to_path_buf(),
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
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
        assert!(!upper.exists());
        assert!(!work.exists());
        assert!(tmp.path().join(META_FILENAME).is_file());
    }

    #[test]
    fn selective_apply_retains_pending_changes_and_can_repeat() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let stage = tmp.path().join("stage");
        let upper = stage.join("upper");
        fs::create_dir_all(target.join("src")).unwrap();
        fs::create_dir_all(upper.join("src")).unwrap();
        fs::write(target.join("src/a.txt"), b"old-a").unwrap();
        fs::write(target.join("src/b.txt"), b"old-b").unwrap();
        fs::write(target.join("gone.txt"), b"old-gone").unwrap();
        fs::write(upper.join("src/a.txt"), b"new-a").unwrap();
        fs::write(upper.join("src/b.txt"), b"new-b").unwrap();
        fs::write(upper.join(".wh.gone.txt"), b"").unwrap();
        let mut record = OverlayRecord {
            id: "selective".into(),
            target: target.clone(),
            upper: OverlayUpper::Directory {
                upper_dir: upper.clone(),
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage.clone(),
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };

        let first = apply_overlay_selected(
            &mut record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["src/a.txt".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap();
        assert_eq!(fs::read(target.join("src/a.txt")).unwrap(), b"new-a");
        assert_eq!(fs::read(target.join("src/b.txt")).unwrap(), b"old-b");
        assert!(target.join("gone.txt").exists());
        assert_eq!(record.state, OverlayState::Staged);
        assert!(first
            .remaining
            .iter()
            .any(|change| change.path == "src/b.txt"));
        assert!(upper.join("src/b.txt").is_file());
        assert!(upper.join(".wh.gone.txt").is_file());

        apply_overlay_selected(
            &mut record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["gone.txt".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap();
        assert!(!target.join("gone.txt").exists());
        assert_eq!(record.state, OverlayState::Staged);

        let final_apply = apply_overlay_selected(
            &mut record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["src".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap();
        assert_eq!(fs::read(target.join("src/b.txt")).unwrap(), b"new-b");
        assert!(final_apply.remaining.is_empty());
        assert_eq!(record.state, OverlayState::Applied);
        assert!(!upper.exists());

        let ledger = load_apply_records(&stage).unwrap();
        assert_eq!(ledger.len(), 3);
        assert_eq!(ledger[0].remaining_changes, first.remaining.len());
        assert_eq!(ledger[2].remaining_changes, 0);
    }

    #[test]
    fn glob_excludes_leave_matching_subtrees_staged() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let stage = tmp.path().join("stage");
        let upper = stage.join("upper");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(upper.join("src/generated")).unwrap();
        fs::write(upper.join("src/lib.rs"), b"accepted").unwrap();
        fs::write(upper.join("src/generated/code.rs"), b"pending").unwrap();
        let mut record = OverlayRecord {
            id: "glob".into(),
            target: target.clone(),
            upper: OverlayUpper::Directory {
                upper_dir: upper.clone(),
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage,
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };

        let outcome = apply_overlay_selected(
            &mut record,
            std::slice::from_ref(&target),
            &ApplySelection {
                includes: vec!["src/**".into()],
                excludes: vec!["src/generated/**".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap();
        assert_eq!(fs::read(target.join("src/lib.rs")).unwrap(), b"accepted");
        assert!(!target.join("src/generated/code.rs").exists());
        assert!(upper.join("src/generated/code.rs").is_file());
        assert!(outcome
            .remaining
            .iter()
            .any(|change| change.path == "src/generated/code.rs"));
        assert_eq!(record.state, OverlayState::Staged);
    }

    #[test]
    fn opaque_directory_requires_atomic_selection() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let stage = tmp.path().join("stage");
        let upper = stage.join("upper");
        fs::create_dir_all(target.join("replaced")).unwrap();
        fs::create_dir_all(upper.join("replaced")).unwrap();
        fs::write(target.join("replaced/old"), b"old").unwrap();
        fs::write(upper.join("replaced").join(OPAQUE_WHITEOUT), b"").unwrap();
        fs::write(upper.join("replaced/new"), b"new").unwrap();
        let mut record = OverlayRecord {
            id: "opaque-select".into(),
            target: target.clone(),
            upper: OverlayUpper::Directory {
                upper_dir: upper,
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage,
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };

        let error = plan_overlay_apply(
            &record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["replaced/new".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("opaque directory replaced"));

        apply_overlay_selected(
            &mut record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["replaced".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap();
        assert!(!target.join("replaced/old").exists());
        assert_eq!(fs::read(target.join("replaced/new")).unwrap(), b"new");
    }

    #[test]
    fn selective_apply_expands_hard_link_groups() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let stage = tmp.path().join("stage");
        let upper = stage.join("upper");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&upper).unwrap();
        fs::write(upper.join("first"), b"linked").unwrap();
        fs::hard_link(upper.join("first"), upper.join("second")).unwrap();
        let mut record = OverlayRecord {
            id: "hard-links".into(),
            target: target.clone(),
            upper: OverlayUpper::Directory {
                upper_dir: upper,
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage,
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };

        let outcome = apply_overlay_selected(
            &mut record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["first".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap();
        assert!(outcome.applied.iter().any(|change| change.path == "first"));
        assert!(outcome.applied.iter().any(|change| change.path == "second"));
        assert_eq!(
            fs::metadata(target.join("first")).unwrap().ino(),
            fs::metadata(target.join("second")).unwrap().ino()
        );
        assert_eq!(record.state, OverlayState::Applied);
    }

    #[test]
    fn apply_selection_rejects_parent_traversal() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let upper = tmp.path().join("upper");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&upper).unwrap();
        fs::write(upper.join("value"), b"value").unwrap();
        let record = OverlayRecord {
            id: "invalid-selection".into(),
            target: target.clone(),
            upper: OverlayUpper::Directory {
                upper_dir: upper,
                work_dir: tmp.path().join("work"),
            },
            merged_dir: tmp.path().join("merged"),
            stage_dir: tmp.path().to_path_buf(),
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        let error = plan_overlay_apply(
            &record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["../outside".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("cannot contain `..`"));
    }

    #[test]
    fn changeset_classifies_added_modified_deleted_and_opaque_entries() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let upper = tmp.path().join("upper");
        fs::create_dir_all(target.join("dir")).unwrap();
        fs::write(target.join("modified.txt"), b"old").unwrap();
        fs::write(target.join("deleted.txt"), b"gone").unwrap();
        fs::write(target.join("dir/lower.txt"), b"lower").unwrap();
        fs::create_dir_all(upper.join("dir")).unwrap();
        fs::write(upper.join("modified.txt"), b"new").unwrap();
        fs::write(upper.join("added.txt"), b"added").unwrap();
        fs::write(upper.join(".wh.deleted.txt"), b"").unwrap();
        fs::write(upper.join("dir/.wh..wh..opq"), b"").unwrap();
        let record = OverlayRecord {
            id: "changes".into(),
            target: target.clone(),
            upper: OverlayUpper::Directory {
                upper_dir: upper,
                work_dir: tmp.path().join("work"),
            },
            merged_dir: tmp.path().join("merged"),
            stage_dir: tmp.path().to_path_buf(),
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };

        let changes = overlay_changes(&record, std::slice::from_ref(&target)).unwrap();
        assert!(changes
            .iter()
            .any(|change| change.path == "added.txt" && change.kind == ChangeKind::Added));
        assert!(changes.iter().any(|change| {
            change.path == "modified.txt" && change.kind == ChangeKind::Modified
        }));
        assert!(changes
            .iter()
            .any(|change| { change.path == "deleted.txt" && change.kind == ChangeKind::Deleted }));
        assert!(changes
            .iter()
            .any(|change| change.path == "dir" && change.kind == ChangeKind::Opaque));
    }

    #[test]
    fn apply_rejects_an_immutable_image_target() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let upper = tmp.path().join("upper");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&upper).unwrap();
        fs::write(target.join("system"), b"cached").unwrap();
        fs::write(upper.join("system"), b"changed").unwrap();
        let mut record = OverlayRecord {
            id: "immutable".into(),
            target: target.clone(),
            upper: OverlayUpper::Directory {
                upper_dir: upper,
                work_dir: tmp.path().join("work"),
            },
            merged_dir: tmp.path().join("merged"),
            stage_dir: tmp.path().to_path_buf(),
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: true,
            state: OverlayState::Staged,
        };
        let error = apply_overlay(&mut record).unwrap_err();
        assert!(error.to_string().contains("immutable image rootfs"));
        assert_eq!(fs::read(target.join("system")).unwrap(), b"cached");
        assert_eq!(record.state, OverlayState::Staged);
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
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
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
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        discard_overlay(&mut rec).unwrap();
        assert!(!upper.exists());
        assert_eq!(rec.state, OverlayState::Discarded);
    }

    #[test]
    fn terminal_decisions_are_idempotent_but_cannot_be_reversed() {
        let applied_root = tempdir().unwrap();
        let applied_target = applied_root.path().join("target");
        let applied_upper = applied_root.path().join("upper");
        fs::create_dir_all(&applied_target).unwrap();
        fs::create_dir_all(&applied_upper).unwrap();
        fs::write(applied_upper.join("value"), b"applied").unwrap();
        let mut applied = OverlayRecord {
            id: "applied-run".into(),
            target: applied_target,
            upper: OverlayUpper::Directory {
                upper_dir: applied_upper,
                work_dir: applied_root.path().join("work"),
            },
            merged_dir: applied_root.path().join("merged"),
            stage_dir: applied_root.path().to_path_buf(),
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        apply_overlay(&mut applied).unwrap();
        apply_overlay(&mut applied).unwrap();
        let error = discard_overlay(&mut applied).unwrap_err();
        assert_eq!(
            error.to_string(),
            "overlay applied-run was already applied; drop cannot undo applied changes"
        );
        assert_eq!(applied.state, OverlayState::Applied);

        let dropped_root = tempdir().unwrap();
        let dropped_upper = dropped_root.path().join("upper");
        fs::create_dir_all(&dropped_upper).unwrap();
        fs::write(dropped_upper.join("value"), b"discarded").unwrap();
        let mut dropped = OverlayRecord {
            id: "dropped-run".into(),
            target: dropped_root.path().join("target"),
            upper: OverlayUpper::Directory {
                upper_dir: dropped_upper,
                work_dir: dropped_root.path().join("work"),
            },
            merged_dir: dropped_root.path().join("merged"),
            stage_dir: dropped_root.path().to_path_buf(),
            excluded_paths: Vec::new(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        discard_overlay(&mut dropped).unwrap();
        discard_overlay(&mut dropped).unwrap();
        let error = apply_overlay(&mut dropped).unwrap_err();
        assert_eq!(
            error.to_string(),
            "overlay dropped-run was already dropped; apply cannot recover discarded changes"
        );
        assert_eq!(dropped.state, OverlayState::Discarded);
    }
}

//! Filesystem-backed logical checkpoints for Agent Runs.

use crate::runtime::{
    is_live, restore_overlay_upper, snapshot_overlay_upper, OverlayState, RunRecord,
};
use crate::unix_now_ms;
use crate::util::{atomic_write, create_dir_all_durable};
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const CHECKPOINTS_DIR: &str = "checkpoints";
const CHECKPOINT_FILENAME: &str = "checkpoint.json";
const CHECKPOINT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointConsistency {
    /// The process tree was no longer running when the filesystem was copied.
    Stopped,
    /// Reserved for a live Agent ABI quiesce + effect-journal barrier.
    AgentQuiesced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicalCheckpoint {
    pub schema_version: u32,
    pub checkpoint_id: String,
    pub run_id: String,
    pub created_at_unix_ms: u64,
    pub consistency: CheckpointConsistency,
    pub source_stage: PathBuf,
    pub upper_snapshot: PathBuf,
    pub target: PathBuf,
    #[serde(default)]
    pub lower_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub protect_target: bool,
}

impl LogicalCheckpoint {
    pub fn manifest_path(&self) -> PathBuf {
        self.upper_snapshot
            .parent()
            .unwrap_or(&self.source_stage)
            .join(CHECKPOINT_FILENAME)
    }

    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let manifest = if path.is_dir() {
            path.join(CHECKPOINT_FILENAME)
        } else {
            path.to_path_buf()
        };
        let checkpoint: Self = serde_json::from_slice(&fs::read(&manifest)?)?;
        anyhow::ensure!(
            checkpoint.schema_version == CHECKPOINT_SCHEMA_VERSION,
            "unsupported logical checkpoint schema {}; expected {}",
            checkpoint.schema_version,
            CHECKPOINT_SCHEMA_VERSION
        );
        Ok(checkpoint)
    }
}

pub fn create_logical_checkpoint(
    record: &RunRecord,
    requested_id: Option<&str>,
) -> anyhow::Result<LogicalCheckpoint> {
    anyhow::ensure!(
        !is_live(&record.stage_dir())?,
        "Run {} is live; CLI checkpoint requires a stopped Run so it cannot copy a changing upper",
        record.run_id
    );
    create_checkpoint(record, requested_id, CheckpointConsistency::Stopped)
}

pub(crate) fn create_agent_quiesced_checkpoint(
    record: &RunRecord,
    checkpoint_id: &str,
) -> anyhow::Result<LogicalCheckpoint> {
    create_checkpoint(
        record,
        Some(checkpoint_id),
        CheckpointConsistency::AgentQuiesced,
    )
}

fn create_checkpoint(
    record: &RunRecord,
    requested_id: Option<&str>,
    consistency: CheckpointConsistency,
) -> anyhow::Result<LogicalCheckpoint> {
    let overlay = record
        .overlay
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Run {} has no OverlayFS stage", record.run_id))?;
    let expected_state = match consistency {
        CheckpointConsistency::Stopped => OverlayState::Staged,
        CheckpointConsistency::AgentQuiesced => OverlayState::Active,
    };
    anyhow::ensure!(
        overlay.state == expected_state,
        "Run {} filesystem is {:?}; {:?} checkpoint requires {:?}",
        record.run_id,
        overlay.state,
        consistency,
        expected_state
    );
    let checkpoint_id = requested_id
        .map(str::to_owned)
        .unwrap_or_else(|| format!("checkpoint-{}", uuid::Uuid::new_v4().simple()));
    validate_checkpoint_id(&checkpoint_id)?;
    let root = record
        .stage_dir()
        .join(CHECKPOINTS_DIR)
        .join(&checkpoint_id);
    anyhow::ensure!(
        !root.exists(),
        "logical checkpoint already exists: {}",
        root.display()
    );
    create_dir_all_durable(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let upper_snapshot = root.join("upper");
    if let Err(error) = snapshot_overlay_upper(overlay, &upper_snapshot) {
        let _ = fs::remove_dir_all(&root);
        return Err(error.into());
    }
    let checkpoint = LogicalCheckpoint {
        schema_version: CHECKPOINT_SCHEMA_VERSION,
        checkpoint_id,
        run_id: record.run_id.clone(),
        created_at_unix_ms: unix_now_ms(),
        consistency,
        source_stage: record.stage_dir(),
        upper_snapshot,
        target: overlay.target.clone(),
        lower_dirs: if record.overlay_lowers.is_empty() {
            vec![overlay.target.clone()]
        } else {
            record.overlay_lowers.clone()
        },
        protect_target: overlay.protect_target,
    };
    atomic_write(
        &root.join(CHECKPOINT_FILENAME),
        &serde_json::to_vec_pretty(&checkpoint)?,
        0o600,
    )?;
    Ok(checkpoint)
}

pub fn latest_logical_checkpoint(record: &RunRecord) -> anyhow::Result<LogicalCheckpoint> {
    let root = record.stage_dir().join(CHECKPOINTS_DIR);
    let mut checkpoints = if root.is_dir() {
        fs::read_dir(root)?
            .filter_map(Result::ok)
            .filter_map(|entry| LogicalCheckpoint::read(&entry.path()).ok())
            .filter(|checkpoint| checkpoint.run_id == record.run_id)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    checkpoints.sort_by_key(|checkpoint| std::cmp::Reverse(checkpoint.created_at_unix_ms));
    checkpoints
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Run {} has no logical checkpoints", record.run_id))
}

pub fn restore_logical_checkpoint(
    checkpoint: &LogicalCheckpoint,
    destination_upper: &Path,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        checkpoint.upper_snapshot.is_dir(),
        "logical checkpoint upper is missing: {}",
        checkpoint.upper_snapshot.display()
    );
    let source = checkpoint.upper_snapshot.canonicalize()?;
    let destination = absolute_candidate(destination_upper)?;
    anyhow::ensure!(
        !source.starts_with(&destination) && !destination.starts_with(&source),
        "checkpoint source and fork upper must not overlap: source={}, destination={}",
        source.display(),
        destination.display()
    );
    restore_overlay_upper(&checkpoint.upper_snapshot, destination_upper)?;
    Ok(())
}

fn absolute_candidate(path: &Path) -> anyhow::Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute
        .parent()
        .ok_or_else(|| anyhow::anyhow!("destination upper has no parent"))?
        .canonicalize()?;
    let name = absolute
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("destination upper has no final component"))?;
    Ok(parent.join(name))
}

fn validate_checkpoint_id(id: &str) -> anyhow::Result<()> {
    let trimmed = id.trim();
    anyhow::ensure!(!trimmed.is_empty(), "checkpoint id cannot be empty");
    anyhow::ensure!(
        trimmed != "." && trimmed != ".." && !trimmed.contains('/') && !trimmed.contains('\\'),
        "checkpoint id must be one path-safe segment"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{OverlayRecord, OverlayUpper, RunLineage};
    use std::os::unix::fs::MetadataExt;

    fn stopped_record(root: &Path) -> RunRecord {
        let target = root.join("target");
        let upper = root.join("upper");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&upper).unwrap();
        RunRecord {
            schema_version: 1,
            run_id: "run-source".into(),
            parent_run_id: None,
            task_id: None,
            session_id: "run-source".into(),
            agent: "codex".into(),
            pid: 0,
            command: vec!["codex".into()],
            executor: None,
            state: "completed".into(),
            started_at_unix_ms: 1,
            finished_at_unix_ms: Some(2),
            storage: root.to_path_buf(),
            workspace: None,
            overlaynet_listen: None,
            network_interception: None,
            network_interception_metrics: None,
            gateway_listen: None,
            network: serde_json::json!({"mode": "ambient"}),
            network_policy: None,
            environment: Default::default(),
            resource_limits: Default::default(),
            overlay: Some(OverlayRecord {
                id: "run-source".into(),
                target: target.clone(),
                upper: OverlayUpper::Directory {
                    upper_dir: upper,
                    work_dir: root.join("work"),
                },
                merged_dir: root.join("merged"),
                stage_dir: root.to_path_buf(),
                excluded_paths: Vec::new(),
                auto_apply: false,
                auto_discard: false,
                protect_target: false,
                state: OverlayState::Staged,
            }),
            overlay_lowers: vec![target],
            lineage: Some(RunLineage {
                parent_run_id: "parent".into(),
                checkpoint_id: "parent-cp".into(),
            }),
            orchestration: Default::default(),
        }
    }

    #[test]
    fn stopped_checkpoint_preserves_links_and_can_seed_a_fork() {
        let temp = tempfile::tempdir().unwrap();
        let mut record = stopped_record(temp.path());
        record.overlay.as_mut().unwrap().protect_target = true;
        let upper = record.overlay.as_ref().unwrap().upper.path();
        fs::write(upper.join("one"), b"value").unwrap();
        fs::hard_link(upper.join("one"), upper.join("two")).unwrap();
        std::os::unix::fs::symlink("one", upper.join("link")).unwrap();

        let checkpoint = create_logical_checkpoint(&record, Some("before-refactor")).unwrap();
        assert!(checkpoint.protect_target);
        let restored = temp.path().join("restored");
        restore_logical_checkpoint(&checkpoint, &restored).unwrap();

        assert_eq!(fs::read(restored.join("one")).unwrap(), b"value");
        assert_eq!(
            fs::read_link(restored.join("link")).unwrap(),
            PathBuf::from("one")
        );
        assert_eq!(
            fs::metadata(restored.join("one")).unwrap().ino(),
            fs::metadata(restored.join("two")).unwrap().ino()
        );
        assert_eq!(
            latest_logical_checkpoint(&record).unwrap().checkpoint_id,
            "before-refactor"
        );
    }

    #[test]
    fn checkpoint_ids_cannot_escape_the_stage() {
        let temp = tempfile::tempdir().unwrap();
        let record = stopped_record(temp.path());
        assert!(create_logical_checkpoint(&record, Some("../escape")).is_err());
    }
}

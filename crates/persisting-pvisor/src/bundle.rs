//! Durable, versioned summary of one pVisor Run.

use crate::runtime::{overlay_status, OverlayState, RunLineage, RunRecord};
use crate::util::{atomic_write, sync_directory};
use crate::{unix_now_ms, AgentAbiSnapshot};
use persisting_control::{
    ArtifactRef, ExecutorDescriptor, IsolationKind, ProcessOutput, RunFailure, RunResult, RunState,
};
use persisting_overlaynet::{InterceptionProfile, InterceptionSnapshot};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const RUN_BUNDLE_SCHEMA_VERSION: u32 = 1;
pub const RUN_BUNDLE_FILENAME: &str = "run-bundle.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunBundle {
    pub schema_version: u32,
    pub generated_at_unix_ms: u64,
    pub run: BundleRun,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<RunLineage>,
    pub safety: SafetySummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<FilesystemSummary>,
    pub network: NetworkSummary,
    pub agent_abi: AgentAbiSnapshot,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub orchestration: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub artifacts: Vec<BundleArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleRun {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub attempt_id: String,
    pub session_id: String,
    pub agent: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<ExecutorDescriptor>,
    pub state: RunState,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunFailure>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub output: ProcessOutput,
    #[serde(default)]
    pub metrics: std::collections::BTreeMap<String, f64>,
    #[serde(default)]
    pub result_artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_stream_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetySummary {
    pub safe_profile_requested: bool,
    pub host_process: bool,
    pub filesystem_changes_staged: bool,
    pub filesystem_non_bypassable: bool,
    pub network_non_bypassable: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemSummary {
    pub state: OverlayState,
    pub target: PathBuf,
    pub upper: PathBuf,
    pub changed_files: usize,
    pub whiteouts: usize,
    #[serde(default)]
    pub sample_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkSummary {
    pub policy: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interception: Option<InterceptionProfile>,
    /// Present only when a driver exports final counters into the bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intercepted: Option<InterceptionSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleArtifact {
    pub kind: String,
    pub path: PathBuf,
}

impl RunBundle {
    pub fn capture(
        record: &RunRecord,
        result: &RunResult,
        agent_abi: AgentAbiSnapshot,
        safe_profile_requested: bool,
    ) -> anyhow::Result<Self> {
        let filesystem = record
            .overlay
            .as_ref()
            .map(|overlay| {
                let status = overlay_status(overlay)?;
                Ok::<_, anyhow::Error>(FilesystemSummary {
                    state: overlay.state,
                    target: overlay.target.clone(),
                    upper: overlay.upper.path().to_path_buf(),
                    changed_files: status.changed_files,
                    whiteouts: status.whiteouts,
                    sample_paths: status.sample_paths,
                })
            })
            .transpose()?;
        let filesystem_changes_staged = filesystem
            .as_ref()
            .is_some_and(|fs| fs.state == OverlayState::Staged);
        let network_non_bypassable = record
            .network_interception
            .as_ref()
            .is_some_and(InterceptionProfile::is_enforcing);
        let host_process = record
            .executor
            .as_ref()
            .is_none_or(|executor| executor.isolation == IsolationKind::HostProcess);
        let mut safety_warnings = Vec::new();
        if safe_profile_requested {
            if host_process {
                safety_warnings.push(
                    "local-process execution can access host paths outside the staged workspace"
                        .into(),
                );
            }
            if !network_non_bypassable {
                safety_warnings.push(
                    "network policy covers cooperative proxy traffic; direct sockets may bypass it"
                        .into(),
                );
            }
        }
        let mut artifacts = vec![BundleArtifact {
            kind: "run-record".into(),
            path: record.stage_dir().join("run.json"),
        }];
        for (kind, relative) in [
            ("capture", ".capture"),
            ("chronicle", "chronicle"),
            ("live-markdown", "live.md"),
        ] {
            let path = record.storage.join(relative);
            if path.exists() {
                artifacts.push(BundleArtifact {
                    kind: kind.into(),
                    path,
                });
            }
        }

        Ok(Self {
            schema_version: RUN_BUNDLE_SCHEMA_VERSION,
            generated_at_unix_ms: unix_now_ms(),
            run: BundleRun {
                run_id: record.run_id.clone(),
                parent_run_id: record.parent_run_id.clone(),
                task_id: record.task_id.clone(),
                attempt_id: result.attempt_id.as_str().to_string(),
                session_id: record.session_id.clone(),
                agent: record.agent.clone(),
                command: record.command.clone(),
                executor: record.executor.clone(),
                state: result.state,
                started_at_unix_ms: result.started_at_unix_ms,
                finished_at_unix_ms: result.finished_at_unix_ms,
                duration_ms: result
                    .finished_at_unix_ms
                    .saturating_sub(result.started_at_unix_ms),
                exit_code: result.exit_code,
                failure: result.failure.clone(),
                warnings: result.warnings.clone(),
                output: result.output.clone(),
                metrics: result.metrics.clone(),
                result_artifacts: result.artifacts.clone(),
                event_stream_ref: result.event_stream_ref.clone(),
            },
            lineage: record.lineage.clone(),
            safety: SafetySummary {
                safe_profile_requested,
                host_process,
                filesystem_changes_staged,
                filesystem_non_bypassable: false,
                network_non_bypassable,
                warnings: safety_warnings,
            },
            filesystem,
            network: NetworkSummary {
                policy: record
                    .network_policy
                    .clone()
                    .unwrap_or_else(|| record.network.clone()),
                interception: record.network_interception.clone(),
                intercepted: record.network_interception_metrics.clone(),
            },
            agent_abi,
            orchestration: record.orchestration.clone(),
            artifacts,
        })
    }

    pub fn path(stage_dir: &Path) -> PathBuf {
        stage_dir.join(RUN_BUNDLE_FILENAME)
    }

    pub fn write(&self, stage_dir: &Path) -> anyhow::Result<PathBuf> {
        let path = Self::path(stage_dir);
        atomic_write(&path, &serde_json::to_vec_pretty(self)?, 0o600)?;
        Ok(path)
    }

    pub fn read(stage_dir: &Path) -> anyhow::Result<Self> {
        let path = Self::path(stage_dir);
        let bundle: Self = serde_json::from_slice(&fs::read(&path)?)?;
        anyhow::ensure!(
            bundle.schema_version == RUN_BUNDLE_SCHEMA_VERSION,
            "unsupported Run Bundle schema {}; expected {}",
            bundle.schema_version,
            RUN_BUNDLE_SCHEMA_VERSION
        );
        Ok(bundle)
    }

    pub(crate) fn invalidate(stage_dir: &Path) -> anyhow::Result<()> {
        let path = Self::path(stage_dir);
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(stage_dir),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{OverlayRecord, OverlayUpper};
    use persisting_control::{AttemptId, RunId};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn bundle_is_private_and_roundtrips() {
        let temp = tempfile::tempdir().unwrap();
        let upper = temp.path().join("upper");
        fs::create_dir(&upper).unwrap();
        fs::write(upper.join("changed.txt"), b"changed").unwrap();
        let record = RunRecord {
            schema_version: 1,
            run_id: "run-1".into(),
            parent_run_id: Some("job-1".into()),
            task_id: Some("task-1".into()),
            session_id: "run-1".into(),
            agent: "codex".into(),
            pid: 1,
            command: vec!["codex".into()],
            executor: None,
            state: "completed".into(),
            started_at_unix_ms: 10,
            finished_at_unix_ms: Some(20),
            storage: temp.path().to_path_buf(),
            overlaynet_listen: None,
            network_interception: Some(InterceptionProfile::explicit_proxy()),
            network_interception_metrics: None,
            gateway_listen: None,
            network: serde_json::json!({"mode": "ambient"}),
            network_policy: None,
            overlay: Some(OverlayRecord {
                id: "run-1".into(),
                target: temp.path().join("target"),
                upper: OverlayUpper::Directory {
                    upper_dir: upper,
                    work_dir: temp.path().join("work"),
                },
                merged_dir: temp.path().join("merged"),
                stage_dir: temp.path().to_path_buf(),
                auto_apply: false,
                auto_discard: false,
                state: OverlayState::Staged,
            }),
            overlay_lowers: vec![],
            lineage: None,
            orchestration: std::collections::BTreeMap::from([(
                "ppilot.job_id".into(),
                serde_json::json!("job-1"),
            )]),
        };
        let result = RunResult {
            run_id: RunId::new("run-1"),
            attempt_id: AttemptId::new("attempt-1"),
            lease_epoch: 1,
            state: RunState::Completed,
            started_at_unix_ms: 10,
            finished_at_unix_ms: 20,
            exit_code: Some(0),
            failure: None,
            output: Default::default(),
            value: None,
            metrics: Default::default(),
            artifacts: vec![],
            event_stream_ref: None,
            warnings: vec![],
        };
        let abi = AgentAbiSnapshot {
            run_id: "run-1".into(),
            attempt_id: "attempt-1".into(),
            directive_seq: 0,
            directive: crate::AgentDirective::Continue,
            clients: vec![],
            processes: vec![],
            effects: vec![],
        };
        let bundle = RunBundle::capture(&record, &result, abi, true).unwrap();
        let path = bundle.write(temp.path()).unwrap();
        assert_eq!(RunBundle::read(temp.path()).unwrap().run.run_id, "run-1");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(bundle.filesystem.unwrap().changed_files, 1);
        assert!(!bundle.safety.network_non_bypassable);
        assert_eq!(bundle.run.parent_run_id.as_deref(), Some("job-1"));
        assert_eq!(bundle.run.task_id.as_deref(), Some("task-1"));
        assert_eq!(bundle.orchestration["ppilot.job_id"], "job-1");
    }
}

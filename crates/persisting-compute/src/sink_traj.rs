//! Vortex trajectory sink: TaskResult → CaptureRecord → TrajectoryAppend.
//!
//! Enabled with feature `traj-sink`. Always Tee with [`JsonlFileSink`] so
//! `--resume` keeps using the JSONL task_id ledger.

use crate::sink::ResultSink;
use crate::task::TaskResult;
use anyhow::{Context, Result};
use async_trait::async_trait;
use persisting_capture::record::{now_rfc3339, record_to_engine_line, CaptureRecord};
use persisting_proto::{TrajectoryAppendRequest, TrajectoryStorageFormat};
use std::collections::HashSet;
use std::sync::Mutex;

/// Append terminal compute results as `compute.result` / `compute.failure` events.
pub struct VortexResultSink {
    storage: String,
    agent_id: String,
    session_id: String,
    seen: Mutex<HashSet<String>>,
}

impl VortexResultSink {
    pub fn new(
        storage: impl Into<String>,
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            storage: storage.into(),
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            seen: Mutex::new(HashSet::new()),
        }
    }

    pub fn storage(&self) -> &str {
        &self.storage
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Seed dedup set (e.g. from JSONL ledger) so re-appends skip known `task_id`s.
    pub fn seed_seen(&self, ids: impl IntoIterator<Item = String>) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.extend(ids);
        }
    }

    fn to_record(&self, result: &TaskResult) -> Result<CaptureRecord> {
        let kind = if result.ok && !result.cancelled {
            "compute.result"
        } else {
            "compute.failure"
        };
        let payload = serde_json::to_value(result).context("TaskResult to JSON")?;
        Ok(CaptureRecord {
            seq: 0,
            source: "persisting-compute".into(),
            kind: kind.into(),
            timestamp: Some(now_rfc3339()),
            session_id: Some(self.session_id.clone()),
            agent_id: Some(self.agent_id.clone()),
            parent_uuid: None,
            trace_id: None,
            call_id: Some(result.task_id.clone()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload,
        })
    }

    async fn append_record(&self, result: &TaskResult) -> Result<()> {
        {
            let mut seen = self
                .seen
                .lock()
                .map_err(|_| anyhow::anyhow!("vortex sink lock poisoned"))?;
            if !seen.insert(result.task_id.clone()) {
                return Ok(());
            }
        }
        let write = async {
            let rec = self.to_record(result)?;
            let line = record_to_engine_line(&rec).context("encode CaptureRecord")?;
            let req = TrajectoryAppendRequest {
                storage: self.storage.clone(),
                agent_id: self.agent_id.clone(),
                session_id: self.session_id.clone(),
                root_session_id: None,
                records_ronl: line,
                storage_format: TrajectoryStorageFormat::Vortex,
            };
            // bridge is sync (blocks on runtime inside engine); ok from async via spawn_blocking.
            let resp =
                tokio::task::spawn_blocking(move || persisting_engine::trajectory_append(req))
                    .await
                    .context("join trajectory_append")?
                    .context("trajectory_append")?;
            tracing::debug!(
                task_id = %result.task_id,
                accepted = resp.accepted_records,
                dataset = %resp.dataset,
                "compute result appended to vortex"
            );
            Ok(())
        };
        match write.await {
            Ok(()) => Ok(()),
            Err(e) => {
                // Roll back reservation so a later retry is not permanently skipped.
                if let Ok(mut seen) = self.seen.lock() {
                    seen.remove(&result.task_id);
                }
                Err(e)
            }
        }
    }
}

#[async_trait]
impl ResultSink for VortexResultSink {
    async fn append_ready(&self, result: &TaskResult) -> Result<()> {
        self.append_record(result).await
    }

    async fn append_failure(&self, result: &TaskResult) -> Result<()> {
        self.append_record(result).await
    }
}

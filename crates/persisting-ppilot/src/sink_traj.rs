//! pChronicle trajectory sink: TaskResult → EventRecord → TrajectoryAppend.
//!
//! Enabled with feature `traj-sink`. Always Tee with [`JsonlFileSink`] so
//! `--resume` keeps using the JSONL task_id ledger.

use crate::sink::ResultSink;
use crate::task::TaskResult;
use anyhow::{Context, Result};
use async_trait::async_trait;
use persisting_events::{ChronicleControl, EventIdentity, EventRecord, TrajectoryAppendRequest};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;

/// Append terminal pPilot results as `ppilot.result` / `ppilot.failure` events.
pub struct LanceResultSink {
    control: Arc<dyn ChronicleControl>,
    storage: String,
    agent_id: String,
    session_id: String,
    seen: Mutex<HashSet<String>>,
}

impl LanceResultSink {
    pub fn new(
        control: Arc<dyn ChronicleControl>,
        storage: impl Into<String>,
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            control,
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

    fn to_record(&self, result: &TaskResult) -> Result<EventRecord> {
        let kind = if result.ok && !result.cancelled {
            "ppilot.result"
        } else {
            "ppilot.failure"
        };
        let payload = serde_json::to_value(result).context("TaskResult to JSON")?;
        Ok(EventRecord {
            identity: EventIdentity {
                event_id: Some(format!("event-{}", uuid::Uuid::new_v4())),
                run_id: result.run_id.clone(),
                attempt_id: result.attempt_id.clone(),
                timestamp_unix_ms: Some((chrono::Utc::now().timestamp_millis()).max(0) as u64),
                producer: Some("persisting-ppilot".into()),
                ..EventIdentity::default()
            },
            seq: 0,
            source: "persisting-ppilot".into(),
            kind: kind.into(),
            timestamp: Some(chrono::Utc::now().to_rfc3339()),
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
                .map_err(|_| anyhow::anyhow!("lance sink lock poisoned"))?;
            if !seen.insert(result.task_id.clone()) {
                return Ok(());
            }
        }
        let write = async {
            let rec = self.to_record(result)?;
            let req = TrajectoryAppendRequest {
                storage: self.storage.clone(),
                agent_id: self.agent_id.clone(),
                session_id: self.session_id.clone(),
                format: Default::default(),
                root_session_id: None,
                records: vec![rec],
            };
            let resp = self
                .control
                .append_trajectory(req)
                .await
                .context("trajectory_append")?;
            tracing::debug!(
                task_id = %result.task_id,
                accepted = resp.accepted_records,
                dataset = %resp.dataset,
                "pPilot result appended to lance"
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
impl ResultSink for LanceResultSink {
    async fn append_ready(&self, result: &TaskResult) -> Result<()> {
        self.append_record(result).await
    }

    async fn append_failure(&self, result: &TaskResult) -> Result<()> {
        self.append_record(result).await
    }
}

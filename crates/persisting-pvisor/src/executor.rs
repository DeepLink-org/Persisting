use crate::event::RunEventPublisher;
use async_trait::async_trait;
use persisting_agentctl::{
    AttemptId, ExecutorDescriptor, RunInvocation, RunResult, RunSpec, RunState, RunStatus,
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub(crate) struct AttemptAttachments {
    pub vm_network: Option<Arc<std::sync::Mutex<Option<crate::runtime::VmNetworkAttachment>>>>,
}

#[derive(Clone)]
pub struct AttemptContext {
    spec: Arc<RunSpec>,
    attempt_id: AttemptId,
    cancel: CancellationToken,
    status: watch::Sender<RunStatus>,
    events: RunEventPublisher,
    agentctl: crate::AgentCtlControl,
    attachments: AttemptAttachments,
}

impl AttemptContext {
    pub(crate) fn new(
        spec: Arc<RunSpec>,
        attempt_id: AttemptId,
        cancel: CancellationToken,
        status: watch::Sender<RunStatus>,
        events: RunEventPublisher,
        agentctl: crate::AgentCtlControl,
        attachments: AttemptAttachments,
    ) -> Self {
        Self {
            spec,
            attempt_id,
            cancel,
            status,
            events,
            agentctl,
            attachments,
        }
    }

    pub fn spec(&self) -> &RunSpec {
        &self.spec
    }

    pub fn attempt_id(&self) -> &AttemptId {
        &self.attempt_id
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub(crate) fn take_vm_network(
        &self,
    ) -> anyhow::Result<Option<crate::runtime::VmNetworkAttachment>> {
        let Some(attachment) = &self.attachments.vm_network else {
            return Ok(None);
        };
        let mut attachment = attachment
            .lock()
            .map_err(|_| anyhow::anyhow!("VM network attachment lock poisoned"))?;
        Ok(attachment.take())
    }

    pub fn events(&self) -> &RunEventPublisher {
        &self.events
    }

    pub(crate) fn import_delegated_agentctl(&self, snapshot: crate::AgentCtlSnapshot) {
        self.agentctl.import_delegated_snapshot(snapshot);
    }

    pub async fn transition(&self, state: RunState, message: impl Into<Option<String>>) {
        let now = crate::util::unix_now_ms();
        let message = message.into();
        self.status.send_modify(|status| {
            status.state = state;
            status.updated_at_unix_ms = now;
            status.message = message.clone();
            if matches!(state, RunState::Starting | RunState::Running)
                && status.attempt.started_at_unix_ms.is_none()
            {
                status.attempt.started_at_unix_ms = Some(now);
            }
            if state.is_terminal() {
                status.attempt.finished_at_unix_ms = Some(now);
            }
        });
        let _ = self
            .events
            .publish(
                "run.state_changed",
                "runtime",
                json!({
                    "state": state,
                    "message": message,
                }),
            )
            .await;
    }

    /// Make a terminal status visible after finalization and terminal-event commit.
    pub(crate) fn finish(&self, state: RunState, message: Option<String>) {
        let now = crate::util::unix_now_ms();
        self.status.send_modify(|status| {
            status.state = state;
            status.updated_at_unix_ms = now;
            status.message = message.clone();
            status.attempt.finished_at_unix_ms = Some(now);
        });
    }
}

#[async_trait]
pub trait RunExecutor: Send + Sync {
    fn descriptor(&self) -> ExecutorDescriptor;
    fn supports(&self, invocation: &RunInvocation) -> bool;
    /// Whether this executor consumes pVisor's VM network attachment.
    ///
    /// A virtual-machine descriptor alone is not sufficient to claim that the
    /// Attempt network is non-bypassable: pluggable executors must explicitly
    /// opt into the transport handoff contract.
    fn supports_vm_network_attachment(&self) -> bool {
        false
    }
    async fn execute(&self, context: AttemptContext) -> RunResult;
}

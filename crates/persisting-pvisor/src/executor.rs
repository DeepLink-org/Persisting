use crate::event::RunEventPublisher;
use async_trait::async_trait;
use persisting_proto::{
    AttemptId, ExecutorDescriptor, RunInvocation, RunResult, RunSpec, RunState, RunStatus,
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct AttemptContext {
    spec: Arc<RunSpec>,
    attempt_id: AttemptId,
    cancel: CancellationToken,
    status: watch::Sender<RunStatus>,
    events: RunEventPublisher,
}

impl AttemptContext {
    pub(crate) fn new(
        spec: Arc<RunSpec>,
        attempt_id: AttemptId,
        cancel: CancellationToken,
        status: watch::Sender<RunStatus>,
        events: RunEventPublisher,
    ) -> Self {
        Self {
            spec,
            attempt_id,
            cancel,
            status,
            events,
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

    pub fn events(&self) -> &RunEventPublisher {
        &self.events
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
    async fn execute(&self, context: AttemptContext) -> RunResult;
}

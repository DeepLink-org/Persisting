use crate::event::{EventSink, NoopEventSink, RunEventPublisher};
use crate::executor::{AttemptContext, RunExecutor};
use crate::process::ProcessExecutor;
use persisting_proto::{
    AttemptId, AttemptInfo, EventEnvelope, PolicyMode, RunResult, RunSpec, RunState, RunStatus,
    RUNTIME_SCHEMA_VERSION,
};
use serde_json::json;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum PVisorError {
    #[error("invalid RunSpec: {0}")]
    InvalidSpec(String),
    #[error("no executor supports this invocation")]
    UnsupportedInvocation,
    #[error("executor `{0}` cannot enforce the requested capability policy")]
    UnsupportedPolicy(String),
    #[error("event sink rejected run creation: {0}")]
    EventSink(#[source] anyhow::Error),
    #[error("run task failed to join: {0}")]
    Join(#[from] tokio::task::JoinError),
}

pub fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub type RunEventStream = broadcast::Receiver<EventEnvelope>;

pub struct RunHandle {
    run_id: persisting_proto::RunId,
    attempt_id: AttemptId,
    status: watch::Receiver<RunStatus>,
    cancellation: CancellationToken,
    events: RunEventPublisher,
    join: JoinHandle<RunResult>,
}

impl RunHandle {
    pub fn run_id(&self) -> &persisting_proto::RunId {
        &self.run_id
    }

    pub fn attempt_id(&self) -> &AttemptId {
        &self.attempt_id
    }

    pub fn status(&self) -> RunStatus {
        self.status.borrow().clone()
    }

    pub async fn status_changed(&mut self) -> Option<RunStatus> {
        self.status.changed().await.ok()?;
        Some(self.status())
    }

    pub fn subscribe_events(&self) -> RunEventStream {
        self.events.subscribe()
    }

    /// Cooperative request followed by executor-specific termination.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub async fn wait(self) -> Result<RunResult, PVisorError> {
        Ok(self.join.await?)
    }
}

#[derive(Clone)]
pub struct PVisor {
    executors: Arc<Vec<Arc<dyn RunExecutor>>>,
    event_sink: Arc<dyn EventSink>,
}

impl Default for PVisor {
    fn default() -> Self {
        Self::new()
    }
}

impl PVisor {
    pub fn new() -> Self {
        Self {
            executors: Arc::new(vec![Arc::new(ProcessExecutor)]),
            event_sink: Arc::new(NoopEventSink),
        }
    }

    pub fn with_event_sink(mut self, event_sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = event_sink;
        self
    }

    pub fn with_executors(
        executors: Vec<Arc<dyn RunExecutor>>,
        event_sink: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            executors: Arc::new(executors),
            event_sink,
        }
    }

    pub async fn submit(&self, spec: RunSpec) -> Result<RunHandle, PVisorError> {
        validate_spec(&spec)?;
        let executor = self
            .executors
            .iter()
            .find(|executor| executor.supports(&spec.invocation))
            .cloned()
            .ok_or(PVisorError::UnsupportedInvocation)?;
        let descriptor = executor.descriptor();
        if spec.runtime.policy_mode == PolicyMode::Enforce && !descriptor.enforces_capabilities {
            return Err(PVisorError::UnsupportedPolicy(descriptor.name));
        }

        let run_id = spec.run_id.clone();
        let attempt_id = AttemptId::new(format!("attempt-{}", uuid::Uuid::new_v4()));
        let now = unix_now_ms();
        let initial = RunStatus {
            run_id: run_id.clone(),
            state: RunState::Created,
            attempt: AttemptInfo {
                attempt_id: attempt_id.clone(),
                number: 0,
                executor: descriptor.clone(),
                started_at_unix_ms: None,
                finished_at_unix_ms: None,
            },
            updated_at_unix_ms: now,
            message: None,
        };
        let (status_tx, status_rx) = watch::channel(initial);
        let (live_tx, _) = broadcast::channel(256);
        let events = RunEventPublisher::new(
            run_id.clone(),
            attempt_id.clone(),
            "persisting-pvisor",
            Arc::clone(&self.event_sink),
            live_tx,
        );
        events
            .publish(
                "run.created",
                "runtime",
                json!({
                    "agent": spec.agent,
                    "task_id": spec.task_id,
                    "executor": descriptor,
                    "policy_mode": spec.runtime.policy_mode,
                }),
            )
            .await
            .map_err(PVisorError::EventSink)?;

        let cancellation = CancellationToken::new();
        let context = AttemptContext::new(
            Arc::new(spec),
            attempt_id.clone(),
            cancellation.clone(),
            status_tx,
            events.clone(),
        );
        let join = tokio::spawn(async move {
            let mut result = executor.execute(context.clone()).await;
            context.transition(result.state, None).await;
            let kind = match result.state {
                RunState::Completed => "run.completed",
                RunState::Cancelled => "run.cancelled",
                _ => "run.failed",
            };
            if let Err(error) = context
                .events()
                .publish(
                    kind,
                    "runtime",
                    json!({
                        "state": result.state,
                        "exit_code": result.exit_code,
                        "failure": result.failure,
                        "started_at_unix_ms": result.started_at_unix_ms,
                        "finished_at_unix_ms": result.finished_at_unix_ms,
                    }),
                )
                .await
            {
                result
                    .warnings
                    .push(format!("terminal event sink failed: {error:#}"));
            }
            result
        });

        Ok(RunHandle {
            run_id,
            attempt_id,
            status: status_rx,
            cancellation,
            events,
            join,
        })
    }
}

fn validate_spec(spec: &RunSpec) -> Result<(), PVisorError> {
    if spec.schema_version != RUNTIME_SCHEMA_VERSION {
        return Err(PVisorError::InvalidSpec(format!(
            "unsupported schema_version {}; expected {}",
            spec.schema_version, RUNTIME_SCHEMA_VERSION
        )));
    }
    if spec.run_id.is_empty() {
        return Err(PVisorError::InvalidSpec("run_id must not be empty".into()));
    }
    if spec.agent.name.trim().is_empty() {
        return Err(PVisorError::InvalidSpec(
            "agent.name must not be empty".into(),
        ));
    }
    let persisting_proto::RunInvocation::Process(process) = &spec.invocation;
    if process.program.trim().is_empty() {
        return Err(PVisorError::InvalidSpec(
            "process program must not be empty".into(),
        ));
    }
    if process.stdin == persisting_proto::StdioMode::Capture {
        return Err(PVisorError::InvalidSpec(
            "captured stdin is not supported in pVisor v1".into(),
        ));
    }
    if spec.runtime.max_output_bytes == 0 {
        return Err(PVisorError::InvalidSpec(
            "runtime.max_output_bytes must be greater than zero".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventSink;
    use persisting_proto::{NetworkCapability, RunFailureKind, RunInvocation, StdioMode};

    #[cfg(unix)]
    #[tokio::test]
    async fn process_run_completes_and_emits_lifecycle() {
        let sink = Arc::new(MemoryEventSink::default());
        let runtime = PVisor::new().with_event_sink(sink.clone());
        let mut spec = RunSpec::process("run-success", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "printf pvisor".into()];
        process.stdout = StdioMode::Capture;
        process.stderr = StdioMode::Capture;

        let handle = runtime.submit(spec).await.unwrap();
        assert_eq!(handle.status().state, RunState::Created);
        let result = handle.wait().await.unwrap();
        assert_eq!(result.state, RunState::Completed);
        assert_eq!(result.output.stdout.as_deref(), Some("pvisor"));

        let kinds: Vec<_> = sink.events().into_iter().map(|event| event.kind).collect();
        assert_eq!(kinds.first().map(String::as_str), Some("run.created"));
        assert_eq!(kinds.last().map(String::as_str), Some("run.completed"));
        assert!(kinds.iter().any(|kind| kind == "run.state_changed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_run_cancels_the_process_tree() {
        let runtime = PVisor::new();
        let mut spec = RunSpec::process("run-cancel", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "sleep 30 & wait".into()];
        process.stdout = StdioMode::Capture;
        process.stderr = StdioMode::Capture;

        let handle = runtime.submit(spec).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        handle.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), handle.wait())
            .await
            .expect("process tree did not terminate")
            .unwrap();
        assert_eq!(result.state, RunState::Cancelled);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_deadline_is_a_typed_failure() {
        let runtime = PVisor::new();
        let mut spec = RunSpec::process("run-timeout", "test-agent", "/bin/sleep");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["30".into()];
        process.stdout = StdioMode::Null;
        process.stderr = StdioMode::Null;
        spec.runtime.timeout_ms = Some(20);

        let result = runtime.submit(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert_eq!(
            result.failure.unwrap().kind,
            RunFailureKind::DeadlineExceeded
        );
    }

    #[tokio::test]
    async fn host_process_refuses_enforced_policy() {
        let runtime = PVisor::new();
        let mut spec = RunSpec::process("run-enforce", "test-agent", "echo");
        spec.runtime.policy_mode = PolicyMode::Enforce;
        spec.capabilities.network = NetworkCapability::Deny;
        let error = match runtime.submit(spec).await {
            Ok(_) => panic!("host process must not claim capability enforcement"),
            Err(error) => error,
        };
        assert!(matches!(error, PVisorError::UnsupportedPolicy(_)));
    }
}

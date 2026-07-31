use crate::executor::{AttemptContext, RunExecutor};
use async_trait::async_trait;
use persisting_proto::{
    ExecutorDescriptor, ExecutorKind, IsolationKind, ProcessInvocation, ProcessOutput, RunFailure,
    RunFailureKind, RunInvocation, RunResult, RunState, StdioMode,
};
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

#[derive(Debug, Default)]
pub struct ProcessExecutor;

#[derive(Debug)]
struct Captured {
    text: String,
    truncated: bool,
}

async fn read_limited<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<Captured> {
    let mut retained = Vec::with_capacity(limit.min(8192));
    let mut buf = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buf).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        let keep = remaining.min(read);
        retained.extend_from_slice(&buf[..keep]);
        truncated |= keep < read;
    }
    Ok(Captured {
        text: String::from_utf8_lossy(&retained).into_owned(),
        truncated,
    })
}

fn stdio(mode: StdioMode) -> Stdio {
    match mode {
        StdioMode::Inherit => Stdio::inherit(),
        StdioMode::Capture => Stdio::piped(),
        StdioMode::Null => Stdio::null(),
    }
}

impl ProcessExecutor {
    fn spawn_command(invocation: &ProcessInvocation) -> Command {
        let mut command = Command::new(&invocation.program);
        command
            .args(&invocation.args)
            .stdin(stdio(invocation.stdin))
            .stdout(stdio(invocation.stdout))
            .stderr(stdio(invocation.stderr))
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        if let Some(cwd) = &invocation.cwd {
            command.current_dir(cwd);
        }
        if !invocation.inherit_env {
            command.env_clear();
        }
        command.envs(&invocation.env);
        command
    }
}

async fn terminate_process_tree(child: &mut Child, grace_ms: u64) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // The child is the leader of the process group configured above.
            let process_group = -(pid as i32);
            unsafe {
                libc::kill(process_group, libc::SIGTERM);
            }
            if tokio::time::timeout(std::time::Duration::from_millis(grace_ms), child.wait())
                .await
                .is_ok()
            {
                return;
            }
            unsafe {
                libc::kill(process_group, libc::SIGKILL);
            }
            let _ = child.wait().await;
            return;
        }
    }

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[async_trait]
impl RunExecutor for ProcessExecutor {
    fn descriptor(&self) -> ExecutorDescriptor {
        ExecutorDescriptor {
            name: "local-process-v1".into(),
            kind: ExecutorKind::Process,
            isolation: IsolationKind::HostProcess,
            enforces_capabilities: false,
            supports_checkpoint: false,
            supports_migration: false,
        }
    }

    fn supports(&self, invocation: &RunInvocation) -> bool {
        matches!(invocation, RunInvocation::Process(_))
    }

    async fn execute(&self, context: AttemptContext) -> RunResult {
        let spec = context.spec().clone();
        let RunInvocation::Process(invocation) = &spec.invocation;
        let started_at = crate::util::unix_now_ms();
        context
            .transition(RunState::Starting, Some("spawning local process".into()))
            .await;

        let mut child = match Self::spawn_command(invocation).spawn() {
            Ok(child) => child,
            Err(error) => {
                return RunResult {
                    run_id: spec.run_id,
                    attempt_id: context.attempt_id().clone(),
                    state: RunState::Failed,
                    started_at_unix_ms: started_at,
                    finished_at_unix_ms: crate::util::unix_now_ms(),
                    exit_code: None,
                    failure: Some(RunFailure {
                        kind: RunFailureKind::Spawn,
                        message: error.to_string(),
                        retryable: false,
                    }),
                    output: ProcessOutput::default(),
                    metrics: Default::default(),
                    artifacts: Vec::new(),
                    event_stream_ref: None,
                    warnings: Vec::new(),
                };
            }
        };

        let stdout_task = child.stdout.take().map(|stdout| {
            let limit = spec.runtime.max_output_bytes;
            tokio::spawn(async move { read_limited(stdout, limit).await })
        });
        let stderr_task = child.stderr.take().map(|stderr| {
            let limit = spec.runtime.max_output_bytes;
            tokio::spawn(async move { read_limited(stderr, limit).await })
        });

        context.transition(RunState::Running, None).await;

        enum End {
            Exited(std::io::Result<std::process::ExitStatus>),
            Cancelled,
            Deadline,
        }

        let cancellation = context.cancellation();
        let end = if let Some(timeout_ms) = spec.runtime.timeout_ms {
            tokio::select! {
                biased;
                status = child.wait() => End::Exited(status),
                _ = cancellation.cancelled() => End::Cancelled,
                _ = tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)) => End::Deadline,
            }
        } else {
            tokio::select! {
                biased;
                status = child.wait() => End::Exited(status),
                _ = cancellation.cancelled() => End::Cancelled,
            }
        };

        if matches!(end, End::Cancelled | End::Deadline) {
            if matches!(end, End::Cancelled) {
                context
                    .transition(RunState::Cancelling, Some("cancellation requested".into()))
                    .await;
            }
            terminate_process_tree(&mut child, spec.runtime.termination_grace_ms).await;
        }

        let mut output = ProcessOutput::default();
        if let Some(task) = stdout_task {
            if let Ok(Ok(captured)) = task.await {
                output.stdout = Some(captured.text);
                output.stdout_truncated = captured.truncated;
            }
        }
        if let Some(task) = stderr_task {
            if let Ok(Ok(captured)) = task.await {
                output.stderr = Some(captured.text);
                output.stderr_truncated = captured.truncated;
            }
        }

        let finished_at = crate::util::unix_now_ms();
        let (state, exit_code, failure) = match end {
            End::Exited(Ok(status)) if status.success() => {
                (RunState::Completed, status.code(), None)
            }
            End::Exited(Ok(status)) => (
                RunState::Failed,
                status.code(),
                Some(RunFailure {
                    kind: RunFailureKind::ProcessExit,
                    message: match status.code() {
                        Some(code) => format!("process exited with code {code}"),
                        None => "process terminated without an exit code".into(),
                    },
                    retryable: false,
                }),
            ),
            End::Exited(Err(error)) => (
                RunState::Failed,
                None,
                Some(RunFailure {
                    kind: RunFailureKind::Infrastructure,
                    message: error.to_string(),
                    retryable: true,
                }),
            ),
            End::Cancelled => (RunState::Cancelled, None, None),
            End::Deadline => (
                RunState::Failed,
                None,
                Some(RunFailure {
                    kind: RunFailureKind::DeadlineExceeded,
                    message: format!(
                        "attempt exceeded {} ms deadline",
                        spec.runtime.timeout_ms.unwrap_or_default()
                    ),
                    retryable: false,
                }),
            ),
        };

        RunResult {
            run_id: spec.run_id,
            attempt_id: context.attempt_id().clone(),
            state,
            started_at_unix_ms: started_at,
            finished_at_unix_ms: finished_at,
            exit_code,
            failure,
            output,
            metrics: Default::default(),
            artifacts: Vec::new(),
            event_stream_ref: None,
            warnings: Vec::new(),
        }
    }
}

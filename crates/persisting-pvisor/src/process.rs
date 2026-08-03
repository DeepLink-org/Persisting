use crate::executor::{AttemptContext, RunExecutor};
use async_trait::async_trait;
use persisting_proto::{
    ExecutorDescriptor, ExecutorKind, IsolationKind, ProcessInvocation, ProcessOutput, RunFailure,
    RunFailureKind, RunInvocation, RunResult, RunState, StdioMode,
};
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

#[cfg(unix)]
struct ForegroundProcessGroup {
    terminal_fd: libc::c_int,
    original_pgrp: libc::pid_t,
}

#[cfg(unix)]
impl ForegroundProcessGroup {
    fn give_to(child: &Child, invocation: &ProcessInvocation) -> std::io::Result<Option<Self>> {
        if invocation.stdin != StdioMode::Inherit
            || unsafe { libc::isatty(libc::STDIN_FILENO) } != 1
        {
            return Ok(None);
        }
        let Some(pid) = child.id() else {
            return Ok(None);
        };
        let terminal_fd = libc::STDIN_FILENO;
        let original_pgrp = unsafe { libc::tcgetpgrp(terminal_fd) };
        if original_pgrp < 0 {
            return Err(std::io::Error::last_os_error());
        }
        set_terminal_pgrp(terminal_fd, pid as libc::pid_t)?;
        // The child may have attempted a terminal read between spawn and
        // tcsetpgrp and received SIGTTIN. Resume its whole process group.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGCONT);
        }
        Ok(Some(Self {
            terminal_fd,
            original_pgrp,
        }))
    }
}

#[cfg(unix)]
impl Drop for ForegroundProcessGroup {
    fn drop(&mut self) {
        let _ = set_terminal_pgrp(self.terminal_fd, self.original_pgrp);
    }
}

/// Change the terminal foreground group without letting a background caller
/// stop itself with SIGTTOU. Signal masking is thread-local and restored before
/// returning, so it is safe inside the multi-threaded Tokio runtime.
#[cfg(unix)]
fn set_terminal_pgrp(fd: libc::c_int, pgrp: libc::pid_t) -> std::io::Result<()> {
    unsafe {
        let mut blocked: libc::sigset_t = std::mem::zeroed();
        let mut previous: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut blocked);
        libc::sigaddset(&mut blocked, libc::SIGTTOU);
        let mask_error = libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous);
        if mask_error != 0 {
            return Err(std::io::Error::from_raw_os_error(mask_error));
        }
        let result = libc::tcsetpgrp(fd, pgrp);
        let error = (result != 0).then(std::io::Error::last_os_error);
        libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

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

fn resolve_host_program(program: &str) -> std::path::PathBuf {
    if program.contains(std::path::MAIN_SEPARATOR) {
        return program.into();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return program.into();
    };
    std::env::split_paths(&path)
        .map(|directory| directory.join(program))
        .find(|candidate| is_executable(candidate))
        .unwrap_or_else(|| program.into())
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

impl ProcessExecutor {
    fn spawn_command(invocation: &ProcessInvocation) -> Command {
        // Resolve a bare command against the host PATH before changing cwd to
        // an OverlayFS merged root. The executable belongs to the host-process
        // executor and need not exist inside the projected lower filesystem.
        let mut command = Command::new(resolve_host_program(&invocation.program));
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
                    lease_epoch: spec.lease_epoch,
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
                    value: None,
                    metrics: Default::default(),
                    artifacts: Vec::new(),
                    event_stream_ref: None,
                    warnings: Vec::new(),
                };
            }
        };

        #[cfg(unix)]
        let _foreground = match ForegroundProcessGroup::give_to(&child, invocation) {
            Ok(foreground) => foreground,
            Err(error) => {
                terminate_process_tree(&mut child, spec.runtime.termination_grace_ms).await;
                return RunResult {
                    run_id: spec.run_id,
                    attempt_id: context.attempt_id().clone(),
                    lease_epoch: spec.lease_epoch,
                    state: RunState::Failed,
                    started_at_unix_ms: started_at,
                    finished_at_unix_ms: crate::util::unix_now_ms(),
                    exit_code: None,
                    failure: Some(RunFailure {
                        kind: RunFailureKind::Infrastructure,
                        message: format!("failed to give terminal to child process: {error}"),
                        retryable: false,
                    }),
                    output: ProcessOutput::default(),
                    value: None,
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
            lease_epoch: spec.lease_epoch,
            state,
            started_at_unix_ms: started_at,
            finished_at_unix_ms: finished_at,
            exit_code,
            failure,
            output,
            value: None,
            metrics: Default::default(),
            artifacts: Vec::new(),
            event_stream_ref: None,
            warnings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn resolves_bare_program_before_overlay_cwd_is_applied() {
        let resolved = resolve_host_program("sh");
        assert!(
            resolved.is_absolute(),
            "resolved path: {}",
            resolved.display()
        );
        assert!(
            is_executable(&resolved),
            "resolved path: {}",
            resolved.display()
        );
        assert_eq!(
            resolved.file_name().and_then(|name| name.to_str()),
            Some("sh")
        );
        let path = std::env::var_os("PATH").expect("test requires PATH");
        assert!(
            std::env::split_paths(&path).any(|directory| directory.join("sh") == resolved),
            "{} was not resolved from PATH",
            resolved.display()
        );
        assert_eq!(
            resolve_host_program("./agent-script"),
            std::path::PathBuf::from("./agent-script")
        );
    }
}

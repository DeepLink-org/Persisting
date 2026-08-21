use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{ReplayError, ReplayErrorKind, ResultExt};

#[allow(dead_code)]
pub(crate) struct ProcessSpec {
    pub command: Command,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Duration,
    pub termination_grace: Duration,
    pub pipe_grace: Duration,
    pub retained_bytes: usize,
    pub log_path: PathBuf,
}

#[allow(dead_code)]
pub(crate) struct ProcessOutput {
    pub status: ExitStatus,
    pub stdout_tail: Vec<u8>,
    pub stderr_tail: Vec<u8>,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub background_cleanup: bool,
}

struct StreamCapture {
    tail: Vec<u8>,
    total: u64,
    log_error: Option<io::Error>,
}

#[allow(dead_code)]
pub(crate) fn run_process(mut spec: ProcessSpec) -> Result<ProcessOutput, ReplayError> {
    let log = owner_only_log(&spec.log_path)?;
    let log = Arc::new(Mutex::new(log));
    spec.command.stdout(Stdio::piped()).stderr(Stdio::piped());
    if spec.stdin.is_some() {
        spec.command.stdin(Stdio::piped());
    }
    #[cfg(unix)]
    unsafe {
        spec.command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        });
    }
    let mut child = spec
        .command
        .spawn()
        .replay_context(ReplayErrorKind::Executor, "spawn supervised replay process")?;
    let process_group = child.id() as i32;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ReplayError::new(ReplayErrorKind::Internal, "stdout pipe missing"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ReplayError::new(ReplayErrorKind::Internal, "stderr pipe missing"))?;
    let stdout_reader = spawn_reader(stdout, Arc::clone(&log), spec.retained_bytes);
    let stderr_reader = spawn_reader(stderr, Arc::clone(&log), spec.retained_bytes);
    if let Some(input) = spec.stdin.take() {
        let write_result = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("stdin pipe missing"))
            .and_then(|mut stdin| stdin.write_all(&input));
        if let Err(error) = write_result {
            #[cfg(unix)]
            let _ = signal_group(process_group, libc::SIGKILL);
            #[cfg(not(unix))]
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(ReplayError::new(
                ReplayErrorKind::Executor,
                format!("write supervised process stdin: {error}"),
            ));
        }
    }

    let started = Instant::now();
    let mut timed_out = false;
    let mut background_cleanup = false;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .replay_context(ReplayErrorKind::Executor, "poll supervised replay process")?
        {
            break status;
        }
        if started.elapsed() >= spec.timeout {
            timed_out = true;
            background_cleanup = true;
            break terminate_running_group(&mut child, process_group, spec.termination_grace)?;
        }
        thread::sleep(Duration::from_millis(10));
    };

    #[cfg(unix)]
    if process_group_exists(process_group)? {
        background_cleanup = true;
        terminate_remaining_group(process_group, spec.termination_grace)?;
    }

    let pipe_deadline = Instant::now() + spec.pipe_grace + spec.termination_grace;
    while (!stdout_reader.is_finished() || !stderr_reader.is_finished())
        && Instant::now() < pipe_deadline
    {
        thread::sleep(Duration::from_millis(5));
    }
    #[cfg(unix)]
    if !stdout_reader.is_finished() || !stderr_reader.is_finished() {
        background_cleanup = true;
        let _ = signal_group(process_group, libc::SIGKILL);
    }

    let stdout = stdout_reader
        .join()
        .map_err(|_| ReplayError::new(ReplayErrorKind::Internal, "stdout reader panicked"))?
        .replay_context(ReplayErrorKind::Executor, "drain supervised stdout")?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| ReplayError::new(ReplayErrorKind::Internal, "stderr reader panicked"))?
        .replay_context(ReplayErrorKind::Executor, "drain supervised stderr")?;
    if let Some(error) = stdout.log_error.or(stderr.log_error) {
        return Err(ReplayError::new(
            ReplayErrorKind::Executor,
            format!("write supervised process log: {error}"),
        ));
    }

    Ok(ProcessOutput {
        status,
        stdout_truncated: stdout.total > stdout.tail.len() as u64,
        stderr_truncated: stderr.total > stderr.tail.len() as u64,
        stdout_tail: stdout.tail,
        stderr_tail: stderr.tail,
        stdout_bytes: stdout.total,
        stderr_bytes: stderr.total,
        timed_out,
        background_cleanup,
    })
}

fn owner_only_log(path: &std::path::Path) -> Result<File, ReplayError> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path).replay_context(
        ReplayErrorKind::Executor,
        format!("create process log {}", path.display()),
    )?;
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .replay_context(
            ReplayErrorKind::Executor,
            format!("restrict process log {}", path.display()),
        )?;
    Ok(file)
}

fn spawn_reader<R>(
    mut reader: R,
    log: Arc<Mutex<File>>,
    retained_bytes: usize,
) -> thread::JoinHandle<io::Result<StreamCapture>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut tail = Vec::with_capacity(retained_bytes.min(64 * 1024));
        let mut total = 0_u64;
        let mut log_error = None;
        let mut chunk = [0_u8; 16 * 1024];
        loop {
            let count = reader.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            total = total.saturating_add(count as u64);
            if log_error.is_none() {
                let write_result = log
                    .lock()
                    .map_err(|_| io::Error::other("process log lock poisoned"))?
                    .write_all(&chunk[..count]);
                if let Err(error) = write_result {
                    log_error = Some(error);
                }
            }
            retain_tail(&mut tail, &chunk[..count], retained_bytes);
        }
        Ok(StreamCapture {
            tail,
            total,
            log_error,
        })
    })
}

fn retain_tail(tail: &mut Vec<u8>, chunk: &[u8], limit: usize) {
    if limit == 0 {
        tail.clear();
    } else if chunk.len() >= limit {
        tail.clear();
        tail.extend_from_slice(&chunk[chunk.len() - limit..]);
    } else {
        let overflow = tail.len().saturating_add(chunk.len()).saturating_sub(limit);
        if overflow != 0 {
            tail.drain(..overflow);
        }
        tail.extend_from_slice(chunk);
    }
}

#[cfg(unix)]
fn terminate_running_group(
    child: &mut std::process::Child,
    process_group: i32,
    grace: Duration,
) -> Result<ExitStatus, ReplayError> {
    let _ = signal_group(process_group, libc::SIGTERM)?;
    let deadline = Instant::now() + grace;
    loop {
        if let Some(status) = child
            .try_wait()
            .replay_context(ReplayErrorKind::Executor, "poll terminated process leader")?
        {
            if process_group_exists(process_group)? {
                let _ = signal_group(process_group, libc::SIGKILL)?;
            }
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = signal_group(process_group, libc::SIGKILL)?;
            return child
                .wait()
                .replay_context(ReplayErrorKind::Executor, "reap killed process leader");
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(not(unix))]
fn terminate_running_group(
    child: &mut std::process::Child,
    _process_group: i32,
    _grace: Duration,
) -> Result<ExitStatus, ReplayError> {
    child
        .kill()
        .replay_context(ReplayErrorKind::Executor, "kill timed out process")?;
    child
        .wait()
        .replay_context(ReplayErrorKind::Executor, "reap killed process")
}

#[cfg(unix)]
fn terminate_remaining_group(process_group: i32, grace: Duration) -> Result<(), ReplayError> {
    let _ = signal_group(process_group, libc::SIGTERM)?;
    let deadline = Instant::now() + grace;
    while process_group_exists(process_group)? && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    if process_group_exists(process_group)? {
        let _ = signal_group(process_group, libc::SIGKILL)?;
    }
    Ok(())
}

#[cfg(unix)]
fn process_group_exists(process_group: i32) -> Result<bool, ReplayError> {
    match unsafe { libc::kill(-process_group, 0) } {
        0 => Ok(true),
        _ => {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(false)
            } else {
                Err(ReplayError::new(
                    ReplayErrorKind::Executor,
                    format!("inspect replay process group {process_group}: {error}"),
                ))
            }
        }
    }
}

#[cfg(unix)]
fn signal_group(process_group: i32, signal: i32) -> Result<bool, ReplayError> {
    match unsafe { libc::kill(-process_group, signal) } {
        0 => Ok(true),
        _ => {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(false)
            } else {
                Err(ReplayError::new(
                    ReplayErrorKind::Executor,
                    format!("signal replay process group {process_group}: {error}"),
                ))
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn shell_spec(script: &str, log_path: &Path) -> ProcessSpec {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        ProcessSpec {
            command,
            stdin: None,
            timeout: Duration::from_secs(5),
            termination_grace: Duration::from_millis(100),
            pipe_grace: Duration::from_millis(100),
            retained_bytes: 64 * 1024,
            log_path: log_path.to_path_buf(),
        }
    }

    #[test]
    fn writes_configured_stdin_before_waiting() {
        let temporary = tempfile::tempdir().unwrap();
        let log_path = temporary.path().join("stdin.log");
        let mut spec = shell_spec("cat", &log_path);
        spec.stdin = Some(b"resume nonce".to_vec());

        let output = run_process(spec).unwrap();

        assert!(output.status.success());
        assert_eq!(output.stdout_tail, b"resume nonce");
    }

    #[test]
    fn drains_large_output_to_log_with_a_bounded_tail() {
        let temporary = tempfile::tempdir().unwrap();
        let log_path = temporary.path().join("large.log");
        let output = run_process(shell_spec("yes x | head -c 8388608", &log_path)).unwrap();

        assert!(output.status.success());
        assert_eq!(output.stdout_bytes, 8 * 1024 * 1024);
        assert!(output.stdout_truncated);
        assert_eq!(output.stdout_tail.len(), 64 * 1024);
        assert_eq!(std::fs::metadata(log_path).unwrap().len(), 8 * 1024 * 1024);
        assert_eq!(
            std::fs::metadata(temporary.path().join("large.log"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn cleans_background_descendants_after_the_leader_exits() {
        let temporary = tempfile::tempdir().unwrap();
        let log_path = temporary.path().join("background.log");
        let started = Instant::now();
        let output = run_process(shell_spec("sleep 30 & echo $!", &log_path)).unwrap();

        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(output.status.success());
        assert!(output.background_cleanup);
        let pid: i32 = String::from_utf8(output.stdout_tail)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
    }

    #[test]
    fn times_out_and_reaps_the_foreground_process_group() {
        let temporary = tempfile::tempdir().unwrap();
        let log_path = temporary.path().join("timeout.log");
        let mut spec = shell_spec("sleep 30", &log_path);
        spec.timeout = Duration::from_millis(100);
        let started = Instant::now();

        let output = run_process(spec).unwrap();

        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(output.timed_out);
        assert!(output.background_cleanup);
    }
}

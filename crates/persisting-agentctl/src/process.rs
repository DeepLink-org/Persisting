//! Foreground pVisor binary client used by control-plane components.

use crate::{RunResult, RunSpec};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct PVisorProcessClient {
    binary: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct PVisorProcessOptions {
    /// Root under which pVisor creates `<run_id>/`.
    pub run_home: Option<PathBuf>,
    /// Additional `pvisor run` switches, before `--run-spec`.
    pub run_args: Vec<OsString>,
}

#[derive(Deserialize)]
struct DelegatedRunOutput {
    result: RunResult,
}

impl PVisorProcessClient {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        let binary = binary.into();
        let binary = if binary.components().count() == 1 {
            std::env::current_exe()
                .ok()
                .and_then(|current| {
                    let parent = current.parent()?;
                    let sibling = parent.join(&binary);
                    if sibling.is_file() {
                        return Some(sibling);
                    }
                    parent
                        .parent()
                        .map(|target_profile| target_profile.join(&binary))
                        .filter(|candidate| candidate.is_file())
                })
                .unwrap_or(binary)
        } else {
            binary
        };
        Self { binary }
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    pub async fn run(
        &self,
        spec: &RunSpec,
        options: &PVisorProcessOptions,
        cancellation: CancellationToken,
    ) -> Result<RunResult> {
        let control = tempfile::Builder::new()
            .prefix("persisting-pvisor-client-")
            .tempdir()
            .context("create pVisor control directory")?;
        let spec_path = control.path().join("run-spec.json");
        let result_path = control.path().join("run-result.json");
        write_private_json(&spec_path, spec).await?;

        let mut command = Command::new(&self.binary);
        command.arg("run").args(&options.run_args);
        command
            .arg("--run-spec")
            .arg(&spec_path)
            .arg("--result-file")
            .arg(&result_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(run_home) = &options.run_home {
            command.arg("--run-home").arg(run_home);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("spawn pVisor binary {}", self.binary.display()))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_task = tokio::spawn(read_pipe(stdout));
        let stderr_task = tokio::spawn(read_pipe(stderr));

        let status = tokio::select! {
            status = child.wait() => status.context("wait for pVisor process")?,
            _ = cancellation.cancelled() => terminate(&mut child).await?,
        };
        let stdout = stdout_task.await.context("join pVisor stdout reader")??;
        let stderr = stderr_task.await.context("join pVisor stderr reader")??;

        let output = match tokio::fs::read(&result_path).await {
            Ok(bytes) => serde_json::from_slice::<DelegatedRunOutput>(&bytes)
                .context("decode pVisor RunResult")?,
            Err(error) => {
                anyhow::bail!(
                    "pVisor exited with {status} without a RunResult ({error}); stdout: {}; stderr: {}",
                    String::from_utf8_lossy(&stdout).trim(),
                    String::from_utf8_lossy(&stderr).trim(),
                )
            }
        };
        Ok(output.result)
    }
}

async fn read_pipe<R>(pipe: Option<R>) -> std::io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        pipe.read_to_end(&mut bytes).await?;
    }
    Ok(bytes)
}

async fn terminate(child: &mut Child) -> Result<std::process::ExitStatus> {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: `pid` belongs to the live child and SIGTERM is the delegated
        // pVisor shutdown contract, allowing it to publish a cancelled result.
        let sent = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if sent != 0 {
            child.start_kill().context("kill pVisor process")?;
        }
    }
    #[cfg(not(unix))]
    child.start_kill().context("kill pVisor process")?;

    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(status) => status.context("wait for terminating pVisor process"),
        Err(_) => {
            child.start_kill().context("force-kill pVisor process")?;
            child.wait().await.context("wait for killed pVisor process")
        }
    }
}

async fn write_private_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    tokio::fs::write(path, serde_json::to_vec_pretty(value)?)
        .await
        .with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    Ok(())
}

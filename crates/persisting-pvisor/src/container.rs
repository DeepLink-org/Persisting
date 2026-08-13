//! Docker/Podman transport that injects a target-specific pVisor and lets the
//! injected pVisor execute the Agent through the normal ProcessExecutor.

use crate::artifact::resolve_pvisor_binary;
use crate::config::{ContainerMount, ContainerPlatform, ContainerSettings};
use crate::delegated::{DelegatedRunFiles, RESULT_FILENAME, SPEC_FILENAME};
use crate::executor::{AttemptContext, RunExecutor};
use async_trait::async_trait;
use persisting_control::{
    ExecutorDescriptor, ExecutorKind, IsolationKind, ProcessOutput, RunFailure, RunFailureKind,
    RunInvocation, RunResult, RunSpec, RunState, StdioMode,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

const CAPTURE_CONFIG_ENV: &str = "PERSISTING_CAPTURE_CONFIG";
const GUEST_PVISOR: &str = "/opt/persisting/pvisor";
const GUEST_CONTROL_DIR: &str = "/run/persisting";

#[derive(Debug, Clone)]
pub struct ContainerExecutor {
    settings: ContainerSettings,
}

#[derive(Debug)]
struct Captured {
    text: String,
    truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BindMount {
    source: PathBuf,
    target: PathBuf,
    read_only: bool,
}

impl ContainerExecutor {
    pub fn new(settings: ContainerSettings) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !settings.runtime.as_os_str().is_empty(),
            "container runtime must not be empty"
        );
        anyhow::ensure!(
            !settings.image.trim().is_empty(),
            "container image must not be empty"
        );
        anyhow::ensure!(
            settings.user.is_none(),
            "container.user is not supported when pVisor is injected; run the injected pVisor as root and express Agent identity through pVisor policy"
        );
        if let Some(workdir) = &settings.workdir {
            anyhow::ensure!(
                workdir.is_absolute(),
                "container workdir must be absolute: {}",
                workdir.display()
            );
        }
        for mount in &settings.mounts {
            validate_mount(mount)?;
        }
        Ok(Self { settings })
    }

    pub fn settings(&self) -> &ContainerSettings {
        &self.settings
    }

    async fn resolve_platform(&self) -> anyhow::Result<ContainerPlatform> {
        if let Some(platform) = self.settings.platform {
            return Ok(platform);
        }
        let output = Command::new(&self.settings.runtime)
            .arg("image")
            .arg("inspect")
            .arg("--format")
            .arg("{{.Os}}/{{.Architecture}}")
            .arg(&self.settings.image)
            .output()
            .await
            .map_err(|error| anyhow::anyhow!("inspect OCI image platform: {error}"))?;
        anyhow::ensure!(
            output.status.success(),
            "cannot inspect OCI image platform; set container.platform explicitly: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .map_err(anyhow::Error::msg)
    }

    fn build_command(
        &self,
        spec: &RunSpec,
        attempt_id: &str,
        platform: ContainerPlatform,
        pvisor_binary: &Path,
        files: &DelegatedRunFiles,
    ) -> anyhow::Result<Command> {
        let RunInvocation::Process(invocation) = &spec.invocation;
        let run_id = spec.run_id.as_str();
        let limits = &spec.runtime.resource_limits;
        let mut mounts = BTreeMap::<PathBuf, BindMount>::new();
        for mount in &self.settings.mounts {
            add_mount(
                &mut mounts,
                bind_mount(&mount.source, &mount.target, mount.read_only)?,
            )?;
        }
        add_mount(
            &mut mounts,
            bind_mount(pvisor_binary, Path::new(GUEST_PVISOR), true)?,
        )?;
        let control_dir = files
            .spec_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("delegated RunSpec has no parent directory"))?;
        add_mount(
            &mut mounts,
            bind_mount(control_dir, Path::new(GUEST_CONTROL_DIR), false)?,
        )?;

        let workdir = invocation
            .cwd
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| self.settings.workdir.clone());
        if let Some(path) = invocation.cwd.as_deref().map(Path::new) {
            if path.exists() {
                let target = absolute_container_path(path)?;
                add_mount(&mut mounts, bind_mount(path, &target, false)?)?;
            } else {
                anyhow::ensure!(
                    path.is_absolute(),
                    "container-native cwd must be absolute when it is not a host path: {}",
                    path.display()
                );
            }
        }
        if let Some(value) = invocation.env.get(CAPTURE_CONFIG_ENV) {
            let path = Path::new(value);
            if path.is_absolute() && path.exists() {
                add_mount(&mut mounts, bind_mount(path, path, true)?)?;
            }
        }

        let mut command = Command::new(&self.settings.runtime);
        command
            .arg("run")
            .arg("--rm")
            .arg("--init")
            .arg("--name")
            .arg(container_name(run_id, attempt_id))
            .arg("--label")
            .arg(format!("io.persisting.run_id={run_id}"))
            .arg("--label")
            .arg(format!("io.persisting.attempt_id={attempt_id}"))
            .arg("--platform")
            .arg(platform.oci_value())
            .arg("--network")
            .arg(self.settings.network.as_runtime_value())
            .arg("--user")
            .arg("0:0")
            .arg("--entrypoint")
            .arg(GUEST_PVISOR);

        if let Some(bytes) = limits.memory_bytes {
            command.arg("--memory").arg(format!("{bytes}b"));
        }
        if let Some(processes) = limits.processes {
            command.arg("--pids-limit").arg(processes.to_string());
        }
        if let Some(milliseconds) = limits.cpu_time_ms {
            let seconds = milliseconds.div_ceil(1_000).max(1);
            command
                .arg("--ulimit")
                .arg(format!("cpu={seconds}:{seconds}"));
        }
        if let Some(open_files) = limits.open_files {
            command
                .arg("--ulimit")
                .arg(format!("nofile={open_files}:{open_files}"));
        }
        if let Some(bytes) = limits.file_size_bytes {
            let blocks = bytes.div_ceil(512);
            command
                .arg("--ulimit")
                .arg(format!("fsize={blocks}:{blocks}"));
        }

        if invocation.stdin == StdioMode::Inherit {
            command.arg("--interactive");
            if invocation.stdout == StdioMode::Inherit
                && invocation.stderr == StdioMode::Inherit
                && terminal_is_tty()
            {
                command.arg("--tty");
            }
        }
        if self.settings.read_only_rootfs {
            command
                .arg("--read-only")
                .arg("--tmpfs")
                .arg("/tmp:rw,nosuid,nodev,mode=1777");
        }
        if let Some(workdir) = workdir {
            anyhow::ensure!(
                workdir.is_absolute(),
                "container workdir must be absolute: {}",
                workdir.display()
            );
            command.arg("--workdir").arg(workdir);
        }
        for mount in mounts.into_values() {
            command.arg("--mount").arg(mount_arg(&mount)?);
        }

        if invocation.inherit_env {
            let env_names = std::env::vars_os()
                .filter_map(|(key, _)| key.into_string().ok())
                .filter(|key| valid_env_name(key))
                .collect::<BTreeSet<_>>();
            for key in env_names {
                command.arg("--env").arg(key);
            }
        }

        command
            .arg(&self.settings.image)
            .arg("run")
            .arg("--executor")
            .arg("host")
            .arg("--run-spec")
            .arg(format!("{GUEST_CONTROL_DIR}/{SPEC_FILENAME}"))
            .arg("--result-file")
            .arg(format!("{GUEST_CONTROL_DIR}/{RESULT_FILENAME}"))
            .stdin(stdio(invocation.stdin))
            .stdout(stdio(invocation.stdout))
            .stderr(stdio(invocation.stderr))
            .kill_on_drop(true);
        Ok(command)
    }

    async fn terminate(
        &self,
        child: &mut Child,
        container_name: &str,
        grace_ms: u64,
    ) -> Option<String> {
        let grace_seconds = grace_ms.div_ceil(1_000).max(1);
        let stop = tokio::time::timeout(
            Duration::from_millis(grace_ms.saturating_add(2_000)),
            Command::new(&self.settings.runtime)
                .arg("stop")
                .arg("--time")
                .arg(grace_seconds.to_string())
                .arg(container_name)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        )
        .await;
        if tokio::time::timeout(Duration::from_millis(2_000), child.wait())
            .await
            .is_ok()
        {
            return match stop {
                Ok(Ok(status)) if status.success() => None,
                Ok(Ok(_)) => Some("container stop reported failure".into()),
                Ok(Err(error)) => Some(format!("failed to execute container stop: {error}")),
                Err(_) => Some("container stop timed out".into()),
            };
        }
        let kill = Command::new(&self.settings.runtime)
            .arg("kill")
            .arg(container_name)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        let _ = child.kill().await;
        let _ = child.wait().await;
        match kill {
            Ok(status) if status.success() => None,
            Ok(_) => Some("container runtime could not kill the delegated pVisor".into()),
            Err(error) => Some(format!("failed to execute container kill: {error}")),
        }
    }
}

#[async_trait]
impl RunExecutor for ContainerExecutor {
    fn descriptor(&self) -> ExecutorDescriptor {
        ExecutorDescriptor {
            name: "docker-pvisor-v2".into(),
            kind: ExecutorKind::Container,
            isolation: IsolationKind::Container,
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
        let started_at = crate::util::unix_now_ms();
        context
            .transition(
                RunState::Starting,
                Some("injecting pVisor into OCI container".into()),
            )
            .await;

        let prepared = async {
            let platform = self.resolve_platform().await?;
            let binary = resolve_pvisor_binary(self.settings.pvisor_binary.as_deref())?;
            let files = DelegatedRunFiles::new(&spec)?;
            let command = self.build_command(
                &spec,
                context.attempt_id().as_str(),
                platform,
                &binary,
                &files,
            )?;
            Ok::<_, anyhow::Error>((files, command))
        }
        .await;
        let (files, mut command) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                return failed_to_start(&spec, context.attempt_id(), started_at, error.to_string());
            }
        };
        let name = container_name(spec.run_id.as_str(), context.attempt_id().as_str());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return failed_to_start(&spec, context.attempt_id(), started_at, error.to_string());
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
            Watchdog,
        }
        let cancellation = context.cancellation();
        let watchdog_ms = spec.runtime.timeout_ms.map(|timeout| {
            timeout
                .saturating_add(spec.runtime.termination_grace_ms)
                .saturating_add(10_000)
        });
        let end = if let Some(watchdog_ms) = watchdog_ms {
            tokio::select! {
                biased;
                status = child.wait() => End::Exited(status),
                _ = cancellation.cancelled() => End::Cancelled,
                _ = tokio::time::sleep(Duration::from_millis(watchdog_ms)) => End::Watchdog,
            }
        } else {
            tokio::select! {
                biased;
                status = child.wait() => End::Exited(status),
                _ = cancellation.cancelled() => End::Cancelled,
            }
        };

        let mut warnings = Vec::new();
        if matches!(end, End::Cancelled | End::Watchdog) {
            if matches!(end, End::Cancelled) {
                context
                    .transition(RunState::Cancelling, Some("cancellation requested".into()))
                    .await;
            }
            if let Some(warning) = self
                .terminate(&mut child, &name, spec.runtime.termination_grace_ms)
                .await
            {
                warnings.push(warning);
            }
        }

        let transport_stdout = join_capture(stdout_task).await;
        let transport_stderr = join_capture(stderr_task).await;
        if matches!(end, End::Exited(_)) && files.result_path.is_file() {
            match files.read_result(&spec.run_id, context.attempt_id(), spec.lease_epoch) {
                Ok(mut output) => {
                    context.import_delegated_agent_abi(output.agent_abi);
                    output.result.warnings.extend(warnings);
                    return output.result;
                }
                Err(error) => warnings.push(format!("decode delegated pVisor result: {error}")),
            }
        }

        let mut output = ProcessOutput::default();
        if let Some(captured) = transport_stdout {
            output.stdout = Some(captured.text);
            output.stdout_truncated = captured.truncated;
        }
        if let Some(captured) = transport_stderr {
            output.stderr = Some(captured.text);
            output.stderr_truncated = captured.truncated;
        }
        let (state, exit_code, failure) = match end {
            End::Cancelled => (RunState::Cancelled, None, None),
            End::Watchdog => (
                RunState::Failed,
                None,
                Some(RunFailure {
                    kind: RunFailureKind::DeadlineExceeded,
                    message: "delegated pVisor did not finish before the transport watchdog".into(),
                    retryable: false,
                }),
            ),
            End::Exited(Ok(status)) if status.code() == Some(125) => (
                RunState::Failed,
                status.code(),
                Some(RunFailure {
                    kind: RunFailureKind::Infrastructure,
                    message: "container runtime failed before injected pVisor started".into(),
                    retryable: false,
                }),
            ),
            End::Exited(Ok(status)) => (
                RunState::Failed,
                status.code(),
                Some(RunFailure {
                    kind: RunFailureKind::Infrastructure,
                    message: "injected pVisor exited without a valid RunResult".into(),
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
        };
        RunResult {
            run_id: spec.run_id,
            attempt_id: context.attempt_id().clone(),
            lease_epoch: spec.lease_epoch,
            state,
            started_at_unix_ms: started_at,
            finished_at_unix_ms: crate::util::unix_now_ms(),
            exit_code,
            failure,
            output,
            value: None,
            metrics: Default::default(),
            artifacts: Vec::new(),
            event_stream_ref: None,
            warnings,
        }
    }
}

fn failed_to_start(
    spec: &persisting_control::RunSpec,
    attempt_id: &persisting_control::AttemptId,
    started_at: u64,
    message: String,
) -> RunResult {
    RunResult {
        run_id: spec.run_id.clone(),
        attempt_id: attempt_id.clone(),
        lease_epoch: spec.lease_epoch,
        state: RunState::Failed,
        started_at_unix_ms: started_at,
        finished_at_unix_ms: crate::util::unix_now_ms(),
        exit_code: None,
        failure: Some(RunFailure {
            kind: RunFailureKind::Spawn,
            message,
            retryable: false,
        }),
        output: ProcessOutput::default(),
        value: None,
        metrics: Default::default(),
        artifacts: Vec::new(),
        event_stream_ref: None,
        warnings: Vec::new(),
    }
}

fn validate_mount(mount: &ContainerMount) -> anyhow::Result<()> {
    anyhow::ensure!(
        !mount.source.as_os_str().is_empty(),
        "container mount source must not be empty"
    );
    anyhow::ensure!(
        mount.target.is_absolute(),
        "container mount target must be absolute: {}",
        mount.target.display()
    );
    validate_mount_path(&mount.target)
}

fn bind_mount(source: &Path, target: &Path, read_only: bool) -> anyhow::Result<BindMount> {
    let source = source.canonicalize().map_err(|error| {
        anyhow::anyhow!("resolve container mount {}: {error}", source.display())
    })?;
    anyhow::ensure!(
        target.is_absolute(),
        "container mount target must be absolute"
    );
    validate_mount_path(&source)?;
    validate_mount_path(target)?;
    Ok(BindMount {
        source,
        target: target.to_path_buf(),
        read_only,
    })
}

fn add_mount(mounts: &mut BTreeMap<PathBuf, BindMount>, mount: BindMount) -> anyhow::Result<()> {
    if let Some(existing) = mounts.get(&mount.target) {
        anyhow::ensure!(
            existing == &mount,
            "conflicting container mounts for {}",
            mount.target.display()
        );
        return Ok(());
    }
    mounts.insert(mount.target.clone(), mount);
    Ok(())
}

fn mount_arg(mount: &BindMount) -> anyhow::Result<String> {
    let source = mount
        .source
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("container mount source is not UTF-8"))?;
    let target = mount
        .target
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("container mount target is not UTF-8"))?;
    let mut value = format!("type=bind,source={source},target={target}");
    if mount.read_only {
        value.push_str(",readonly");
    }
    Ok(value)
}

fn validate_mount_path(path: &Path) -> anyhow::Result<()> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("container mount path is not UTF-8"))?;
    anyhow::ensure!(
        !value.contains([',', '\n', '\r']),
        "container mount path contains an unsupported delimiter: {}",
        path.display()
    );
    Ok(())
}

fn absolute_container_path(path: &Path) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn valid_env_name(key: &str) -> bool {
    !key.is_empty() && !key.contains(['=', '\0'])
}

fn container_name(run_id: &str, attempt_id: &str) -> String {
    fn clean(value: &str) -> String {
        value
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                    ch
                } else {
                    '-'
                }
            })
            .collect()
    }
    let run = clean(run_id).chars().take(32).collect::<String>();
    let attempt = clean(attempt_id);
    let suffix = attempt
        .chars()
        .rev()
        .take(24)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("pvisor-{run}-{suffix}")
}

fn stdio(mode: StdioMode) -> Stdio {
    match mode {
        StdioMode::Inherit => Stdio::inherit(),
        StdioMode::Capture => Stdio::piped(),
        StdioMode::Null => Stdio::null(),
    }
}

async fn read_limited<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<Captured> {
    let mut retained = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let keep = limit.saturating_sub(retained.len()).min(read);
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok(Captured {
        text: String::from_utf8_lossy(&retained).into_owned(),
        truncated,
    })
}

async fn join_capture(
    task: Option<tokio::task::JoinHandle<std::io::Result<Captured>>>,
) -> Option<Captured> {
    match task {
        Some(task) => task.await.ok().and_then(Result::ok),
        None => None,
    }
}

#[cfg(unix)]
fn terminal_is_tty() -> bool {
    unsafe {
        libc::isatty(libc::STDIN_FILENO) == 1
            && libc::isatty(libc::STDOUT_FILENO) == 1
            && libc::isatty(libc::STDERR_FILENO) == 1
    }
}

#[cfg(not(unix))]
fn terminal_is_tty() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ContainerNetwork, ContainerPlatform};
    use persisting_control::ResourceLimits;
    use std::ffi::OsStr;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    fn executable(path: &Path) {
        std::fs::write(path, b"runtime").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_injects_pvisor_and_never_executes_agent_directly() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime = temporary.path().join("pvisor");
        executable(&runtime);
        let cwd = temporary.path().join("workspace");
        std::fs::create_dir(&cwd).unwrap();
        let executor = ContainerExecutor::new(ContainerSettings {
            image: "example/agent:latest".into(),
            pvisor_binary: Some(runtime.clone()),
            platform: Some(ContainerPlatform::LinuxAmd64),
            network: ContainerNetwork::None,
            ..ContainerSettings::default()
        })
        .unwrap();
        let mut spec = persisting_control::RunSpec::process("run-one", "agent", "secret-agent");
        spec.runtime.resource_limits = ResourceLimits {
            memory_bytes: Some(1_048_576),
            processes: Some(8),
            open_files: Some(32),
            ..ResourceLimits::default()
        };
        let RunInvocation::Process(invocation) = &mut spec.invocation;
        invocation.cwd = Some(cwd.display().to_string());
        invocation.inherit_env = false;
        let files = DelegatedRunFiles::new(&spec).unwrap();
        let command = executor
            .build_command(
                &spec,
                "attempt-one",
                ContainerPlatform::LinuxAmd64,
                &runtime,
                &files,
            )
            .unwrap();
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--entrypoint", GUEST_PVISOR]));
        assert!(args.windows(2).any(|pair| pair == ["--executor", "host"]));
        assert!(args.windows(2).any(|pair| pair == ["--memory", "1048576b"]));
        assert!(args.windows(2).any(|pair| pair == ["--pids-limit", "8"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--ulimit", "nofile=32:32"]));
        assert!(args.iter().any(|arg| arg.ends_with(SPEC_FILENAME)));
        assert!(!args.iter().any(|arg| arg == "secret-agent"));
    }

    #[test]
    fn descriptor_reports_container_without_overclaiming_enforcement() {
        let executor = ContainerExecutor::new(ContainerSettings {
            image: "example/agent:latest".into(),
            ..ContainerSettings::default()
        })
        .unwrap();
        let descriptor = executor.descriptor();
        assert_eq!(descriptor.name, "docker-pvisor-v2");
        assert_eq!(descriptor.kind, ExecutorKind::Container);
        assert_eq!(descriptor.isolation, IsolationKind::Container);
        assert!(!descriptor.enforces_capabilities);
    }

    #[test]
    fn rejects_custom_container_user() {
        assert!(ContainerExecutor::new(ContainerSettings {
            image: "agent".into(),
            user: Some("1000".into()),
            ..ContainerSettings::default()
        })
        .is_err());
    }

    #[test]
    fn runtime_name_is_path_safe_and_retains_attempt_entropy() {
        let name = container_name("run/unsafe", "attempt:1234567890");
        assert!(!name.contains('/'));
        assert!(!name.contains(':'));
        assert!(name.ends_with("1234567890"));
        assert_ne!(
            container_name("run", "attempt-one"),
            container_name("run", "attempt-two")
        );
        assert_ne!(OsStr::new(&name), OsStr::new(""));
    }
}

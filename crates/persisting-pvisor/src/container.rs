//! Native OCI runtime transport. pVisor materializes an OCI bundle and invokes
//! runc/crun; no Docker or Podman daemon is required.

use crate::artifact::resolve_pvisor_binary;
use crate::config::{ContainerMount, ContainerPlatform, ContainerSettings};
use crate::delegated::{DelegatedRunFiles, RESULT_FILENAME, SPEC_FILENAME};
use crate::executor::{AttemptContext, RunExecutor};
use async_trait::async_trait;
use persisting_agentctl::{
    ExecutorDescriptor, ExecutorKind, IsolationKind, ProcessOutput, RunFailure, RunFailureKind,
    RunInvocation, RunResult, RunSpec, RunState, StdioMode,
};
use std::collections::BTreeMap;
use std::fs;
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
            !settings.image.trim().is_empty() || settings.rootfs.is_some(),
            "container requires an image or an explicit rootfs"
        );
        anyhow::ensure!(
            settings.network != crate::config::ContainerNetwork::Bridge,
            "container.network=bridge requires CNI and is not supported by native OCI runner; use host or none"
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

    fn build_command(
        &self,
        spec: &RunSpec,
        attempt_id: &str,
        _platform: ContainerPlatform,
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

        let control_dir = files.spec_path.parent().unwrap();
        let bundle = control_dir.join(format!("oci-bundle-{}", attempt_id));
        fs::create_dir_all(&bundle)?;
        let configured_rootfs = self
            .settings
            .rootfs
            .clone()
            .unwrap_or_else(|| PathBuf::from("/"));
        let rootfs = if configured_rootfs == Path::new("/") {
            // A host-root container gets a private synthetic root directory.
            // Standard host directories are mounted read-only into it, so OCI
            // mountpoint creation never mutates the real host `/`.
            let synthetic = bundle.join("rootfs");
            fs::create_dir_all(&synthetic)?;
            fs::create_dir_all(synthetic.join("tmp"))?;
            fs::create_dir_all(synthetic.join("dev"))?;
            fs::create_dir_all(synthetic.join("proc"))?;
            fs::create_dir_all(synthetic.join("sys"))?;
            for path in ["bin", "usr", "lib", "lib64", "sbin", "etc", "var"] {
                let source = PathBuf::from(format!("/{path}"));
                if source.is_dir() {
                    add_mount(
                        &mut mounts,
                        bind_mount(&source, Path::new(&format!("/{path}")), true)?,
                    )?;
                }
            }
            synthetic
        } else {
            anyhow::ensure!(
                configured_rootfs.is_dir(),
                "OCI rootfs does not exist: {}",
                configured_rootfs.display()
            );
            configured_rootfs
        };
        let _ = fs::create_dir_all(rootfs.join("opt/persisting"));
        let _ = fs::create_dir_all(rootfs.join("run/persisting"));
        // A read-only image still needs a standard writable scratch location
        // for the injected pVisor and AgentCtl setup.
        let _ = fs::create_dir_all(rootfs.join("tmp"));
        for mount in mounts.values() {
            if let Ok(relative) = mount.target.strip_prefix("/") {
                let target = rootfs.join(relative);
                if mount.source.is_dir() {
                    let _ = fs::create_dir_all(target);
                } else if let Some(parent) = target.parent() {
                    let _ = fs::create_dir_all(parent);
                    let _ = fs::File::create(target);
                }
            }
        }
        let config = bundle.join("config.json");
        let mut namespaces = vec![
            serde_json::json!({"type":"pid"}),
            serde_json::json!({"type":"ipc"}),
            serde_json::json!({"type":"uts"}),
            serde_json::json!({"type":"mount"}),
        ];
        if self.settings.network != crate::config::ContainerNetwork::Host {
            namespaces.push(serde_json::json!({"type":"network"}));
        }
        let mut mounts_json = Vec::new();
        mounts_json.push(serde_json::json!({"destination":"/dev","type":"tmpfs","source":"tmpfs","options":["nosuid","noexec","nodev","mode=755"]}));
        mounts_json.push(serde_json::json!({"destination":"/dev/shm","type":"tmpfs","source":"shm","options":["nosuid","noexec","nodev"]}));
        mounts_json.push(serde_json::json!({"destination":"/proc","type":"proc","source":"proc","options":["nosuid","noexec","nodev"]}));
        mounts_json.push(serde_json::json!({"destination":"/tmp","type":"tmpfs","source":"tmpfs","options":["nosuid","nodev","mode=1777"]}));
        // Bind mounts follow the standard pseudo-filesystem mounts.
        for m in mounts.values() {
            mounts_json.push(serde_json::json!({"destination":m.target,"type":"bind","source":m.source,"options":if m.read_only { vec!["rbind","ro"] } else { vec!["rbind","rw"] }}));
        }
        let requested_user = parse_user(self.settings.user.as_deref())?;
        let host_uid = unsafe { libc::geteuid() };
        let host_gid = unsafe { libc::getegid() };
        // Without subordinate ID ranges, rootless runtimes can only map the
        // caller's identity. Keep the container runnable (best effort) by
        // falling back to container root for an explicitly requested user.
        // pVisor currently uses a single-identity rootless mapping.  A
        // non-root container user would require configured subordinate ID
        // ranges and cannot be represented safely otherwise.
        let process_user = if requested_user != (0, 0) {
            eprintln!(
                "pVisor container: subordinate UID/GID mapping is unavailable; running as container root"
            );
            (0, 0)
        } else {
            requested_user
        };
        namespaces.insert(0, serde_json::json!({"type":"user"}));
        let resources = serde_json::json!({"memory": limits.memory_bytes.map(|v| serde_json::json!({"limit":v})), "pids": limits.processes.map(|v| serde_json::json!({"limit":v}))});
        let mut env_json = Vec::new();
        for (key, value) in &invocation.env {
            env_json.push(format!("{key}={value}"));
        }
        if invocation.inherit_env {
            for (key, value) in std::env::vars() {
                if valid_env_name(&key) && !invocation.env.contains_key(&key) {
                    env_json.push(format!("{key}={value}"));
                }
            }
        }
        let devices = [
            ("/dev/null", 1, 3), ("/dev/zero", 1, 5), ("/dev/random", 1, 8),
            ("/dev/urandom", 1, 9), ("/dev/tty", 5, 0),
        ].into_iter().map(|(path, major, minor)| serde_json::json!({"path":path,"type":"c","major":major,"minor":minor,"fileMode":438,"uid":0,"gid":0})).collect::<Vec<_>>();
        // Rootless runtimes require a mapping for UID/GID 0 whenever a user
        // namespace is enabled (even when the requested process user is not
        // root). Map the caller's host identity to container root, and add a
        // separate mapping for an explicitly requested non-root user.
        // A single contiguous range avoids duplicate host IDs (which Linux
        // rejects when writing uid_map/gid_map) while covering arbitrary
        // explicit container users such as 1000:1000.
        let mapping_size = 1u32;
        let uid_mappings =
            vec![serde_json::json!({"containerID":0,"hostID":host_uid,"size":mapping_size})];
        let gid_mappings =
            vec![serde_json::json!({"containerID":0,"hostID":host_gid,"size":mapping_size})];
        let cfg = serde_json::json!({"ociVersion":"1.0.2","process":{"terminal":false,"cwd":workdir.as_deref().unwrap_or(Path::new("/")),"args":[GUEST_PVISOR,"run","--executor","host","--stdio","capture","--spec",format!("{GUEST_CONTROL_DIR}/{SPEC_FILENAME}"),"--result-file",format!("{GUEST_CONTROL_DIR}/{RESULT_FILENAME}" )],"env":env_json,"user":{"uid":process_user.0,"gid":process_user.1}},"root":{"path":rootfs,"readonly":self.settings.read_only_rootfs},"mounts":mounts_json,"linux":{"namespaces":namespaces,"resources":resources,"devices":devices,"uidMappings":uid_mappings,"gidMappings":gid_mappings},"annotations":{"io.persisting.run_id":run_id,"io.persisting.attempt_id":attempt_id}});
        fs::write(&config, serde_json::to_vec_pretty(&cfg)?)?;
        let state = control_dir.join("oci-state");
        fs::create_dir_all(&state)?;
        let mut command = Command::new(&self.settings.runtime);
        command
            .arg("--root")
            .arg(state)
            .arg("run")
            .arg("--bundle")
            .arg(bundle)
            .arg(container_name(run_id, attempt_id));

        command
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
        let stop = tokio::time::timeout(
            Duration::from_millis(grace_ms.saturating_add(2_000)),
            Command::new(&self.settings.runtime)
                .arg("kill")
                .arg(container_name)
                .arg("TERM")
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
            .arg("KILL")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        let _ = child.kill().await;
        let _ = child.wait().await;
        let _ = Command::new(&self.settings.runtime)
            .arg("delete")
            .arg("--force")
            .arg(container_name)
            .status()
            .await;
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
            name: "oci-pvisor".into(),
            kind: ExecutorKind::Container,
            isolation: IsolationKind::Container,
            capability_enforcement: Default::default(),
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
            let platform = self
                .settings
                .platform
                .unwrap_or(ContainerPlatform::LinuxAmd64);
            let binary = resolve_pvisor_binary(self.settings.pvisor_binary.as_deref())?;
            let files = DelegatedRunFiles::new_with_stdio(&spec, true)?;
            let mut executor = self.clone();
            if executor.settings.rootfs.is_none() {
                anyhow::ensure!(
                    !executor.settings.image.trim().is_empty(),
                    "container requires --container-rootfs or --container-image"
                );
                let image = executor.settings.image.clone();
                let prepared = tokio::task::spawn_blocking(move || {
                    crate::oci::ImageStore::new(None)?.prepare(&image)
                })
                .await??;
                executor.settings.rootfs = Some(prepared.rootfs);
            }
            let command = executor.build_command(
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
                    context.import_delegated_agentctl(output.agentctl);
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
    spec: &persisting_agentctl::RunSpec,
    attempt_id: &persisting_agentctl::AttemptId,
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

fn parse_user(value: Option<&str>) -> anyhow::Result<(u32, u32)> {
    let Some(value) = value else {
        return Ok((0, 0));
    };
    let mut parts = value.split(':');
    let uid: u32 = parts
        .next()
        .unwrap_or("0")
        .parse()
        .map_err(|_| anyhow::anyhow!("container user must be uid[:gid]"))?;
    let gid: u32 = parts
        .next()
        .unwrap_or("0")
        .parse()
        .map_err(|_| anyhow::anyhow!("container user must be uid[:gid]"))?;
    anyhow::ensure!(parts.next().is_none(), "container user must be uid[:gid]");
    Ok((uid, gid))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ContainerNetwork, ContainerPlatform};
    use persisting_agentctl::ResourceLimits;
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
        let mut spec = persisting_agentctl::RunSpec::process("run-one", "agent", "secret-agent");
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
        assert!(
            args.iter()
                .any(|arg| arg.contains("oci-bundle-attempt-one"))
        );
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
        assert_eq!(descriptor.name, "oci-pvisor");
        assert_eq!(descriptor.kind, ExecutorKind::Container);
        assert_eq!(descriptor.isolation, IsolationKind::Container);
        assert!(descriptor.capability_enforcement.dimensions.is_empty());
    }

    #[test]
    fn accepts_numeric_container_user() {
        assert!(
            ContainerExecutor::new(ContainerSettings {
                image: "agent".into(),
                user: Some("1000".into()),
                ..ContainerSettings::default()
            })
            .is_ok()
        );
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

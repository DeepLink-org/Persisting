//! libkrun VM process isolation over a pVisor-provided root OverlayFS.

use crate::config::VmSettings;
use crate::executor::{AttemptContext, RunExecutor};
use anyhow::Context as _;
use async_trait::async_trait;
use persisting_agentctl::{
    CapabilityDimension, CapabilityEnforcementEvidence, ExecutorDescriptor, ExecutorKind,
    IsolationKind, ProcessOutput, ResourceLimits, RunFailure, RunFailureKind, RunInvocation,
    RunResult, RunState, StdioMode,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

const RUNNER_SPEC_ENV: &str = "PERSISTING_KRUN_RUNNER_SPEC";
const WORKSPACE_TAG: &str = "pvisor-workspace";
const NETWORK_FD_ENV: &str = "PERSISTING_KRUN_NETWORK_FD";
const NETWORK_CHILD_FD: RawFd = 198;
const NET_FLAG_DHCP_CLIENT: u32 = 1 << 1;

#[derive(Debug, Clone)]
pub struct VmExecutor {
    settings: VmSettings,
}

#[derive(Debug)]
struct Captured {
    text: String,
    truncated: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct RunnerSpec {
    root: OverlayDeviceSpec,
    workspace: Option<OverlayDeviceSpec>,
    workspace_target: Option<PathBuf>,
    mount_helper: Option<PathBuf>,
    guest: GuestSpec,
    cpus: u8,
    memory_mib: u32,
    library_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OverlayDeviceSpec {
    lowers: Vec<PathBuf>,
    upper: PathBuf,
    work: Option<PathBuf>,
    #[serde(default)]
    preimages: Option<PathBuf>,
    #[serde(default)]
    excluded: Vec<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GuestSpec {
    program: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
}

impl VmExecutor {
    pub fn new(mut settings: VmSettings) -> anyhow::Result<Self> {
        anyhow::ensure!(settings.memory_mib > 0, "vm.memory_mib must be positive");
        anyhow::ensure!(settings.cpus > 0, "vm.cpus must be positive");
        anyhow::ensure!(settings.cpus <= 8, "libkrunfw supports at most 8 vCPUs");
        let rootfs = settings
            .rootfs
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("vm.rootfs must be configured"))?;
        anyhow::ensure!(
            rootfs.is_dir(),
            "vm.rootfs is not a directory: {}",
            rootfs.display()
        );
        if let Some(directory) = &settings.library_dir {
            anyhow::ensure!(
                directory.is_dir(),
                "vm.library_dir is not a directory: {}",
                directory.display()
            );
            anyhow::ensure!(
                directory.join(firmware_name()).is_file(),
                "vm.library_dir does not contain {}: {}",
                firmware_name(),
                directory.display()
            );
        } else if let Some(directory) = bundled_firmware_dir() {
            settings.library_dir = Some(directory);
        }
        Ok(Self { settings })
    }

    pub fn settings(&self) -> &VmSettings {
        &self.settings
    }
}

pub(crate) fn bundled_firmware_dir() -> Option<PathBuf> {
    let directory = std::env::current_exe().ok()?.parent()?.to_path_buf();
    directory
        .join(firmware_name())
        .is_file()
        .then_some(directory)
}

pub(crate) const fn firmware_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "libkrunfw.5.dylib"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "libkrunfw.so.5"
    }
}

#[async_trait]
impl RunExecutor for VmExecutor {
    fn descriptor(&self) -> ExecutorDescriptor {
        ExecutorDescriptor {
            name: "libkrun-root-overlay-v1".into(),
            kind: ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
            capability_enforcement: CapabilityEnforcementEvidence::default()
                .enforced(
                    CapabilityDimension::FilesystemRead,
                    "libkrun-guest-kernel-virtiofs-root",
                )
                .enforced(
                    CapabilityDimension::FilesystemWrite,
                    "libkrun-guest-kernel-virtiofs-overlay",
                ),
            supports_checkpoint: true,
            supports_migration: false,
        }
    }

    fn supports(&self, invocation: &RunInvocation) -> bool {
        matches!(invocation, RunInvocation::Process(_))
    }

    fn supports_vm_network_attachment(&self) -> bool {
        true
    }

    async fn execute(&self, context: AttemptContext) -> RunResult {
        let mut spec = context.spec().clone();
        let started_at = crate::util::unix_now_ms();
        let cancellation = context.cancellation();
        context
            .transition(
                RunState::Starting,
                Some("starting libkrun guest over pVisor root OverlayFS".into()),
            )
            .await;
        if !cfg!(any(
            target_os = "linux",
            all(target_os = "macos", target_arch = "aarch64")
        )) {
            return failed_to_start(
                &spec,
                context.attempt_id(),
                started_at,
                "libkrun execution requires Linux/KVM or Apple Silicon macOS/HVF".into(),
            );
        }

        let RunInvocation::Process(invocation) = &mut spec.invocation;
        let overlay_target = spec
            .metadata
            .get("pvisor.vm.overlay_target")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from);
        let root = self
            .settings
            .rootfs
            .clone()
            .expect("validated by VmExecutor::new");
        if !root.is_dir() {
            return failed_to_start(
                &spec,
                context.attempt_id(),
                started_at,
                format!("prepared root OverlayFS is not mounted: {}", root.display()),
            );
        }
        let guest_cwd = spec
            .metadata
            .get("pvisor.vm.guest_cwd")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let workspace = spec
            .metadata
            .get("pvisor.vm.workspace_overlay")
            .cloned()
            .map(serde_json::from_value::<OverlayDeviceSpec>)
            .transpose();
        let configured_overlay = match workspace {
            Ok(workspace) => workspace,
            Err(error) => {
                return failed_to_start(
                    &spec,
                    context.attempt_id(),
                    started_at,
                    format!("invalid libkrun workspace overlay metadata: {error}"),
                );
            }
        };
        let (root_overlay, workspace) = if overlay_target.is_none() {
            (
                configured_overlay.unwrap_or_else(|| OverlayDeviceSpec {
                    lowers: vec![root.clone()],
                    upper: PathBuf::new(),
                    work: None,
                    preimages: None,
                    excluded: Vec::new(),
                }),
                None,
            )
        } else {
            (
                OverlayDeviceSpec {
                    lowers: vec![root.clone()],
                    upper: PathBuf::new(),
                    work: None,
                    preimages: None,
                    excluded: Vec::new(),
                },
                configured_overlay,
            )
        };
        let workspace_target = workspace.as_ref().and(overlay_target.clone());
        let mut env = if invocation.inherit_env {
            std::env::vars().collect::<BTreeMap<_, _>>()
        } else {
            BTreeMap::new()
        };
        for key in [
            crate::AGENTCTL_ENDPOINT_ENV,
            crate::AGENTCTL_TOKEN_ENV,
            crate::AGENTCTL_TRANSPORT_ENV,
            crate::AGENTCTL_VERSION_ENV,
            crate::LEGACY_AGENT_ABI_ENDPOINT_ENV,
            crate::LEGACY_AGENT_ABI_TOKEN_ENV,
            crate::LEGACY_AGENT_ABI_TRANSPORT_ENV,
            crate::LEGACY_AGENT_ABI_VERSION_ENV,
        ] {
            env.remove(key);
        }
        for key in [
            "DYLD_LIBRARY_PATH",
            "DYLD_FALLBACK_LIBRARY_PATH",
            "LD_LIBRARY_PATH",
        ] {
            env.remove(key);
        }
        if !invocation.env.contains_key("PATH") {
            env.insert(
                "PATH".into(),
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
            );
        }
        if !invocation.env.contains_key("HOME") {
            env.insert("HOME".into(), "/root".into());
        }
        if !invocation.env.contains_key("TMPDIR") {
            env.insert("TMPDIR".into(), "/tmp".into());
        }
        env.extend(invocation.env.clone());

        let temporary = match tempfile::Builder::new().prefix("pvisor-krun-").tempdir() {
            Ok(value) => value,
            Err(error) => {
                return failed_to_start(&spec, context.attempt_id(), started_at, error.to_string());
            }
        };
        let executable = match std::env::current_exe() {
            Ok(path) if path.is_absolute() => path,
            Ok(path) => {
                return failed_to_start(
                    &spec,
                    context.attempt_id(),
                    started_at,
                    format!("pVisor executable is not absolute: {}", path.display()),
                );
            }
            Err(error) => {
                return failed_to_start(&spec, context.attempt_id(), started_at, error.to_string());
            }
        };
        let runner_root = root.clone();
        let root_upper = temporary.path().join("root-upper");
        let root_work = temporary.path().join("root-work");
        if let Err(error) =
            std::fs::create_dir_all(&root_upper).and_then(|()| std::fs::create_dir_all(&root_work))
        {
            return failed_to_start(
                &spec,
                context.attempt_id(),
                started_at,
                format!("prepare libkrun root overlay: {error}"),
            );
        }
        let mut root_overlay = if root_overlay.upper.as_os_str().is_empty() {
            OverlayDeviceSpec {
                lowers: root_overlay.lowers,
                upper: root_upper.clone(),
                work: Some(root_work.clone()),
                preimages: root_overlay.preimages,
                excluded: root_overlay.excluded,
            }
        } else {
            root_overlay
        };
        let vm_network_enabled = context
            .spec()
            .metadata
            .get("pvisor.network.driver")
            .and_then(serde_json::Value::as_str)
            == Some("vm-smoltcp");
        if vm_network_enabled {
            let network_lower = temporary.path().join("network-lower");
            let resolver = network_lower.join("etc/resolv.conf");
            if let Err(error) = std::fs::create_dir_all(resolver.parent().expect("resolver parent"))
                .and_then(|()| {
                    std::fs::write(
                        &resolver,
                        b"nameserver 192.0.2.1\noptions timeout:2 attempts:2\n",
                    )
                })
            {
                return failed_to_start(
                    &spec,
                    context.attempt_id(),
                    started_at,
                    format!("prepare VM synthetic resolver: {error}"),
                );
            }
            root_overlay.lowers.insert(0, network_lower);
        }
        if let Some(workspace) = &workspace {
            if workspace.lowers.is_empty()
                || workspace.lowers.iter().any(|lower| !lower.is_dir())
                || !workspace.upper.is_dir()
                || workspace.work.as_ref().is_some_and(|work| !work.is_dir())
            {
                return failed_to_start(
                    &spec,
                    context.attempt_id(),
                    started_at,
                    "libkrun workspace overlay contains a missing backing directory".into(),
                );
            }
            let target = workspace_target
                .as_deref()
                .expect("workspace is only configured with an overlay target");
            let mountpoint = match guest_path_in_root(&runner_root, target) {
                Ok(path) => path,
                Err(error) => {
                    return failed_to_start(
                        &spec,
                        context.attempt_id(),
                        started_at,
                        error.to_string(),
                    );
                }
            };
            match std::fs::symlink_metadata(&mountpoint) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return failed_to_start(
                        &spec,
                        context.attempt_id(),
                        started_at,
                        format!(
                            "guest overlay target must be a directory: {}",
                            target.display()
                        ),
                    );
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let upper_mountpoint =
                        guest_path_in_root(&root_upper, target).expect("validated guest target");
                    if let Err(error) = std::fs::create_dir_all(&upper_mountpoint) {
                        return failed_to_start(
                            &spec,
                            context.attempt_id(),
                            started_at,
                            format!("create guest overlay target: {error}"),
                        );
                    }
                }
                Err(error) => {
                    return failed_to_start(
                        &spec,
                        context.attempt_id(),
                        started_at,
                        error.to_string(),
                    );
                }
            }
        }
        let guest = GuestSpec {
            program: invocation.program.clone(),
            args: invocation.args.clone(),
            env,
            cwd: guest_cwd,
        };
        let mount_helper_guest = if workspace_target.is_some() {
            let source = match guest_mount_program(&root) {
                Some(path) => path,
                None => {
                    return failed_to_start(
                        &spec,
                        context.attempt_id(),
                        started_at,
                        format!(
                            "libkrun guest rootfs does not contain mount: {}",
                            root.display()
                        ),
                    );
                }
            };
            let name = format!(".pvisor-mount-{}", uuid::Uuid::new_v4().simple());
            let host = root_overlay.upper.join(&name);
            if let Err(error) = copy_guest_mount_program(&source, &host) {
                return failed_to_start(
                    &spec,
                    context.attempt_id(),
                    started_at,
                    format!("prepare guest mount helper: {error:#}"),
                );
            }
            Some(Path::new("/").join(name))
        } else {
            None
        };
        let helper_name = format!(".pvisor-exec-{}.sh", uuid::Uuid::new_v4().simple());
        let helper_host = root_overlay.upper.join(&helper_name);
        if let Err(error) = write_guest_helper(
            &helper_host,
            workspace_target.as_deref(),
            mount_helper_guest.as_deref(),
            &guest,
            &spec.runtime.resource_limits,
        ) {
            return failed_to_start(
                &spec,
                context.attempt_id(),
                started_at,
                format!("create guest execution helper: {error:#}"),
            );
        }
        let helper_guest = Path::new("/").join(helper_name);
        let requested_memory_mib = spec
            .runtime
            .resource_limits
            .memory_bytes
            .map(|bytes| bytes.div_ceil(1024 * 1024).max(1))
            .and_then(|mib| u32::try_from(mib).ok());
        let runner = RunnerSpec {
            root: root_overlay,
            workspace,
            workspace_target: workspace_target.clone(),
            mount_helper: Some(helper_guest),
            guest,
            cpus: self.settings.cpus as u8,
            memory_mib: requested_memory_mib
                .map(|requested| requested.min(self.settings.memory_mib))
                .unwrap_or(self.settings.memory_mib),
            library_dir: self.settings.library_dir.clone(),
        };
        let runner_path = temporary.path().join("runner.json");
        if let Err(error) = write_private_json(&runner_path, &runner) {
            return failed_to_start(&spec, context.attempt_id(), started_at, error.to_string());
        }

        let mut vm_network = match context.take_vm_network() {
            Ok(network) => network,
            Err(error) => {
                return failed_to_start(&spec, context.attempt_id(), started_at, error.to_string());
            }
        };
        if vm_network_enabled && vm_network.is_none() {
            return failed_to_start(
                &spec,
                context.attempt_id(),
                started_at,
                "pVisor VM network attachment is missing".into(),
            );
        }
        let mut command = Command::new(executable);
        command
            .env(RUNNER_SPEC_ENV, &runner_path)
            .stdin(stdio(invocation.stdin))
            .stdout(stdio(invocation.stdout))
            .stderr(stdio(invocation.stderr))
            .kill_on_drop(true);
        if let Some(network) = &vm_network {
            let source_fd = network.guest_stream().as_raw_fd();
            command.env(NETWORK_FD_ENV, NETWORK_CHILD_FD.to_string());
            // The socketpair has CLOEXEC. Duplicate it to one fixed inherited
            // descriptor after fork and before exec; the JSON runner spec never
            // contains a process-local FD number.
            unsafe {
                command.pre_exec(move || {
                    if libc::dup2(source_fd, NETWORK_CHILD_FD) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        // libkrun's x86_64 KVM path can otherwise race guest workqueue
        // creation and halt before init runs. The upstream compatibility
        // switch is still required on the Fedora 43 / Linux 6.17 host used by
        // pVisor's Linux validation, not only the older kernels named in the
        // vendored libkrun comment.
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        command.env("KRUN_ENOMEM_WORKAROUND", "1");
        if let Some(directory) = &self.settings.library_dir {
            #[cfg(target_os = "linux")]
            command.env("LD_LIBRARY_PATH", directory);
            #[cfg(target_os = "macos")]
            command.env("DYLD_LIBRARY_PATH", directory);
        }
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
        if matches!(end, End::Cancelled | End::Watchdog) {
            if matches!(end, End::Cancelled) {
                context
                    .transition(RunState::Cancelling, Some("cancellation requested".into()))
                    .await;
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        let transport_stdout = join_capture(stdout_task).await;
        let transport_stderr = join_capture(stderr_task).await;
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
                    message: "libkrun guest exceeded the transport watchdog".into(),
                    retryable: false,
                }),
            ),
            End::Exited(Ok(status)) if status.code().is_some() => {
                (RunState::Completed, status.code(), None)
            }
            End::Exited(Ok(status)) => (
                RunState::Failed,
                None,
                Some(RunFailure {
                    kind: RunFailureKind::Infrastructure,
                    message: format!("libkrun runner terminated by {status}"),
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
        let mut warnings = Vec::new();
        if let Some(network) = vm_network.take() {
            if let Err(error) = network.shutdown() {
                tracing::warn!(%error, "failed to stop VM smoltcp backend");
                warnings.push(format!("failed to stop VM smoltcp backend: {error:#}"));
            }
        }
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
            metrics: BTreeMap::from([(
                "resource.vm_memory_bytes".into(),
                f64::from(runner.memory_mib) * 1024.0 * 1024.0,
            )]),
            artifacts: Vec::new(),
            event_stream_ref: None,
            warnings,
        }
    }
}

/// Handle the self-exec libkrun runner.
/// Returns `true` when the current process was consumed by an internal mode.
pub fn run_internal_if_requested() -> anyhow::Result<bool> {
    if let Some(path) = std::env::var_os(RUNNER_SPEC_ENV) {
        let spec: RunnerSpec = serde_json::from_slice(&std::fs::read(&path)?)?;
        run_runner(spec)?;
        return Ok(true);
    }
    Ok(false)
}

fn run_runner(spec: RunnerSpec) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let mut read_only = spec.root.lowers.clone();
        let mut read_write = vec![spec.root.upper.clone()];
        read_write.extend(spec.root.work.iter().cloned());
        if let Some(workspace) = &spec.workspace {
            read_only.extend(workspace.lowers.iter().cloned());
            read_write.push(workspace.upper.clone());
            read_write.extend(workspace.work.iter().cloned());
            read_write.extend(workspace.preimages.iter().cloned());
        }
        crate::sandbox::restrict_krun_runner(read_only, read_write, spec.library_dir.clone())?;
    }
    run_linked_krun(spec)
}

fn run_linked_krun(spec: RunnerSpec) -> anyhow::Result<()> {
    if std::env::var_os("PERSISTING_KRUN_LOG").is_some() {
        check_krun(krun::krun_set_log_level(5), "krun_set_log_level")?;
    }
    let workspace_tag = CString::new(WORKSPACE_TAG)?;
    let helper = spec
        .mount_helper
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("libkrun guest execution helper is missing"))?;
    let program = path_cstring(helper)?;
    let workdir = CString::new("/")?;
    // libkrun 1.19 serializes argv and env through the kernel command line
    // without escaping embedded quotes. The helper contains the exact
    // invocation instead, so only its quote-free path crosses that boundary.
    let argv = Vec::<CString>::new();
    let mut argv_ptrs = argv.iter().map(|value| value.as_ptr()).collect::<Vec<_>>();
    argv_ptrs.push(std::ptr::null());
    let env = Vec::<CString>::new();
    let mut env_ptrs = env.iter().map(|value| value.as_ptr()).collect::<Vec<_>>();
    env_ptrs.push(std::ptr::null());

    let ctx = check_ctx(krun::krun_create_ctx(), "krun_create_ctx")?;
    check_krun(
        krun::krun_set_vm_config(ctx, spec.cpus, spec.memory_mib),
        "krun_set_vm_config",
    )?;
    add_krun_overlay(ctx, "/dev/root", &spec.root, 1 << 29)?;
    if let Some(workspace) = &spec.workspace {
        add_krun_overlay(ctx, workspace_tag.to_str()?, workspace, 0)?;
    }
    if let Some(fd) = std::env::var_os(NETWORK_FD_ENV) {
        let fd = fd
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid {NETWORK_FD_ENV}"))?
            .parse::<RawFd>()
            .with_context(|| format!("parse {NETWORK_FD_ENV}"))?;
        check_krun(
            unsafe {
                krun::krun_add_net_unixstream(
                    ctx,
                    std::ptr::null(),
                    fd,
                    persisting_overlaynet::vm::VM_MAC.as_ptr(),
                    0,
                    NET_FLAG_DHCP_CLIENT,
                )
            },
            "krun_add_net_unixstream",
        )?;
    }
    // Contexts start with an implicit vsock whose heuristic enables TSI when
    // there is no virtio-net device. Replace it with an explicit zero-feature
    // device so ordinary guest sockets cannot escape through the host stack.
    check_krun(
        krun::krun_disable_implicit_vsock(ctx),
        "krun_disable_implicit_vsock",
    )?;
    check_krun(krun::krun_add_vsock(ctx, 0), "krun_add_vsock")?;
    check_krun(
        unsafe { krun::krun_set_workdir(ctx, workdir.as_ptr()) },
        "krun_set_workdir",
    )?;
    check_krun(
        unsafe {
            krun::krun_set_exec(ctx, program.as_ptr(), argv_ptrs.as_ptr(), env_ptrs.as_ptr())
        },
        "krun_set_exec",
    )?;
    let started = krun::krun_start_enter(ctx);
    #[cfg(target_os = "macos")]
    if started == -libc::EINVAL {
        anyhow::bail!(
            "krun_start_enter failed with errno 22; source-built macOS binaries must be signed \
             with crates/persisting-pvisor/macos-hypervisor.entitlements"
        );
    }
    check_krun(started, "krun_start_enter")?;
    Ok(())
}

fn add_krun_overlay(
    ctx: u32,
    tag: &str,
    overlay: &OverlayDeviceSpec,
    shm_size: u64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !overlay.lowers.is_empty(),
        "libkrun overlay requires a lower directory"
    );
    let tag = CString::new(tag)?;
    let lowers = overlay
        .lowers
        .iter()
        .map(|path| path_cstring(path))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let lower_ptrs = lowers.iter().map(|path| path.as_ptr()).collect::<Vec<_>>();
    let upper = path_cstring(&overlay.upper)?;
    let work = overlay.work.as_deref().map(path_cstring).transpose()?;
    let preimages = overlay.preimages.as_deref().map(path_cstring).transpose()?;
    let excluded = overlay
        .excluded
        .iter()
        .map(|path| path_cstring(path))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let excluded_ptrs = excluded
        .iter()
        .map(|path| path.as_ptr())
        .collect::<Vec<_>>();
    check_krun(
        unsafe {
            krun::krun_add_virtiofs_overlay(
                ctx,
                tag.as_ptr(),
                lower_ptrs.as_ptr(),
                lower_ptrs.len(),
                upper.as_ptr(),
                work.as_ref().map_or(std::ptr::null(), |path| path.as_ptr()),
                preimages
                    .as_ref()
                    .map_or(std::ptr::null(), |path| path.as_ptr()),
                excluded_ptrs.as_ptr(),
                excluded_ptrs.len(),
                shm_size,
            )
        },
        "krun_add_virtiofs_overlay",
    )
}

fn write_guest_helper(
    path: &Path,
    workspace_target: Option<&Path>,
    mount_helper: Option<&Path>,
    guest: &GuestSpec,
    limits: &ResourceLimits,
) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut script = String::from("#!/bin/sh\nset -eu\n");
    if let Some(target) = workspace_target {
        let mount_helper =
            mount_helper.ok_or_else(|| anyhow::anyhow!("workspace mount helper is missing"))?;
        script.push_str(&shell_quote(&mount_helper.to_string_lossy())?);
        script.push_str(" -t virtiofs pvisor-workspace ");
        script.push_str(&shell_quote(&target.to_string_lossy())?);
        script.push('\n');
    }
    if let Some(bytes) = limits.memory_bytes {
        script.push_str(&format!("ulimit -v {}\n", bytes.div_ceil(1024)));
    }
    if let Some(processes) = limits.processes {
        script.push_str(&format!("ulimit -u {processes}\n"));
    }
    if let Some(milliseconds) = limits.cpu_time_ms {
        script.push_str(&format!("ulimit -t {}\n", milliseconds.div_ceil(1_000)));
    }
    if let Some(open_files) = limits.open_files {
        script.push_str(&format!("ulimit -n {open_files}\n"));
    }
    if let Some(bytes) = limits.file_size_bytes {
        script.push_str(&format!("ulimit -f {}\n", bytes.div_ceil(512)));
    }
    script.push_str("rm -f /init.krun \"$0\"");
    if let Some(mount_helper) = mount_helper {
        script.push(' ');
        script.push_str(&shell_quote(&mount_helper.to_string_lossy())?);
    }
    script.push_str("\ncd ");
    script.push_str(&shell_quote(&guest.cwd.to_string_lossy())?);
    script.push_str("\nexec env -i");
    for (key, value) in &guest.env {
        script.push(' ');
        script.push_str(&shell_quote(&format!("{key}={value}"))?);
    }
    script.push(' ');
    script.push_str(&shell_quote(&guest.program)?);
    for argument in &guest.args {
        script.push(' ');
        script.push_str(&shell_quote(argument)?);
    }
    script.push('\n');
    std::fs::write(path, script)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn guest_mount_program(root: &Path) -> Option<PathBuf> {
    ["bin/mount", "usr/bin/mount", "sbin/mount", "usr/sbin/mount"]
        .into_iter()
        .map(|relative| root.join(relative))
        .find(|path| path.is_file())
}

fn copy_guest_mount_program(source: &Path, destination: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::copy(source, destination)?;
    // Host distributions commonly install mount setuid-root. The rootless
    // passthrough cannot preserve its owner, so executing that file would
    // switch guest root to the mapped host uid. A private non-setuid copy
    // keeps the already-root guest credentials and can perform the mount.
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn shell_quote(value: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !value.as_bytes().contains(&0),
        "guest command and environment cannot contain NUL bytes"
    );
    Ok(format!("'{}'", value.replace('\'', "'\"'\"'")))
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

fn write_private_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, serde_json::to_vec(value)?)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
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

fn path_cstring(path: &Path) -> anyhow::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    Ok(CString::new(path.as_os_str().as_bytes())?)
}

fn guest_path_in_root(root: &Path, target: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        target.is_absolute() && target != Path::new("/"),
        "libkrun guest overlay target must be an absolute path other than /"
    );
    anyhow::ensure!(
        !target
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir)),
        "libkrun guest overlay target must not contain .."
    );
    let relative = target.strip_prefix(Path::new("/"))?;
    let mut resolved = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            continue;
        };
        resolved.push(component);
        match std::fs::symlink_metadata(&resolved) {
            Ok(metadata) => anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "libkrun guest overlay target traverses a symlink: {}",
                target.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(resolved)
}

fn check_ctx(value: i32, operation: &str) -> anyhow::Result<u32> {
    if value < 0 {
        anyhow::bail!("{operation} failed with errno {}", -value);
    }
    Ok(value as u32)
}

fn check_krun(value: i32, operation: &str) -> anyhow::Result<()> {
    if value < 0 {
        anyhow::bail!("{operation} failed with errno {}", -value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_validate_resource_limits() {
        let rootfs = tempfile::tempdir().unwrap();
        let settings = VmSettings {
            rootfs: Some(rootfs.path().to_path_buf()),
            ..VmSettings::default()
        };
        assert!(VmExecutor::new(settings.clone()).is_ok());
        assert!(VmExecutor::new(VmSettings::default()).is_err());
        assert!(VmExecutor::new(VmSettings {
            cpus: 9,
            ..settings
        })
        .is_err());
    }

    #[test]
    fn guest_helper_preserves_quoted_arguments_and_environment() {
        let temporary = tempfile::tempdir().unwrap();
        let helper = temporary.path().join("guest-helper.sh");
        let environment_value = "space ' single \" double\nnewline";
        let argument_value = "argument ' with \" quotes";
        let guest = GuestSpec {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf '%s\\n%s' \"$COMPLEX\" \"$0\" > result".into(),
                argument_value.into(),
            ],
            env: BTreeMap::from([("COMPLEX".into(), environment_value.into())]),
            cwd: temporary.path().to_path_buf(),
        };
        write_guest_helper(&helper, None, None, &guest, &ResourceLimits::default()).unwrap();

        let status = std::process::Command::new(&helper).status().unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(temporary.path().join("result")).unwrap(),
            format!("{environment_value}\n{argument_value}")
        );
    }

    #[test]
    fn guest_helper_emits_requested_resource_limits() {
        let temporary = tempfile::tempdir().unwrap();
        let helper = temporary.path().join("guest-helper.sh");
        let guest = GuestSpec {
            program: "/bin/true".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: PathBuf::from("/"),
        };
        write_guest_helper(
            &helper,
            None,
            None,
            &guest,
            &ResourceLimits {
                memory_bytes: Some(2 * 1024 * 1024),
                processes: Some(8),
                cpu_time_ms: Some(1_500),
                open_files: Some(32),
                file_size_bytes: Some(1024),
            },
        )
        .unwrap();
        let script = std::fs::read_to_string(helper).unwrap();
        assert!(script.contains("ulimit -v 2048"));
        assert!(script.contains("ulimit -u 8"));
        assert!(script.contains("ulimit -t 2"));
        assert!(script.contains("ulimit -n 32"));
        assert!(script.contains("ulimit -f 2"));
    }

    #[test]
    fn guest_mount_copy_strips_privilege_bits() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("mount");
        let destination = temporary.path().join("mount-helper");
        std::fs::write(&source, b"mount").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o4755)).unwrap();

        copy_guest_mount_program(&source, &destination).unwrap();

        assert_eq!(
            std::fs::metadata(destination).unwrap().mode() & 0o7777,
            0o700
        );
    }
}

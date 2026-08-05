//! QEMU/KVM transport that boots a Linux guest, copies in the matching static
//! pVisor over SSH, and executes the prepared Run through ProcessExecutor.

use crate::artifact::resolve_pvisor_binary;
use crate::config::{KvmArchitecture, KvmSettings};
use crate::delegated::{DelegatedRunFiles, RESULT_FILENAME, SPEC_FILENAME};
use crate::executor::{AttemptContext, RunExecutor};
use async_trait::async_trait;
use persisting_control::{
    ExecutorDescriptor, ExecutorKind, IsolationKind, ProcessOutput, RunFailure, RunFailureKind,
    RunInvocation, RunResult, RunState, StdioMode,
};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

const REMOTE_DIR: &str = "/run/persisting";
const REMOTE_PVISOR: &str = "/run/persisting/pvisor";
const WORKSPACE_TAG: &str = "persisting-workspace";
const REMOTE_WORKSPACE: &str = "/run/persisting/workspace";
const CAPTURE_CONFIG_ENV: &str = "PERSISTING_CAPTURE_CONFIG";

#[derive(Debug, Clone)]
pub struct KvmExecutor {
    settings: KvmSettings,
}

#[derive(Debug)]
struct Captured {
    text: String,
    truncated: bool,
}

impl KvmExecutor {
    pub fn new(mut settings: KvmSettings) -> anyhow::Result<Self> {
        let image = settings
            .image
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("kvm.image is required"))?;
        anyhow::ensure!(
            image.is_file(),
            "KVM image is not a file: {}",
            image.display()
        );
        anyhow::ensure!(settings.memory_mib > 0, "kvm.memory_mib must be positive");
        anyhow::ensure!(settings.cpus > 0, "kvm.cpus must be positive");
        anyhow::ensure!(
            !settings.ssh_user.trim().is_empty(),
            "kvm.ssh_user must not be empty"
        );
        let key = settings
            .ssh_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("kvm.ssh_key is required"))?;
        anyhow::ensure!(
            key.is_file(),
            "KVM SSH key is not a file: {}",
            key.display()
        );
        validate_qemu_path(image)?;
        if let Some(firmware) = &settings.firmware {
            anyhow::ensure!(
                firmware.is_file(),
                "KVM firmware is not a file: {}",
                firmware.display()
            );
            validate_qemu_path(firmware)?;
        }
        anyhow::ensure!(
            settings.architecture != KvmArchitecture::Aarch64 || settings.firmware.is_some(),
            "kvm.firmware is required for the QEMU aarch64 virt machine"
        );
        if settings.architecture == KvmArchitecture::Aarch64
            && settings.qemu == Path::new("qemu-system-x86_64")
        {
            settings.qemu = "qemu-system-aarch64".into();
        }
        Ok(Self { settings })
    }

    pub fn settings(&self) -> &KvmSettings {
        &self.settings
    }

    fn build_qemu_command(
        &self,
        ssh_port: u16,
        shared_cwd: Option<&Path>,
    ) -> anyhow::Result<Command> {
        let image = self.settings.image.as_deref().expect("validated image");
        let mut command = Command::new(&self.settings.qemu);
        command
            .arg("-enable-kvm")
            .arg("-machine")
            .arg(match self.settings.architecture {
                KvmArchitecture::X86_64 => "q35,accel=kvm",
                KvmArchitecture::Aarch64 => "virt,accel=kvm",
            })
            .arg("-cpu")
            .arg("host")
            .arg("-m")
            .arg(self.settings.memory_mib.to_string())
            .arg("-smp")
            .arg(self.settings.cpus.to_string())
            .arg("-drive")
            .arg(format!(
                "file={},if=virtio,format={}",
                image.display(),
                self.settings.image_format.as_qemu_value()
            ))
            .arg("-netdev")
            .arg(format!(
                "user,id=persisting-net,hostfwd=tcp:127.0.0.1:{ssh_port}-:22"
            ))
            .arg("-device")
            .arg("virtio-net-pci,netdev=persisting-net")
            .arg("-display")
            .arg("none")
            .arg("-serial")
            .arg("none")
            .arg("-monitor")
            .arg("none");
        if self.settings.snapshot {
            command.arg("-snapshot");
        }
        if let Some(firmware) = &self.settings.firmware {
            command.arg("-bios").arg(firmware);
        }
        if let Some(cwd) = shared_cwd {
            validate_qemu_path(cwd)?;
            command
                .arg("-fsdev")
                .arg(format!(
                    "local,id=persisting-fs,path={},security_model=none",
                    cwd.display()
                ))
                .arg("-device")
                .arg(format!(
                    "virtio-9p-pci,fsdev=persisting-fs,mount_tag={WORKSPACE_TAG}"
                ));
        }
        command
            .args(&self.settings.extra_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        Ok(command)
    }

    fn ssh_command(&self, port: u16) -> Command {
        let mut command = Command::new(&self.settings.ssh);
        self.add_ssh_options(&mut command, port);
        command.arg(self.ssh_destination());
        command
    }

    fn scp_command(&self, port: u16) -> Command {
        let mut command = Command::new(&self.settings.scp);
        command
            .arg("-P")
            .arg(port.to_string())
            .arg("-i")
            .arg(self.settings.ssh_key.as_deref().expect("validated key"))
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("StrictHostKeyChecking=no")
            .arg("-o")
            .arg("UserKnownHostsFile=/dev/null");
        command
    }

    fn add_ssh_options(&self, command: &mut Command, port: u16) {
        command
            .arg("-p")
            .arg(port.to_string())
            .arg("-i")
            .arg(self.settings.ssh_key.as_deref().expect("validated key"))
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("StrictHostKeyChecking=no")
            .arg("-o")
            .arg("UserKnownHostsFile=/dev/null")
            .arg("-o")
            .arg("ConnectTimeout=2");
    }

    fn ssh_destination(&self) -> String {
        format!("{}@127.0.0.1", self.settings.ssh_user)
    }

    async fn wait_for_ssh(&self, qemu: &mut Child, port: u16) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_millis(self.settings.boot_timeout_ms);
        loop {
            if let Some(status) = qemu.try_wait()? {
                anyhow::bail!("QEMU exited before SSH became ready: {status}");
            }
            let status = self
                .ssh_command(port)
                .arg("true")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
            if status.is_ok_and(|status| status.success()) {
                return Ok(());
            }
            anyhow::ensure!(Instant::now() < deadline, "KVM guest SSH boot timeout");
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn copy_to_guest(&self, port: u16, source: &Path, target: &str) -> anyhow::Result<()> {
        let status = self
            .scp_command(port)
            .arg(source)
            .arg(format!("{}:{target}", self.ssh_destination()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
        anyhow::ensure!(
            status.success(),
            "copy {} into KVM guest failed",
            source.display()
        );
        Ok(())
    }

    async fn copy_from_guest(&self, port: u16, source: &str, target: &Path) -> anyhow::Result<()> {
        let status = self
            .scp_command(port)
            .arg(format!("{}:{source}", self.ssh_destination()))
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
        anyhow::ensure!(
            status.success(),
            "copy delegated RunResult from KVM guest failed"
        );
        Ok(())
    }

    async fn stop_vm(&self, qemu: &mut Child) {
        let _ = qemu.kill().await;
        let _ = qemu.wait().await;
    }
}

#[async_trait]
impl RunExecutor for KvmExecutor {
    fn descriptor(&self) -> ExecutorDescriptor {
        ExecutorDescriptor {
            name: "qemu-kvm-pvisor-v1".into(),
            kind: ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
            enforces_capabilities: false,
            supports_checkpoint: false,
            supports_migration: false,
        }
    }

    fn supports(&self, invocation: &RunInvocation) -> bool {
        matches!(invocation, RunInvocation::Process(_))
    }

    async fn execute(&self, context: AttemptContext) -> RunResult {
        let mut spec = context.spec().clone();
        let started_at = crate::util::unix_now_ms();
        context
            .transition(
                RunState::Starting,
                Some("booting KVM guest for injected pVisor".into()),
            )
            .await;
        if !cfg!(target_os = "linux") {
            return failed_to_start(
                &spec,
                context.attempt_id(),
                started_at,
                "KVM execution requires a Linux host".into(),
            );
        }

        let RunInvocation::Process(process) = &mut spec.invocation;
        let shared_cwd = process
            .cwd
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.exists());
        if shared_cwd.is_some() {
            process.cwd = Some(REMOTE_WORKSPACE.into());
        }
        let capture_source = process
            .env
            .get(CAPTURE_CONFIG_ENV)
            .map(PathBuf::from)
            .filter(|path| path.is_file());
        if capture_source.is_some() {
            process.env.insert(
                CAPTURE_CONFIG_ENV.into(),
                format!("{REMOTE_DIR}/capture-config.toml"),
            );
        }

        let prepared = (|| {
            let binary = resolve_pvisor_binary(
                self.settings.pvisor_binary.as_deref(),
                self.settings.architecture.platform(),
            )?;
            let files = DelegatedRunFiles::new(&spec)?;
            let port = match self.settings.ssh_port {
                Some(port) => port,
                None => reserve_loopback_port()?,
            };
            let qemu = self.build_qemu_command(port, shared_cwd.as_deref())?;
            Ok::<_, anyhow::Error>((binary, files, port, qemu))
        })();
        let (binary, files, port, mut qemu_command) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                return failed_to_start(&spec, context.attempt_id(), started_at, error.to_string());
            }
        };
        let mut qemu = match qemu_command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return failed_to_start(&spec, context.attempt_id(), started_at, error.to_string());
            }
        };
        let bootstrap = async {
            self.wait_for_ssh(&mut qemu, port).await?;
            let status = self
                .ssh_command(port)
                .arg(format!("mkdir -p {REMOTE_DIR}"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await?;
            anyhow::ensure!(status.success(), "create pVisor directory in KVM guest failed");
            if shared_cwd.is_some() {
                let mount = self
                    .ssh_command(port)
                    .arg(format!(
                        "mkdir -p {REMOTE_WORKSPACE} && mount -t 9p -o trans=virtio,version=9p2000.L {WORKSPACE_TAG} {REMOTE_WORKSPACE}"
                    ))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await?;
                anyhow::ensure!(mount.success(), "mount shared Run cwd in KVM guest failed");
            }
            self.copy_to_guest(port, &binary, REMOTE_PVISOR).await?;
            self.copy_to_guest(
                port,
                &files.spec_path,
                &format!("{REMOTE_DIR}/{SPEC_FILENAME}"),
            )
            .await?;
            if let Some(source) = &capture_source {
                self.copy_to_guest(port, source, &format!("{REMOTE_DIR}/capture-config.toml"))
                    .await?;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = bootstrap {
            self.stop_vm(&mut qemu).await;
            return failed_to_start(&spec, context.attempt_id(), started_at, error.to_string());
        }

        let RunInvocation::Process(invocation) = &spec.invocation;
        let mut remote = self.ssh_command(port);
        remote
            .arg(format!(
                "chmod 0755 {REMOTE_PVISOR} && exec {REMOTE_PVISOR} run --executor host --run-spec {REMOTE_DIR}/{SPEC_FILENAME} --result-file {REMOTE_DIR}/{RESULT_FILENAME}"
            ))
            .stdin(stdio(invocation.stdin))
            .stdout(stdio(invocation.stdout))
            .stderr(stdio(invocation.stderr))
            .kill_on_drop(true);
        let mut child = match remote.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.stop_vm(&mut qemu).await;
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
        if matches!(end, End::Cancelled) {
            context
                .transition(RunState::Cancelling, Some("cancellation requested".into()))
                .await;
        }
        if matches!(end, End::Cancelled | End::Watchdog) {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        let transport_stdout = join_capture(stdout_task).await;
        let transport_stderr = join_capture(stderr_task).await;

        if matches!(end, End::Exited(_)) {
            let copied = self
                .copy_from_guest(
                    port,
                    &format!("{REMOTE_DIR}/{RESULT_FILENAME}"),
                    &files.result_path,
                )
                .await;
            if copied.is_ok() {
                if let Ok(output) =
                    files.read_result(&spec.run_id, context.attempt_id(), spec.lease_epoch)
                {
                    context.import_delegated_agent_abi(output.agent_abi);
                    self.stop_vm(&mut qemu).await;
                    return output.result;
                }
            }
        }
        self.stop_vm(&mut qemu).await;

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
                    message: "KVM delegated pVisor exceeded the transport watchdog".into(),
                    retryable: false,
                }),
            ),
            End::Exited(status) => match status {
                Ok(status) => (
                    RunState::Failed,
                    status.code(),
                    Some(RunFailure {
                        kind: RunFailureKind::Infrastructure,
                        message: "KVM delegated pVisor exited without a valid RunResult".into(),
                        retryable: false,
                    }),
                ),
                Err(error) => (
                    RunState::Failed,
                    None,
                    Some(RunFailure {
                        kind: RunFailureKind::Infrastructure,
                        message: error.to_string(),
                        retryable: true,
                    }),
                ),
            },
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
            warnings: Vec::new(),
        }
    }
}

fn validate_qemu_path(path: &Path) -> anyhow::Result<()> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("QEMU path is not UTF-8: {}", path.display()))?;
    anyhow::ensure!(
        !value.contains([',', '\n', '\r']),
        "QEMU path contains an unsupported delimiter: {}",
        path.display()
    );
    Ok(())
}

fn reserve_loopback_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fixture_settings(temporary: &tempfile::TempDir) -> KvmSettings {
        use std::os::unix::fs::PermissionsExt;
        let image = temporary.path().join("guest.qcow2");
        let key = temporary.path().join("id_ed25519");
        let pvisor = temporary.path().join("pvisor");
        std::fs::write(&image, b"image").unwrap();
        std::fs::write(&key, b"key").unwrap();
        std::fs::write(&pvisor, b"runtime").unwrap();
        std::fs::set_permissions(&pvisor, std::fs::Permissions::from_mode(0o755)).unwrap();
        KvmSettings {
            image: Some(image),
            ssh_key: Some(key),
            pvisor_binary: Some(pvisor),
            ..KvmSettings::default()
        }
    }

    #[cfg(unix)]
    #[test]
    fn qemu_command_enables_kvm_ssh_forwarding_and_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = KvmExecutor::new(fixture_settings(&temporary)).unwrap();
        let command = executor.build_qemu_command(22022, None).unwrap();
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.iter().any(|arg| arg == "-enable-kvm"));
        assert!(args
            .iter()
            .any(|arg| arg.contains("hostfwd=tcp:127.0.0.1:22022-:22")));
        assert!(args.iter().any(|arg| arg == "-snapshot"));
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_reports_virtual_machine_isolation() {
        let temporary = tempfile::tempdir().unwrap();
        let executor = KvmExecutor::new(fixture_settings(&temporary)).unwrap();
        let descriptor = executor.descriptor();
        assert_eq!(descriptor.kind, ExecutorKind::VirtualMachine);
        assert_eq!(descriptor.isolation, IsolationKind::VirtualMachine);
        assert!(!descriptor.enforces_capabilities);
    }
}

//! libkrun/KVM process isolation over a pVisor-provided root OverlayFS.

use crate::config::KvmSettings;
use crate::executor::{AttemptContext, RunExecutor};
use async_trait::async_trait;
use persisting_control::{
    ExecutorDescriptor, ExecutorKind, IsolationKind, ProcessOutput, RunFailure, RunFailureKind,
    RunInvocation, RunResult, RunState, StdioMode,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::ffi::{c_char, c_void, CString};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

const RUNNER_SPEC_ENV: &str = "PERSISTING_KRUN_RUNNER_SPEC";
const GUEST_SPEC_ENV: &str = "PERSISTING_KRUN_GUEST_SPEC";
const GUEST_INTERNAL_ARG: &str = "__pvisor-krun-guest";
#[cfg(target_os = "linux")]
const ROOT_TAG: &str = "/dev/root";
#[cfg(target_os = "linux")]
const VMADDR_CID_HOST: u32 = 2;

#[derive(Debug, Clone)]
pub struct KvmExecutor {
    settings: KvmSettings,
}

#[derive(Debug)]
struct Captured {
    text: String,
    truncated: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct RunnerSpec {
    root: PathBuf,
    guest_executable: PathBuf,
    guest: GuestSpec,
    cpus: u8,
    memory_mib: u32,
    library_dir: Option<PathBuf>,
    vsock_mappings: Vec<VsockMapping>,
}

#[derive(Debug, Serialize, Deserialize)]
struct VsockMapping {
    port: u32,
    host_socket: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct GuestSpec {
    program: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
    uid: u32,
    gid: u32,
    additional_gids: Vec<u32>,
    proxy: Option<TcpVsockBridge>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TcpVsockBridge {
    listen: SocketAddr,
    port: u32,
}

impl KvmExecutor {
    pub fn new(settings: KvmSettings) -> anyhow::Result<Self> {
        anyhow::ensure!(settings.memory_mib > 0, "kvm.memory_mib must be positive");
        anyhow::ensure!(settings.cpus > 0, "kvm.cpus must be positive");
        anyhow::ensure!(settings.cpus <= 8, "libkrunfw supports at most 8 vCPUs");
        anyhow::ensure!(
            settings.proxy_vsock_port > 1024,
            "kvm.proxy_vsock_port must be greater than 1024"
        );
        if let Some(directory) = &settings.library_dir {
            anyhow::ensure!(
                directory.is_dir(),
                "kvm.library_dir is not a directory: {}",
                directory.display()
            );
        }
        Ok(Self { settings })
    }

    pub fn settings(&self) -> &KvmSettings {
        &self.settings
    }
}

#[async_trait]
impl RunExecutor for KvmExecutor {
    fn descriptor(&self) -> ExecutorDescriptor {
        ExecutorDescriptor {
            name: "libkrun-root-overlay-v1".into(),
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
                Some("starting libkrun guest over pVisor root OverlayFS".into()),
            )
            .await;
        if !cfg!(target_os = "linux") {
            return failed_to_start(
                &spec,
                context.attempt_id(),
                started_at,
                "libkrun host-root execution requires Linux/KVM".into(),
            );
        }

        let RunInvocation::Process(invocation) = &mut spec.invocation;
        let Some(root) = invocation.cwd.as_deref().map(PathBuf::from) else {
            return failed_to_start(
                &spec,
                context.attempt_id(),
                started_at,
                "libkrun executor requires the prepared root OverlayFS mount".into(),
            );
        };
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
            .get("pvisor.workspace")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut env = if invocation.inherit_env {
            std::env::vars().collect::<BTreeMap<_, _>>()
        } else {
            BTreeMap::new()
        };
        env.extend(invocation.env.clone());

        let temporary = match tempfile::Builder::new().prefix("pvisor-krun-").tempdir() {
            Ok(value) => value,
            Err(error) => {
                return failed_to_start(&spec, context.attempt_id(), started_at, error.to_string());
            }
        };
        let mut mappings = Vec::new();
        let mut proxy_relay = None;
        let proxy = match proxy_address(&env) {
            Some(target) => {
                let socket = temporary.path().join("overlaynet.sock");
                match spawn_unix_tcp_relay(&socket, target) {
                    Ok(task) => {
                        proxy_relay = Some(task);
                        mappings.push(VsockMapping {
                            port: self.settings.proxy_vsock_port,
                            host_socket: socket,
                        });
                        Some(TcpVsockBridge {
                            listen: target,
                            port: self.settings.proxy_vsock_port,
                        })
                    }
                    Err(error) => {
                        return failed_to_start(
                            &spec,
                            context.attempt_id(),
                            started_at,
                            format!("failed to establish the OverlayNet VM relay: {error}"),
                        );
                    }
                }
            }
            None => None,
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
        let guest_executable = executable
            .strip_prefix(Path::new("/"))
            .map(|relative| root.join(relative))
            .unwrap_or_else(|_| root.join(&executable));
        if !guest_executable.is_file() {
            return failed_to_start(
                &spec,
                context.attempt_id(),
                started_at,
                format!(
                    "the running pVisor binary is not visible in the guest root: {}",
                    executable.display()
                ),
            );
        }
        let guest = GuestSpec {
            program: invocation.program.clone(),
            args: invocation.args.clone(),
            env,
            cwd: guest_cwd,
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
            additional_gids: supplementary_groups(),
            proxy,
        };
        let runner = RunnerSpec {
            root,
            guest_executable: executable.clone(),
            guest,
            cpus: self.settings.cpus as u8,
            memory_mib: self.settings.memory_mib,
            library_dir: self.settings.library_dir.clone(),
            vsock_mappings: mappings,
        };
        let runner_path = temporary.path().join("runner.json");
        if let Err(error) = write_private_json(&runner_path, &runner) {
            return failed_to_start(&spec, context.attempt_id(), started_at, error.to_string());
        }

        let mut command = Command::new(executable);
        command
            .env(RUNNER_SPEC_ENV, &runner_path)
            .stdin(stdio(invocation.stdin))
            .stdout(stdio(invocation.stdout))
            .stderr(stdio(invocation.stderr))
            .kill_on_drop(true);
        if let Some(directory) = &self.settings.library_dir {
            command.env("LD_LIBRARY_PATH", directory);
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
        if matches!(end, End::Cancelled | End::Watchdog) {
            if matches!(end, End::Cancelled) {
                context
                    .transition(RunState::Cancelling, Some("cancellation requested".into()))
                    .await;
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        if let Some(task) = proxy_relay {
            task.abort();
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

/// Handle the self-exec libkrun runner or the guest-side process supervisor.
/// Returns `true` when the current process was consumed by an internal mode.
pub fn run_internal_if_requested() -> anyhow::Result<bool> {
    if let Some(path) = std::env::var_os(RUNNER_SPEC_ENV) {
        let spec: RunnerSpec = serde_json::from_slice(&std::fs::read(&path)?)?;
        run_runner(spec)?;
        return Ok(true);
    }
    if std::env::args().nth(1).as_deref() == Some(GUEST_INTERNAL_ARG) {
        let encoded = std::env::var(GUEST_SPEC_ENV)
            .map_err(|_| anyhow::anyhow!("missing {GUEST_SPEC_ENV}"))?;
        let spec: GuestSpec = serde_json::from_str(&encoded)?;
        let code = run_guest(spec)?;
        std::process::exit(code);
    }
    Ok(false)
}

fn run_runner(spec: RunnerSpec) -> anyhow::Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = spec;
        anyhow::bail!("libkrun runner is only supported on Linux");
    }
    #[cfg(target_os = "linux")]
    {
        let krun =
            DynamicLibrary::open(spec.library_dir.as_deref(), &["libkrun.so.2", "libkrun.so"])?;
        let init = DynamicLibrary::open(
            spec.library_dir.as_deref(),
            &["libkrun_init.so.0", "libkrun_init.so"],
        )?;
        crate::sandbox::restrict_krun_runner(spec.root.clone(), spec.library_dir.clone())?;
        unsafe {
            let create: unsafe extern "C" fn() -> i32 = krun.symbol(b"krun_create_ctx\0")?;
            let set_vm: unsafe extern "C" fn(u32, u8, u32) -> i32 =
                krun.symbol(b"krun_set_vm_config\0")?;
            let console: unsafe extern "C" fn(u32, i32, i32, i32) -> i32 =
                krun.symbol(b"krun_add_virtio_console_default\0")?;
            let rootfs: unsafe extern "C" fn(u32, *const c_char, *const c_char, u64, bool) -> i32 =
                krun.symbol(b"krun_add_virtiofs3\0")?;
            let add_vsock: unsafe extern "C" fn(u32, u32) -> i32 =
                krun.symbol(b"krun_add_vsock\0")?;
            let add_vsock_port: unsafe extern "C" fn(u32, u32, *const c_char) -> i32 =
                krun.symbol(b"krun_add_vsock_port\0")?;
            let start: unsafe extern "C" fn(u32) -> i32 = krun.symbol(b"krun_start_enter\0")?;
            let ctx = check_ctx(create(), "krun_create_ctx")?;
            check_krun(
                set_vm(ctx, spec.cpus, spec.memory_mib),
                "krun_set_vm_config",
            )?;
            check_krun(console(ctx, 0, 1, 2), "krun_add_virtio_console_default")?;
            let root_tag = CString::new(ROOT_TAG)?;
            let root = path_cstring(&spec.root)?;
            check_krun(
                rootfs(ctx, root_tag.as_ptr(), root.as_ptr(), 0, false),
                "krun_add_virtiofs3",
            )?;
            check_krun(add_vsock(ctx, 0), "krun_add_vsock")?;
            for mapping in &spec.vsock_mappings {
                let path = path_cstring(&mapping.host_socket)?;
                check_krun(
                    add_vsock_port(ctx, mapping.port, path.as_ptr()),
                    "krun_add_vsock_port",
                )?;
            }

            let from_oci: unsafe extern "C" fn(KrunStr, *mut *mut c_void) -> *mut c_void =
                init.symbol(b"krun_init_builder_from_oci_json\0")?;
            let build: unsafe extern "C" fn(*mut *mut c_void) -> *mut c_void =
                init.symbol(b"krun_init_builder_build\0")?;
            let apply: unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                u32,
                KrunStr,
                *mut *mut c_void,
            ) -> u64 = init.symbol(b"krun_init_config_apply\0")?;

            let guest_json = serde_json::to_string(&spec.guest)?;
            let guest_env = format!("{GUEST_SPEC_ENV}={guest_json}");
            let oci = init_oci_spec(&spec, guest_env, libc::isatty(0) == 1);
            let oci = serde_json::to_vec(&oci)?;
            let mut init_error: *mut c_void = std::ptr::null_mut();
            let mut builder = from_oci(KrunStr::from_bytes(&oci), &mut init_error);
            anyhow::ensure!(
                !builder.is_null() && init_error.is_null(),
                "libkrun-init rejected the OCI process configuration"
            );
            let config = build(&mut builder);
            anyhow::ensure!(!config.is_null(), "libkrun-init returned an empty config");
            let result = apply(
                config,
                krun.handle,
                ctx,
                KrunStr::from_bytes(ROOT_TAG.as_bytes()),
                &mut init_error,
            );
            anyhow::ensure!(
                result == 0 && init_error.is_null(),
                "libkrun-init apply failed with result {result}"
            );
            check_krun(start(ctx), "krun_start_enter")?;
        }
        Ok(())
    }
}

#[cfg(any(target_os = "linux", test))]
fn init_oci_spec(spec: &RunnerSpec, guest_env: String, terminal: bool) -> serde_json::Value {
    serde_json::json!({
        "ociVersion": "1.1.0",
        "mounts": [
            { "destination": "/proc", "type": "proc", "source": "proc" },
            { "destination": "/sys", "type": "sysfs", "source": "sysfs", "options": ["nosuid", "noexec", "nodev", "ro"] },
            { "destination": "/dev", "type": "devtmpfs", "source": "devtmpfs", "options": ["nosuid", "mode=755"] },
            { "destination": "/dev/pts", "type": "devpts", "source": "devpts", "options": ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620"] },
            { "destination": "/dev/shm", "type": "tmpfs", "source": "shm", "options": ["nosuid", "noexec", "nodev", "mode=1777"] },
            { "destination": "/run", "type": "tmpfs", "source": "tmpfs", "options": ["nosuid", "nodev", "mode=755"] },
            { "destination": "/tmp", "type": "tmpfs", "source": "tmpfs", "options": ["nosuid", "nodev", "mode=1777"] }
        ],
        "process": {
            "terminal": terminal,
            "user": {
                "uid": spec.guest.uid,
                "gid": spec.guest.gid,
                "additionalGids": spec.guest.additional_gids,
            },
            "args": [
                spec.guest_executable.display().to_string(),
                GUEST_INTERNAL_ARG,
            ],
            "env": [guest_env],
            "cwd": "/",
            "noNewPrivileges": true,
        }
    })
}

fn run_guest(spec: GuestSpec) -> anyhow::Result<i32> {
    if let Some(proxy) = spec.proxy {
        spawn_tcp_vsock_bridge(proxy)?;
    }
    let status = StdCommand::new(&spec.program)
        .args(&spec.args)
        .env_clear()
        .envs(&spec.env)
        .current_dir(&spec.cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    Ok(status.code().unwrap_or(128))
}

fn spawn_tcp_vsock_bridge(bridge: TcpVsockBridge) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bridge.listen)?;
    std::thread::Builder::new()
        .name("pvisor-krun-proxy".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                spawn_stream_bridge(stream, bridge.port);
            }
        })?;
    Ok(())
}

fn spawn_stream_bridge<S>(stream: S, port: u32)
where
    S: CloneStream + Send + 'static,
{
    std::thread::spawn(move || {
        if let Ok(vsock) = connect_host_vsock(port) {
            let _ = copy_bidirectional_blocking(stream, vsock);
        }
    });
}

fn connect_host_vsock(port: u32) -> std::io::Result<UnixStream> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = port;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "AF_VSOCK requires Linux",
        ))
    }
    #[cfg(target_os = "linux")]
    unsafe {
        let fd = libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let address = SockAddrVm {
            family: libc::AF_VSOCK as libc::sa_family_t,
            reserved: 0,
            port,
            cid: VMADDR_CID_HOST,
            zero: [0; 4],
        };
        let result = libc::connect(
            fd,
            &address as *const SockAddrVm as *const libc::sockaddr,
            std::mem::size_of::<SockAddrVm>() as libc::socklen_t,
        );
        if result != 0 {
            let error = std::io::Error::last_os_error();
            libc::close(fd);
            return Err(error);
        }
        Ok(UnixStream::from_raw_fd(fd))
    }
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct SockAddrVm {
    family: libc::sa_family_t,
    reserved: u16,
    port: u32,
    cid: u32,
    zero: [u8; 4],
}

fn copy_bidirectional_blocking<A, B>(mut left: A, mut right: B) -> std::io::Result<()>
where
    A: CloneStream + Send + 'static,
    B: CloneStream + Send + 'static,
{
    let mut left_reader = left.try_clone_stream()?;
    let mut right_writer = right.try_clone_stream()?;
    let forward = std::thread::spawn(move || std::io::copy(&mut left_reader, &mut right_writer));
    let _ = std::io::copy(&mut right, &mut left);
    let _ = forward.join();
    Ok(())
}

trait CloneStream: Read + Write + Sized {
    fn try_clone_stream(&self) -> std::io::Result<Self>;
}

impl CloneStream for TcpStream {
    fn try_clone_stream(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
}

impl CloneStream for UnixStream {
    fn try_clone_stream(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
}

fn spawn_unix_tcp_relay(
    socket: &Path,
    target: SocketAddr,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let listener = tokio::net::UnixListener::bind(socket)?;
    Ok(tokio::spawn(async move {
        while let Ok((mut local, _)) = listener.accept().await {
            tokio::spawn(async move {
                if let Ok(mut remote) = tokio::net::TcpStream::connect(target).await {
                    let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
                    let _ = local.shutdown().await;
                    let _ = remote.shutdown().await;
                }
            });
        }
    }))
}

fn proxy_address(env: &BTreeMap<String, String>) -> Option<SocketAddr> {
    ["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy"]
        .iter()
        .filter_map(|key| env.get(*key))
        .find_map(|value| value.strip_prefix("http://").unwrap_or(value).parse().ok())
}

fn supplementary_groups() -> Vec<u32> {
    #[cfg(target_os = "linux")]
    unsafe {
        let count = libc::getgroups(0, std::ptr::null_mut());
        if count <= 0 {
            return Vec::new();
        }
        let mut groups = vec![0; count as usize];
        let read = libc::getgroups(count, groups.as_mut_ptr());
        if read < 0 {
            Vec::new()
        } else {
            groups.truncate(read as usize);
            groups
        }
    }
    #[cfg(not(target_os = "linux"))]
    Vec::new()
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

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy)]
struct KrunStr {
    data: *const c_char,
    len: usize,
}

#[cfg(target_os = "linux")]
impl KrunStr {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            data: bytes.as_ptr() as *const c_char,
            len: bytes.len(),
        }
    }
}

#[cfg(target_os = "linux")]
struct DynamicLibrary {
    handle: *mut c_void,
}

#[cfg(target_os = "linux")]
impl DynamicLibrary {
    fn open(directory: Option<&Path>, names: &[&str]) -> anyhow::Result<Self> {
        let mut errors = Vec::new();
        for name in names {
            let candidate = directory
                .map(|directory| directory.join(name))
                .unwrap_or_else(|| PathBuf::from(name));
            let encoded = path_cstring(&candidate)?;
            let handle =
                unsafe { libc::dlopen(encoded.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
            if !handle.is_null() {
                return Ok(Self { handle });
            }
            errors.push(format!("{}: {}", candidate.display(), dlerror()));
        }
        anyhow::bail!("load libkrun dependency failed: {}", errors.join("; "))
    }

    unsafe fn symbol<T: Copy>(&self, name: &[u8]) -> anyhow::Result<T> {
        let symbol = unsafe { libc::dlsym(self.handle, name.as_ptr() as *const c_char) };
        anyhow::ensure!(
            !symbol.is_null(),
            "missing libkrun symbol {}: {}",
            String::from_utf8_lossy(&name[..name.len().saturating_sub(1)]),
            dlerror()
        );
        Ok(unsafe { std::mem::transmute_copy(&symbol) })
    }
}

#[cfg(target_os = "linux")]
impl Drop for DynamicLibrary {
    fn drop(&mut self) {
        unsafe { libc::dlclose(self.handle) };
    }
}

#[cfg(target_os = "linux")]
fn path_cstring(path: &Path) -> anyhow::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    Ok(CString::new(path.as_os_str().as_bytes())?)
}

#[cfg(target_os = "linux")]
fn dlerror() -> String {
    let error = unsafe { libc::dlerror() };
    if error.is_null() {
        "unknown loader error".into()
    } else {
        unsafe { std::ffi::CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(target_os = "linux")]
fn check_ctx(value: i32, operation: &str) -> anyhow::Result<u32> {
    if value < 0 {
        anyhow::bail!("{operation} failed with errno {}", -value);
    }
    Ok(value as u32)
}

#[cfg(target_os = "linux")]
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
        assert!(KvmExecutor::new(KvmSettings::default()).is_ok());
        assert!(KvmExecutor::new(KvmSettings {
            cpus: 9,
            ..KvmSettings::default()
        })
        .is_err());
    }

    #[test]
    fn proxy_address_accepts_injected_loopback_proxy() {
        let env = BTreeMap::from([("HTTP_PROXY".into(), "http://127.0.0.1:19081".into())]);
        assert_eq!(
            proxy_address(&env),
            Some("127.0.0.1:19081".parse().unwrap())
        );
    }

    #[test]
    fn init_oci_spec_preserves_identity_and_masks_host_pseudo_filesystems() {
        let runner = RunnerSpec {
            root: "/merged".into(),
            guest_executable: "/usr/bin/pvisor".into(),
            guest: GuestSpec {
                program: "/bin/true".into(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: "/workspace".into(),
                uid: 1000,
                gid: 100,
                additional_gids: vec![10, 20],
                proxy: None,
            },
            cpus: 2,
            memory_mib: 2048,
            library_dir: None,
            vsock_mappings: Vec::new(),
        };
        let oci = init_oci_spec(&runner, "PERSISTING_KRUN_GUEST_SPEC={}".into(), false);
        assert_eq!(oci["process"]["user"]["uid"], 1000);
        assert_eq!(oci["process"]["user"]["additionalGids"][1], 20);
        let destinations = oci["mounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|mount| mount["destination"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            destinations,
            ["/proc", "/sys", "/dev", "/dev/pts", "/dev/shm", "/run", "/tmp"]
        );
    }
}

use crate::executor::{AttemptContext, RunExecutor};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::sandbox::{INTERNAL_SANDBOX_ARG, NetworkIsolation};
#[cfg(target_os = "macos")]
use crate::sandbox::{MACOS_SANDBOX_EXEC, SEATBELT_ATTESTATION, SeatbeltPlan, seatbelt_profile};
#[cfg(target_os = "linux")]
use crate::sandbox::{ROOTLESS_ATTESTATION, SandboxPlan, landlock_runtime_available};
use crate::sandbox::{SANDBOX_PLAN_ENV, SANDBOX_SETUP_FAILED_WARNING};
use async_trait::async_trait;
use persisting_agentctl::{
    CapabilityDimension, CapabilityEnforcementEvidence, ExecutorDescriptor, ExecutorKind,
    IsolationKind, ProcessInvocation, ProcessOutput, ResourceLimits, RunFailure, RunFailureKind,
    RunInvocation, RunResult, RunSpec, RunState, StdioMode,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use persisting_agentctl::{FilesystemAccess, NetworkCapability};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Path;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command as StdCommand;
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

#[cfg(target_os = "linux")]
struct ResourceCgroup {
    path: PathBuf,
}

#[cfg(target_os = "linux")]
fn validate_cgroup_relative_path(relative: &Path) -> std::io::Result<()> {
    if relative
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        Ok(())
    } else {
        Err(std::io::Error::other("unsafe cgroup v2 membership path"))
    }
}

#[cfg(target_os = "linux")]
impl ResourceCgroup {
    fn prepare(limits: &ResourceLimits) -> std::io::Result<Option<Self>> {
        if limits.memory_bytes.is_none() && limits.processes.is_none() {
            return Ok(None);
        }
        let membership = std::fs::read_to_string("/proc/self/cgroup")?;
        let relative = membership
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .ok_or_else(|| std::io::Error::other("unified cgroup v2 membership is unavailable"))?;
        let relative = Path::new(relative.trim_start_matches('/'));
        validate_cgroup_relative_path(relative)?;
        let parent = Path::new("/sys/fs/cgroup").join(relative);
        let path = parent.join(format!("persisting-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&path)?;
        let configure = (|| {
            if let Some(bytes) = limits.memory_bytes {
                std::fs::write(path.join("memory.max"), bytes.to_string())?;
            }
            if let Some(processes) = limits.processes {
                std::fs::write(path.join("pids.max"), processes.to_string())?;
            }
            Ok::<_, std::io::Error>(())
        })();
        if let Err(error) = configure {
            let _ = std::fs::remove_dir(&path);
            return Err(error);
        }
        Ok(Some(Self { path }))
    }

    fn install(&self, command: &mut Command) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;

        let membership = std::fs::OpenOptions::new()
            .write(true)
            .open(self.path.join("cgroup.procs"))?;
        // SAFETY: the pre-exec hook performs one async-signal-safe write to a
        // cgroup.procs file opened by the parent. Writing `0` moves the calling
        // child into the prepared cgroup before Agent code executes.
        unsafe {
            command.as_std_mut().pre_exec(move || {
                let fd = membership.as_raw_fd();
                let moved = libc::write(fd, b"0".as_ptr().cast(), 1);
                if moved == 1 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for ResourceCgroup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.path);
    }
}

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

#[derive(Debug, Clone, Default)]
pub struct ProcessExecutor {
    /// `Some` selects the platform sandbox launcher. The normal library
    /// default intentionally remains the compatibility host process.
    sandbox_launcher: Option<PathBuf>,
}

struct PreparedCommand {
    command: Command,
    resources: SandboxResources,
}

enum SandboxResources {
    None,
    #[cfg(target_os = "linux")]
    Linux {
        root: PathBuf,
        attestation: tempfile::NamedTempFile,
    },
    #[cfg(target_os = "macos")]
    MacOS {
        scratch: tempfile::TempDir,
        attestation: tempfile::NamedTempFile,
    },
}

impl SandboxResources {
    fn none() -> Self {
        Self::None
    }

    #[cfg(target_os = "linux")]
    fn create() -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            ".pvisor-rootfs-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        let attestation = tempfile::Builder::new()
            .prefix("pvisor-rootless-attestation-")
            .tempfile()?;
        Ok(Self::Linux {
            root: path,
            attestation,
        })
    }

    #[cfg(target_os = "linux")]
    fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Linux { root, .. } => Some(root),
            Self::None => None,
        }
    }

    #[cfg(target_os = "linux")]
    fn attestation_path(&self) -> Option<&Path> {
        match self {
            Self::Linux { attestation, .. } => Some(attestation.path()),
            Self::None => None,
        }
    }

    #[cfg(target_os = "macos")]
    fn create() -> std::io::Result<Self> {
        let scratch = tempfile::Builder::new()
            .prefix("pvisor-seatbelt-scratch-")
            .tempdir()?;
        let attestation = tempfile::Builder::new()
            .prefix("pvisor-seatbelt-attestation-")
            .tempfile()?;
        Ok(Self::MacOS {
            scratch,
            attestation,
        })
    }

    #[cfg(target_os = "macos")]
    fn scratch_path(&self) -> Option<&Path> {
        match self {
            Self::MacOS { scratch, .. } => Some(scratch.path()),
            Self::None => None,
        }
    }

    #[cfg(target_os = "macos")]
    fn attestation_path(&self) -> Option<&Path> {
        match self {
            Self::MacOS { attestation, .. } => Some(attestation.path()),
            Self::None => None,
        }
    }

    #[cfg(target_os = "macos")]
    fn setup_attested(&mut self) -> bool {
        use std::io::{Read, Seek};

        let Self::MacOS { attestation, .. } = self else {
            return true;
        };
        let file = attestation.as_file_mut();
        if file.rewind().is_err() {
            return false;
        }
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).is_ok() && contents == SEATBELT_ATTESTATION
    }

    #[cfg(target_os = "linux")]
    fn setup_attested(&mut self) -> bool {
        use std::io::{Read, Seek};

        let Self::Linux { attestation, .. } = self else {
            return true;
        };
        let file = attestation.as_file_mut();
        if file.rewind().is_err() {
            return false;
        }
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).is_ok() && contents == ROOTLESS_ATTESTATION
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn setup_attested(&mut self) -> bool {
        true
    }
}

impl Drop for SandboxResources {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        if let Self::Linux { root: path, .. } = self {
            // Never recurse over a security-sensitive path.  A successful
            // launcher leaves an empty mountpoint; a non-empty directory is
            // retained for diagnosis instead of being removed destructively.
            let _ = std::fs::remove_dir(path);
        }
    }
}

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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn network_isolation(spec: &RunSpec) -> NetworkIsolation {
    if matches!(spec.capabilities.network, NetworkCapability::Deny) {
        NetworkIsolation::LoopbackOnly
    } else {
        NetworkIsolation::Ambient
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
    /// Build a Linux rootless executor using `launcher` for the trusted
    /// namespace/Landlock setup stage.
    ///
    /// The launcher must dispatch [`crate::sandbox::run_internal_if_requested`]
    /// before starting threads or an async runtime.  The `pvisor` binary is the
    /// canonical launcher and uses this path automatically for default host Runs.
    #[cfg(target_os = "linux")]
    pub fn rootless_with_launcher(launcher: impl Into<PathBuf>) -> std::io::Result<Self> {
        let launcher = launcher.into().canonicalize()?;
        if !is_executable(&launcher) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "rootless sandbox launcher is not executable: {}",
                    launcher.display()
                ),
            ));
        }
        Ok(Self {
            sandbox_launcher: Some(launcher),
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn rootless_with_launcher(launcher: impl Into<PathBuf>) -> std::io::Result<Self> {
        let _ = launcher.into();
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the rootless local process executor is only available on Linux",
        ))
    }

    /// Build a macOS executor that installs a generated Seatbelt profile
    /// before entering the hidden launcher and executing Agent code.
    #[cfg(target_os = "macos")]
    pub fn seatbelt_with_launcher(launcher: impl Into<PathBuf>) -> std::io::Result<Self> {
        let launcher = launcher.into().canonicalize()?;
        if !is_executable(&launcher) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "Seatbelt sandbox launcher is not executable: {}",
                    launcher.display()
                ),
            ));
        }
        if !is_executable(Path::new(MACOS_SANDBOX_EXEC)) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("required Seatbelt launcher is unavailable: {MACOS_SANDBOX_EXEC}"),
            ));
        }
        Ok(Self {
            sandbox_launcher: Some(launcher),
        })
    }

    #[cfg(not(target_os = "macos"))]
    pub fn seatbelt_with_launcher(launcher: impl Into<PathBuf>) -> std::io::Result<Self> {
        let _ = launcher.into();
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the Seatbelt local process sandbox is only available on macOS",
        ))
    }

    pub fn is_rootless(&self) -> bool {
        cfg!(target_os = "linux") && self.sandbox_launcher.is_some()
    }

    pub fn is_seatbelt(&self) -> bool {
        cfg!(target_os = "macos") && self.sandbox_launcher.is_some()
    }

    pub fn is_sandboxed(&self) -> bool {
        self.sandbox_launcher.is_some()
    }

    fn spawn_command(
        &self,
        spec: &RunSpec,
        invocation: &ProcessInvocation,
    ) -> std::io::Result<PreparedCommand> {
        // Resolve a bare command against the host PATH before changing cwd to
        // an OverlayFS merged root. The executable belongs to the host-process
        // executor and need not exist inside the projected lower filesystem.
        let program = resolve_host_program(&invocation.program);
        let (mut command, sandbox_plan, resources) = if let Some(launcher) = &self.sandbox_launcher
        {
            platform_launcher_command(launcher, spec, invocation, &program)?
        } else {
            let mut command = Command::new(program);
            command.args(&invocation.args);
            (command, None, SandboxResources::none())
        };
        command
            .stdin(stdio(invocation.stdin))
            .stdout(stdio(invocation.stdout))
            .stderr(stdio(invocation.stderr))
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
            let mut limits = spec.runtime.resource_limits.clone();
            // RLIMIT_NPROC must be applied after the rootless launcher has
            // created its private PID namespace and reaper. Applying it to
            // the launcher itself can make setup fail with EAGAIN when the
            // host user already has more processes than the requested cap.
            if sandbox_plan.is_some() {
                limits.processes = None;
            }
            install_resource_limit_hook(&mut command, limits);
        }
        if let Some(cwd) = &invocation.cwd {
            command.current_dir(cwd);
        }
        if !invocation.inherit_env {
            command.env_clear();
        }
        command.envs(&invocation.env);
        // This is a reserved supervisor-to-launcher capability. Apply it last
        // so an untrusted Run environment cannot remove or replace the policy.
        if let Some(sandbox_plan) = sandbox_plan {
            command.env(SANDBOX_PLAN_ENV, sandbox_plan);
        }
        #[cfg(target_os = "macos")]
        if let Some(scratch) = resources.scratch_path() {
            // A Run-owned temporary directory avoids granting the Agent the
            // shared /tmp or per-user Darwin temporary hierarchy.
            command.env("TMPDIR", scratch);
        }
        Ok(PreparedCommand { command, resources })
    }
}

/// Probe the namespace primitives used by the default Linux launcher without
/// mutating the pVisor process itself.  A short-lived `unshare` child keeps the
/// probe safe in a multithreaded Tokio process and distinguishes an unavailable
/// host capability from a later Agent failure.
#[cfg(target_os = "linux")]
pub(crate) fn rootless_runtime_available() -> bool {
    landlock_runtime_available()
        && StdCommand::new("unshare")
            .args(["--user", "--mount", "--pid", "--fork", "true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn install_resource_limit_hook(command: &mut Command, limits: ResourceLimits) {
    if limits.is_empty() {
        return;
    }
    use std::os::unix::process::CommandExt;
    // SAFETY: the hook only invokes async-signal-safe getrlimit/setrlimit calls
    // and does not allocate or acquire locks between fork and exec.
    unsafe {
        command
            .as_std_mut()
            .pre_exec(move || apply_resource_limits(&limits));
    }
}

#[cfg(unix)]
fn apply_resource_limits(limits: &ResourceLimits) -> std::io::Result<()> {
    macro_rules! set_limit {
        ($resource:expr, $value:expr) => {{
            let mut current = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if unsafe { libc::getrlimit($resource, &mut current) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let requested = $value as libc::rlim_t;
            let effective = requested.min(current.rlim_max);
            let limit = libc::rlimit {
                rlim_cur: effective,
                rlim_max: effective,
            };
            if unsafe { libc::setrlimit($resource, &limit) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }};
    }

    #[cfg(not(target_os = "macos"))]
    if let Some(bytes) = limits.memory_bytes {
        set_limit!(libc::RLIMIT_AS, bytes);
    }
    if let Some(processes) = limits.processes {
        set_limit!(libc::RLIMIT_NPROC, processes);
    }
    if let Some(milliseconds) = limits.cpu_time_ms {
        let seconds = milliseconds
            .saturating_add(999)
            .checked_div(1_000)
            .unwrap_or(0)
            .max(1);
        set_limit!(libc::RLIMIT_CPU, seconds);
    }
    if let Some(open_files) = limits.open_files {
        set_limit!(libc::RLIMIT_NOFILE, open_files);
    }
    if let Some(bytes) = limits.file_size_bytes {
        set_limit!(libc::RLIMIT_FSIZE, bytes);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_launcher_command(
    launcher: &Path,
    spec: &RunSpec,
    invocation: &ProcessInvocation,
    program: &Path,
) -> std::io::Result<(Command, Option<String>, SandboxResources)> {
    let program = program.canonicalize().map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("resolve Agent executable {}: {error}", program.display()),
        )
    })?;
    let sandbox_root = SandboxResources::create()?;
    let network = network_isolation(spec);
    let plan = rootless_plan(
        spec,
        invocation,
        &program,
        sandbox_root
            .path()
            .expect("created sandbox root")
            .to_owned(),
        sandbox_root
            .attestation_path()
            .expect("created rootless attestation")
            .to_owned(),
        network,
    )?;
    let encoded = serde_json::to_string(&plan).map_err(std::io::Error::other)?;
    let mut command = Command::new(launcher);
    command
        .arg(INTERNAL_SANDBOX_ARG)
        .arg("--")
        .arg(&program)
        .args(&invocation.args);
    Ok((command, Some(encoded), sandbox_root))
}

#[cfg(target_os = "macos")]
fn platform_launcher_command(
    launcher: &Path,
    spec: &RunSpec,
    invocation: &ProcessInvocation,
    program: &Path,
) -> std::io::Result<(Command, Option<String>, SandboxResources)> {
    let program = program.canonicalize().map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("resolve Agent executable {}: {error}", program.display()),
        )
    })?;
    let resources = SandboxResources::create()?;
    let cwd = invocation
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let cwd = cwd.canonicalize()?;
    let mut writable_paths = vec![
        cwd.clone(),
        resources
            .scratch_path()
            .expect("created Seatbelt scratch directory")
            .to_owned(),
        resources
            .attestation_path()
            .expect("created Seatbelt attestation")
            .to_owned(),
    ];
    for path in ["/dev/null", "/dev/zero", "/dev/tty", "/dev/fd"] {
        push_existing(&mut writable_paths, Path::new(path));
    }
    for capability in &spec.capabilities.filesystem {
        if capability.access != FilesystemAccess::ReadWrite {
            continue;
        }
        let path = PathBuf::from(&capability.path);
        let path = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        if !path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "filesystem capability path does not exist: {}",
                    path.display()
                ),
            ));
        }
        writable_paths.push(path);
    }

    let network = network_isolation(spec);
    let (allowed_unix_sockets, local_socket_roots) = if network.is_loopback_only() {
        (
            invocation
                .env
                .get(crate::AGENTCTL_ENDPOINT_ENV)
                .map(PathBuf::from)
                .filter(|path| path.exists())
                .into_iter()
                .collect::<Vec<_>>(),
            vec![
                cwd,
                resources
                    .scratch_path()
                    .expect("created Seatbelt scratch directory")
                    .to_owned(),
            ],
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let (profile, parameters) = seatbelt_profile(
        &writable_paths,
        &allowed_unix_sockets,
        &local_socket_roots,
        network,
    )?;
    let plan = SeatbeltPlan {
        attestation: resources
            .attestation_path()
            .expect("created Seatbelt attestation")
            .to_owned(),
        network,
    };
    let encoded = serde_json::to_string(&plan).map_err(std::io::Error::other)?;

    let mut command = Command::new(MACOS_SANDBOX_EXEC);
    command.arg("-p").arg(profile);
    for (key, path) in parameters {
        let path = path.to_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Seatbelt parameter path is not valid UTF-8: {}",
                    path.display()
                ),
            )
        })?;
        command.arg(format!("-D{key}={path}"));
    }
    command
        .arg("--")
        .arg(launcher)
        .arg(INTERNAL_SANDBOX_ARG)
        .arg("--")
        .arg(program)
        .args(&invocation.args);
    Ok((command, Some(encoded), resources))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_launcher_command(
    _launcher: &std::path::Path,
    _spec: &RunSpec,
    _invocation: &ProcessInvocation,
    _program: &std::path::Path,
) -> std::io::Result<(Command, Option<String>, SandboxResources)> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the local process sandbox is not available on this platform",
    ))
}

#[cfg(target_os = "linux")]
fn rootless_plan(
    spec: &RunSpec,
    invocation: &ProcessInvocation,
    program: &Path,
    root: PathBuf,
    attestation: PathBuf,
    network: NetworkIsolation,
) -> std::io::Result<SandboxPlan> {
    let cwd = invocation
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let cwd = cwd.canonicalize()?;
    let mut read_only = Vec::new();
    let mut read_write = vec![cwd.clone()];

    // A broad but immutable OS runtime keeps arbitrary local executables and
    // dynamic language runtimes working while excluding user data by default.
    for path in ["/bin", "/sbin", "/usr", "/lib", "/lib64", "/etc"] {
        let path = PathBuf::from(path);
        if path.exists() {
            // Preserve compatibility aliases such as /bin and /lib64 inside
            // the synthetic root. Canonicalizing them would project only
            // /usr/bin or /usr/lib and break ELF interpreter paths.
            read_only.push(path);
        }
    }
    // On systemd-resolved hosts this follows /etc/resolv.conf into /run,
    // whose containing hierarchy is intentionally not otherwise projected.
    push_existing(&mut read_only, Path::new("/etc/resolv.conf"));
    read_only.push(program.to_path_buf());
    for path in [
        "/dev/null",
        "/dev/zero",
        "/dev/full",
        "/dev/random",
        "/dev/urandom",
        "/dev/tty",
    ] {
        push_existing(&mut read_write, Path::new(path));
    }

    // The Run-scoped AgentCtl and an explicitly supplied SSH agent are
    // capabilities represented by their exact socket inode, not by /tmp.
    // Merely inheriting the host environment must not project signing
    // authority into a safe Run.
    for key in [crate::AGENTCTL_ENDPOINT_ENV, "SSH_AUTH_SOCK"] {
        if let Some(path) = invocation.env.get(key) {
            push_existing(&mut read_write, Path::new(path));
        }
    }

    for capability in &spec.capabilities.filesystem {
        let path = PathBuf::from(&capability.path);
        let path = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        if !path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "filesystem capability path does not exist: {}",
                    path.display()
                ),
            ));
        }
        match capability.access {
            FilesystemAccess::Read => push_existing(&mut read_only, &path),
            FilesystemAccess::ReadWrite => push_existing(&mut read_write, &path),
        }
    }

    read_only.sort_unstable();
    read_only.dedup();
    read_write.sort_unstable();
    read_write.dedup();
    Ok(SandboxPlan {
        root,
        cwd,
        attestation,
        read_only,
        read_write,
        network,
        process_limit: spec.runtime.resource_limits.processes,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn push_existing(paths: &mut Vec<PathBuf>, path: &Path) {
    if let Ok(path) = path.canonicalize() {
        paths.push(path);
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
        let (name, isolation) = if self.is_rootless() {
            ("local-rootless-v1", IsolationKind::RootlessProcess)
        } else if self.is_seatbelt() {
            ("local-seatbelt-v1", IsolationKind::SandboxedProcess)
        } else {
            ("local-process-v1", IsolationKind::HostProcess)
        };
        let mut capability_enforcement = CapabilityEnforcementEvidence::default()
            .enforced(CapabilityDimension::Resources, "posix-rlimit");
        if self.is_rootless() {
            capability_enforcement = capability_enforcement
                .enforced(
                    CapabilityDimension::FilesystemRead,
                    "linux-synthetic-root-landlock",
                )
                .enforced(
                    CapabilityDimension::FilesystemWrite,
                    "linux-synthetic-root-landlock",
                );
        } else if self.is_seatbelt() {
            capability_enforcement = capability_enforcement.enforced(
                CapabilityDimension::FilesystemWrite,
                "macos-seatbelt-write-policy",
            );
        }
        ExecutorDescriptor {
            name: name.into(),
            kind: ExecutorKind::Process,
            isolation,
            capability_enforcement,
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

        let PreparedCommand {
            mut command,
            mut resources,
        } = match self.spawn_command(&spec, invocation) {
            Ok(command) => command,
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
        let mut warnings = Vec::new();
        #[cfg(target_os = "linux")]
        let mut metrics = std::collections::BTreeMap::new();
        #[cfg(not(target_os = "linux"))]
        let metrics = std::collections::BTreeMap::new();
        #[cfg(target_os = "linux")]
        let _resource_cgroup = match ResourceCgroup::prepare(&spec.runtime.resource_limits) {
            Ok(Some(cgroup)) => match cgroup.install(&mut command) {
                Ok(()) => {
                    metrics.insert("resource.cgroup_v2".into(), 1.0);
                    Some(cgroup)
                }
                Err(error) => {
                    warnings.push(format!(
                        "cgroup v2 resource controller unavailable; using inherited rlimits: {error}"
                    ));
                    None
                }
            },
            Ok(None) => None,
            Err(error) => {
                warnings.push(format!(
                    "cgroup v2 resource controller unavailable; using inherited rlimits: {error}"
                ));
                None
            }
        };
        let mut child = match command.spawn() {
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
        if let Some(task) = stdout_task
            && let Ok(Ok(captured)) = task.await
        {
            output.stdout = Some(captured.text);
            output.stdout_truncated = captured.truncated;
        }
        if let Some(task) = stderr_task
            && let Ok(Ok(captured)) = task.await
        {
            output.stderr = Some(captured.text);
            output.stderr_truncated = captured.truncated;
        }

        let finished_at = crate::util::unix_now_ms();
        let sandbox_attested = resources.setup_attested();
        let sandbox_setup_failed = self.is_sandboxed() && !sandbox_attested;
        let (state, exit_code, failure) = if sandbox_setup_failed {
            (
                RunState::Failed,
                None,
                Some(RunFailure {
                    kind: RunFailureKind::Infrastructure,
                    message: "local sandbox setup failed before Agent execution".into(),
                    retryable: false,
                }),
            )
        } else {
            match end {
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
            }
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
            metrics,
            artifacts: Vec::new(),
            event_stream_ref: None,
            warnings: {
                if sandbox_setup_failed {
                    warnings.push(SANDBOX_SETUP_FAILED_WARNING.into());
                }
                warnings
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_membership_path_rejects_non_normal_components() {
        assert!(validate_cgroup_relative_path(Path::new("user.slice/session.scope")).is_ok());
        assert!(validate_cgroup_relative_path(Path::new("")).is_ok());
        assert!(validate_cgroup_relative_path(Path::new("../escape")).is_err());
        assert!(validate_cgroup_relative_path(Path::new("/absolute")).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_process_receives_requested_open_file_limit() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "ulimit -n"]);
        command.stdout(Stdio::piped());
        install_resource_limit_hook(
            &mut command,
            ResourceLimits {
                open_files: Some(32),
                ..ResourceLimits::default()
            },
        );
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "32");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rootless_executor_reports_an_honest_partial_boundary() {
        let executor =
            ProcessExecutor::rootless_with_launcher(std::env::current_exe().unwrap()).unwrap();
        let descriptor = executor.descriptor();
        assert_eq!(descriptor.name, "local-rootless-v1");
        assert_eq!(descriptor.isolation, IsolationKind::RootlessProcess);
        assert!(
            descriptor
                .capability_enforcement
                .is_enforced(CapabilityDimension::FilesystemRead)
        );
        assert!(
            descriptor
                .capability_enforcement
                .is_enforced(CapabilityDimension::FilesystemWrite)
        );
        assert!(
            !descriptor
                .capability_enforcement
                .is_enforced(CapabilityDimension::Network)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_executor_reports_write_confinement_without_overclaiming_capabilities() {
        let executor =
            ProcessExecutor::seatbelt_with_launcher(std::env::current_exe().unwrap()).unwrap();
        let descriptor = executor.descriptor();
        assert_eq!(descriptor.name, "local-seatbelt-v1");
        assert_eq!(descriptor.isolation, IsolationKind::SandboxedProcess);
        assert!(
            !descriptor
                .capability_enforcement
                .is_enforced(CapabilityDimension::FilesystemRead)
        );
        assert!(
            descriptor
                .capability_enforcement
                .is_enforced(CapabilityDimension::FilesystemWrite)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_plan_and_scratch_override_an_untrusted_environment() {
        let temporary = tempfile::tempdir().unwrap();
        let mut spec = RunSpec::process("run", "agent", "/usr/bin/true");
        {
            let RunInvocation::Process(invocation) = &mut spec.invocation;
            invocation.cwd = Some(temporary.path().display().to_string());
            invocation.inherit_env = false;
            invocation
                .env
                .insert(SANDBOX_PLAN_ENV.into(), r#"{"attestation":"/"}"#.into());
            invocation.env.insert("TMPDIR".into(), "/".into());
        }

        let executor =
            ProcessExecutor::seatbelt_with_launcher(std::env::current_exe().unwrap()).unwrap();
        let RunInvocation::Process(invocation) = &spec.invocation;
        let prepared = executor.spawn_command(&spec, invocation).unwrap();
        let environment = prepared
            .command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let plan: SeatbeltPlan =
            serde_json::from_str(environment.get(SANDBOX_PLAN_ENV).unwrap()).unwrap();
        assert_ne!(plan.attestation, PathBuf::from("/"));
        assert_ne!(environment.get("TMPDIR").map(String::as_str), Some("/"));
        assert!(prepared.resources.scratch_path().unwrap().is_dir());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rootless_plan_is_reserved_even_when_the_run_clears_or_poisons_its_environment() {
        let temporary = tempfile::tempdir().unwrap();
        let mut spec = RunSpec::process("run", "agent", "/bin/true");
        {
            let RunInvocation::Process(invocation) = &mut spec.invocation;
            invocation.cwd = Some(temporary.path().display().to_string());
            invocation.inherit_env = false;
            invocation
                .env
                .insert(SANDBOX_PLAN_ENV.into(), r#"{"read_write":["/"]}"#.into());
        }

        let executor =
            ProcessExecutor::rootless_with_launcher(std::env::current_exe().unwrap()).unwrap();
        let RunInvocation::Process(invocation) = &spec.invocation;
        let command = executor.spawn_command(&spec, invocation).unwrap();
        let encoded = command
            .command
            .as_std()
            .get_envs()
            .find_map(|(key, value)| {
                (key == SANDBOX_PLAN_ENV).then(|| value.unwrap().to_string_lossy().into_owned())
            })
            .expect("trusted sandbox plan must survive env_clear");
        let plan: SandboxPlan = serde_json::from_str(&encoded).unwrap();
        assert_eq!(plan.cwd, temporary.path().canonicalize().unwrap());
        assert_ne!(encoded, r#"{"read_write":["/"]}"#);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn rootless_executor_fails_closed_off_linux() {
        let error = ProcessExecutor::rootless_with_launcher("pvisor").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    }

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

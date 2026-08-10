//! Platform sandbox launchers used by the local process executor.
//!
//! The launcher is a hidden self-exec mode of the `pvisor` binary. On Linux it
//! installs namespaces and Landlock before Agent code starts. On macOS it is
//! entered only after `/usr/bin/sandbox-exec` has installed a generated
//! Seatbelt profile and records an attestation before replacing itself with
//! the Agent.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::PathBuf;

pub(crate) const INTERNAL_SANDBOX_ARG: &str = "__pvisor-sandbox-exec";
pub(crate) const SANDBOX_PLAN_ENV: &str = "PERSISTING_INTERNAL_SANDBOX_PLAN";
/// Reserved launcher exit status: setup failed before the Agent was executed.
#[doc(hidden)]
pub const SANDBOX_SETUP_EXIT_CODE: i32 = 125;
pub(crate) const SANDBOX_SETUP_FAILED_WARNING: &str = "pvisor.sandbox.setup_failed";

#[cfg(target_os = "macos")]
pub(crate) const MACOS_SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
#[cfg(target_os = "macos")]
pub(crate) const SEATBELT_ATTESTATION: &[u8] = b"pvisor-seatbelt-ready-v1\n";

#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 0;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 2;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 3;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_V3: u64 = (1 << 15) - 1;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_READ: u64 =
    LANDLOCK_ACCESS_FS_EXECUTE | LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR;

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SandboxPlan {
    pub root: PathBuf,
    pub cwd: PathBuf,
    pub read_only: Vec<PathBuf>,
    pub read_write: Vec<PathBuf>,
    pub deny_network: bool,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SeatbeltPlan {
    pub attestation: PathBuf,
    pub deny_network: bool,
}

/// Enter the hidden launcher when the first argument is the internal marker.
///
/// Returns `Ok(false)` for an ordinary pVisor invocation.  A successful
/// sandbox invocation never returns because it replaces itself with the Agent.
#[doc(hidden)]
pub fn run_internal_if_requested() -> anyhow::Result<bool> {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new(INTERNAL_SANDBOX_ARG)) {
        return Ok(false);
    }
    run_internal()?;
    Ok(true)
}

#[cfg(target_os = "linux")]
fn run_internal() -> anyhow::Result<()> {
    use anyhow::{bail, Context};
    use std::os::unix::process::CommandExt;

    let encoded = std::env::var(SANDBOX_PLAN_ENV).context("missing rootless sandbox plan")?;
    let plan: SandboxPlan =
        serde_json::from_str(&encoded).context("decode rootless sandbox plan")?;
    let mut arguments = std::env::args_os().skip(2);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        bail!("invalid internal rootless sandbox invocation");
    }
    let program = arguments
        .next()
        .context("rootless sandbox invocation is missing the Agent executable")?;
    let arguments = arguments.collect::<Vec<_>>();

    enter_rootless_namespaces(plan.deny_network)
        .context("initialize rootless user and mount namespaces")?;
    enter_synthetic_root(&plan).context("construct private sandbox root")?;
    std::env::set_current_dir(&plan.cwd)
        .with_context(|| format!("enter sandbox workspace {}", plan.cwd.display()))?;

    // Enumerating /proc/self/fd must happen before Landlock intentionally
    // removes access to the host procfs tree.
    close_unexpected_file_descriptors().context("close inherited file descriptors")?;
    let landlock_abi = install_landlock(&plan).context("install Landlock filesystem policy")?;

    drop_process_capabilities().context("drop namespace capabilities")?;
    std::env::remove_var(SANDBOX_PLAN_ENV);
    std::env::set_var("PERSISTING_SANDBOX_FILESYSTEM", "landlock");
    std::env::set_var("PERSISTING_SANDBOX_LANDLOCK_ABI", landlock_abi.to_string());
    std::env::set_var("PERSISTING_SANDBOX_USER_NAMESPACE", "1");
    std::env::set_var(
        "PERSISTING_SANDBOX_NETWORK",
        if plan.deny_network { "deny" } else { "ambient" },
    );

    Err(std::process::Command::new(program)
        .args(arguments)
        .exec()
        .into())
}

#[cfg(target_os = "macos")]
fn run_internal() -> anyhow::Result<()> {
    use anyhow::{bail, Context};
    use std::io::Write;
    use std::os::unix::process::CommandExt;

    let encoded = std::env::var(SANDBOX_PLAN_ENV).context("missing Seatbelt sandbox plan")?;
    let plan: SeatbeltPlan =
        serde_json::from_str(&encoded).context("decode Seatbelt sandbox plan")?;
    let mut arguments = std::env::args_os().skip(2);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        bail!("invalid internal Seatbelt sandbox invocation");
    }
    let program = arguments
        .next()
        .context("Seatbelt sandbox invocation is missing the Agent executable")?;
    let arguments = arguments.collect::<Vec<_>>();

    // The parent keeps the already-open inode and checks these bytes after the
    // process exits. Unlinking before Agent execution keeps the random path and
    // its narrow write grant out of the Agent-visible filesystem namespace.
    let mut attestation = std::fs::OpenOptions::new()
        .write(true)
        .open(&plan.attestation)
        .with_context(|| {
            format!(
                "open Seatbelt setup attestation {}",
                plan.attestation.display()
            )
        })?;
    attestation
        .write_all(SEATBELT_ATTESTATION)
        .context("write Seatbelt setup attestation")?;
    attestation
        .sync_data()
        .context("sync Seatbelt setup attestation")?;
    drop(attestation);
    std::fs::remove_file(&plan.attestation).with_context(|| {
        format!(
            "unlink Seatbelt setup attestation {}",
            plan.attestation.display()
        )
    })?;

    std::env::remove_var(SANDBOX_PLAN_ENV);
    std::env::set_var("PERSISTING_SANDBOX_FILESYSTEM", "seatbelt-write");
    std::env::set_var(
        "PERSISTING_SANDBOX_NETWORK",
        if plan.deny_network { "deny" } else { "ambient" },
    );

    Err(std::process::Command::new(program)
        .args(arguments)
        .exec()
        .into())
}

#[cfg(target_os = "linux")]
fn install_landlock(plan: &SandboxPlan) -> std::io::Result<u32> {
    use std::io::{Error, ErrorKind};

    // ABI v3 is the minimum useful boundary for a writable workspace: v2
    // controls cross-directory refer and v3 adds truncate.  Calling the small
    // stable kernel ABI directly keeps this launcher dependency-free and makes
    // unsupported hosts fail closed instead of silently degrading.
    const CREATE_RULESET_VERSION: libc::c_uint = 1;
    const RULE_PATH_BENEATH: libc::c_int = 1;
    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }

    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0,
            CREATE_RULESET_VERSION,
        )
    };
    if abi < 0 {
        return Err(Error::last_os_error());
    }
    if abi < 3 {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("Landlock ABI v3 is required; kernel provides v{abi}"),
        ));
    }

    let attr = RulesetAttr {
        handled_access_fs: LANDLOCK_ACCESS_FS_V3,
    };
    let ruleset_fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr,
            std::mem::size_of::<RulesetAttr>(),
            0,
        )
    } as libc::c_int;
    if ruleset_fd < 0 {
        return Err(Error::last_os_error());
    }
    let ruleset = OwnedFd(ruleset_fd);

    for path in &plan.read_only {
        add_landlock_path_rule(ruleset.0, path, LANDLOCK_ACCESS_FS_READ, RULE_PATH_BENEATH)
            .map_err(|error| {
                Error::new(
                    error.kind(),
                    format!("add read-only rule for {}: {error}", path.display()),
                )
            })?;
    }
    for path in &plan.read_write {
        add_landlock_path_rule(ruleset.0, path, LANDLOCK_ACCESS_FS_V3, RULE_PATH_BENEATH).map_err(
            |error| {
                Error::new(
                    error.kind(),
                    format!("add read-write rule for {}: {error}", path.display()),
                )
            },
        )?;
    }

    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(Error::last_os_error());
    }
    if unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset.0, 0) } != 0 {
        return Err(Error::last_os_error());
    }
    Ok(abi as u32)
}

/// Confine the libkrun VMM process while leaving the pVisor FUSE server in the
/// trusted parent. The VMM gets a private network and mount namespace, may
/// access only its virtio-fs root plus KVM/runtime files, and retains no
/// namespace capabilities after setup.
#[cfg(target_os = "linux")]
pub(crate) fn restrict_krun_runner(
    root: PathBuf,
    library_dir: Option<PathBuf>,
) -> anyhow::Result<u32> {
    use anyhow::Context;

    enter_rootless_namespaces(true)
        .context("initialize libkrun user, mount, and network namespaces")?;
    let mut read_only = [
        "/usr/lib",
        "/usr/lib64",
        "/lib",
        "/lib64",
        "/proc/self",
        "/dev/urandom",
    ]
    .into_iter()
    .map(PathBuf::from)
    .filter(|path| path.exists())
    .collect::<Vec<_>>();
    if let Some(directory) = library_dir {
        read_only.push(directory);
    }
    let mut read_write = vec![root];
    if PathBuf::from("/dev/kvm").exists() {
        read_write.push(PathBuf::from("/dev/kvm"));
    }
    let plan = SandboxPlan {
        root: PathBuf::from("/"),
        cwd: PathBuf::from("/"),
        read_only,
        read_write,
        deny_network: true,
    };
    let abi = install_landlock(&plan).context("install libkrun Landlock policy")?;
    drop_process_capabilities().context("drop libkrun namespace capabilities")?;
    Ok(abi)
}

#[cfg(target_os = "linux")]
fn add_landlock_path_rule(
    ruleset_fd: libc::c_int,
    path: &std::path::Path,
    allowed_access: u64,
    rule_type: libc::c_int,
) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileTypeExt;

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: libc::c_int,
    }

    // Landlock rejects directory-only access bits on a non-directory anchor.
    // Filter the requested access against the anchor's inode type before
    // adding the rule.  Pathname Unix sockets are not governed by Landlock's
    // filesystem rights, so there is no useful rule to add for them.
    let file_type = std::fs::metadata(path)?.file_type();
    let allowed_access = if file_type.is_dir() {
        allowed_access
    } else if file_type.is_file() {
        allowed_access
            & (LANDLOCK_ACCESS_FS_EXECUTE
                | LANDLOCK_ACCESS_FS_WRITE_FILE
                | LANDLOCK_ACCESS_FS_READ_FILE
                | LANDLOCK_ACCESS_FS_TRUNCATE)
    } else if file_type.is_socket() {
        return Ok(());
    } else {
        allowed_access & (LANDLOCK_ACCESS_FS_WRITE_FILE | LANDLOCK_ACCESS_FS_READ_FILE)
    };
    if allowed_access == 0 {
        return Ok(());
    }

    let encoded = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("sandbox path contains a NUL byte: {}", path.display()),
        )
    })?;
    let path_fd = unsafe { libc::open(encoded.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if path_fd < 0 {
        return Err(Error::last_os_error());
    }
    let path_fd = OwnedFd(path_fd);
    let attr = PathBeneathAttr {
        allowed_access,
        parent_fd: path_fd.0,
    };
    if unsafe { libc::syscall(libc::SYS_landlock_add_rule, ruleset_fd, rule_type, &attr, 0) } != 0 {
        return Err(Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct OwnedFd(libc::c_int);

#[cfg(target_os = "linux")]
impl Drop for OwnedFd {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn run_internal() -> anyhow::Result<()> {
    anyhow::bail!("the local process sandbox is not available on this platform")
}

/// Generate a compatibility-oriented Seatbelt profile.
///
/// Reads remain ambient so ordinary developer toolchains keep working. Every
/// pathname write outside `writable_paths` is denied by Seatbelt. A deny-all
/// network Run instead starts from `deny default` and admits only the exact
/// Run-scoped Unix sockets plus sockets rooted in Run-owned directories.
#[cfg(target_os = "macos")]
pub(crate) fn seatbelt_profile(
    writable_paths: &[PathBuf],
    allowed_unix_sockets: &[PathBuf],
    local_socket_roots: &[PathBuf],
    deny_network: bool,
) -> std::io::Result<(String, Vec<(String, PathBuf)>)> {
    use std::io::{Error, ErrorKind};

    let writable_paths = canonical_seatbelt_paths(writable_paths, "writable")?;
    if writable_paths.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Seatbelt requires at least one writable path",
        ));
    }
    if writable_paths
        .iter()
        .any(|path| path == std::path::Path::new("/"))
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "the host root cannot be granted as a Seatbelt writable path",
        ));
    }
    let mut parameters = Vec::with_capacity(writable_paths.len());
    for (index, path) in writable_paths.iter().enumerate() {
        let key = format!("PVISOR_WRITABLE_{index}");
        parameters.push((key, path.clone()));
    }

    if deny_network {
        let allowed_unix_sockets = canonical_seatbelt_paths(allowed_unix_sockets, "Unix socket")?;
        let local_socket_roots = canonical_seatbelt_paths(local_socket_roots, "local socket root")?;
        parameters.reserve(allowed_unix_sockets.len() + local_socket_roots.len());
        for (index, path) in allowed_unix_sockets.iter().enumerate() {
            parameters.push((format!("PVISOR_UNIX_SOCKET_{index}"), path.clone()));
        }
        for (index, path) in local_socket_roots.iter().enumerate() {
            parameters.push((format!("PVISOR_SOCKET_ROOT_{index}"), path.clone()));
        }

        // Deny by default for a genuine no-network Run. The allowlist below is
        // intentionally small and mirrors the system services required by
        // shells, language runtimes, PTYs, and read-only preferences. Socket
        // operations are admitted only so the filtered denies below can retain
        // Run-local Unix IPC while rejecting IP and ambient host Unix sockets.
        let mut profile = String::from(
            "(version 1)\n\
             (deny default)\n\
             (allow process-exec)\n\
             (allow process-fork)\n\
             (allow signal (target same-sandbox))\n\
             (allow process-info* (target same-sandbox))\n\
             (allow file-read* file-test-existence file-map-executable)\n\
             (allow sysctl-read)\n\
             (allow system-mac-syscall (mac-policy-name \"vnguard\"))\n\
             (allow system-mac-syscall\n\
               (require-all (mac-policy-name \"Sandbox\") (mac-syscall-number 67)))\n\
             (allow system-fsctl)\n\
             (allow iokit-open (iokit-registry-entry-class \"RootDomainUserClient\"))\n\
             (allow ipc-posix-sem)\n\
             (allow ipc-posix-shm-read*)\n\
             (allow pseudo-tty)\n\
             (allow user-preference-read)\n\
             (allow mach-lookup\n\
               (global-name \"com.apple.system.opendirectoryd.libinfo\")\n\
               (global-name \"com.apple.system.opendirectoryd.membership\")\n\
               (global-name \"com.apple.cfprefsd.daemon\")\n\
               (global-name \"com.apple.cfprefsd.agent\")\n\
               (local-name \"com.apple.cfprefsd.agent\")\n\
               (global-name \"com.apple.PowerManagement.control\"))\n\
             (allow file-ioctl (regex #\"^/dev/ttys[0-9]+$\"))\n\
             (allow system-socket (socket-domain AF_UNIX))\n\
             (allow network*)\n\
             (deny network-bind (local ip))\n\
             (deny network-inbound (local ip))\n\
             (deny network-outbound (remote ip))\n",
        );
        profile.push_str("(allow file-write*\n");
        for index in 0..writable_paths.len() {
            profile.push_str(&format!(
                "  (literal (param \"PVISOR_WRITABLE_{index}\"))\n\
                 (subpath (param \"PVISOR_WRITABLE_{index}\"))\n"
            ));
        }
        profile.push_str(")\n");
        profile.push_str("(deny network-outbound\n  (require-all\n    (remote unix-socket)\n");
        for index in 0..allowed_unix_sockets.len() {
            profile.push_str(&format!(
                "    (require-not (remote unix-socket\n\
                       (literal (param \"PVISOR_UNIX_SOCKET_{index}\"))))\n"
            ));
        }
        for index in 0..local_socket_roots.len() {
            profile.push_str(&format!(
                "    (require-not (remote unix-socket\n\
                       (subpath (param \"PVISOR_SOCKET_ROOT_{index}\"))))\n"
            ));
        }
        profile.push_str("  )\n)\n");
        return Ok((profile, parameters));
    }

    // Starting from `allow default` preserves compatibility with local macOS
    // toolchains. The filtered deny is fail-closed for writes: it matches only
    // when a target is neither an exact writable root nor beneath one.
    let mut profile = String::from(
        "(version 1)\n\
         (allow default)\n\
         (deny file-write*\n\
           (require-all\n",
    );
    for index in 0..writable_paths.len() {
        profile.push_str(&format!(
            "    (require-not (literal (param \"PVISOR_WRITABLE_{index}\")))\n\
             (require-not (subpath (param \"PVISOR_WRITABLE_{index}\")))\n"
        ));
    }
    profile.push_str("  )\n)\n");
    Ok((profile, parameters))
}

#[cfg(target_os = "macos")]
fn canonical_seatbelt_paths(paths: &[PathBuf], kind: &str) -> std::io::Result<Vec<PathBuf>> {
    use std::io::{Error, ErrorKind};

    let mut canonical = paths
        .iter()
        .map(|path| {
            path.canonicalize().map_err(|error| {
                Error::new(
                    error.kind(),
                    format!(
                        "canonicalize Seatbelt {kind} path {}: {error}",
                        path.display()
                    ),
                )
            })
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    canonical.sort_unstable();
    canonical.dedup();
    if canonical.iter().any(|path| path.to_str().is_none()) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Seatbelt {kind} paths must be valid UTF-8"),
        ));
    }
    Ok(canonical)
}

#[cfg(target_os = "linux")]
fn enter_rootless_namespaces(deny_network: bool) -> std::io::Result<()> {
    use std::io::Error;

    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let mut flags = libc::CLONE_NEWUSER | libc::CLONE_NEWNS;
    if deny_network {
        flags |= libc::CLONE_NEWNET;
    }
    if unsafe { libc::unshare(flags) } != 0 {
        return Err(Error::last_os_error());
    }

    // A one-ID identity mapping is sufficient for a local Agent executable and
    // avoids /etc/subuid, newuidmap, and a privileged setup helper.
    match std::fs::write("/proc/self/setgroups", b"deny\n") {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::write("/proc/self/uid_map", format!("{uid} {uid} 1\n"))?;
    std::fs::write("/proc/self/gid_map", format!("{gid} {gid} 1\n"))?;

    // Never propagate mounts performed by the child back into the host mount
    // namespace.  Landlock later prevents the Agent from changing topology.
    if unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn enter_synthetic_root(plan: &SandboxPlan) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    if !plan.root.is_absolute() || plan.root == std::path::Path::new("/") {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "sandbox root must be a non-root absolute path: {}",
                plan.root.display()
            ),
        ));
    }
    if !plan.root.is_dir() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("sandbox root does not exist: {}", plan.root.display()),
        ));
    }

    let root = path_cstring(&plan.root)?;
    if unsafe {
        libc::mount(
            c"tmpfs".as_ptr(),
            root.as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            c"mode=0755,size=16m".as_ptr().cast(),
        )
    } != 0
    {
        return Err(Error::last_os_error());
    }

    // procfs is needed by the trusted launcher for FD cleanup.  Landlock does
    // not admit it to the Agent, including magic-link escape paths.
    bind_path_into_root(&plan.root, std::path::Path::new("/proc"))?;

    let mut paths = plan
        .read_only
        .iter()
        .chain(&plan.read_write)
        .collect::<Vec<_>>();
    paths.sort_unstable_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    paths.dedup();
    for path in paths {
        if path == std::path::Path::new("/") {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "the host root cannot be granted to a rootless sandbox",
            ));
        }
        bind_path_into_root(&plan.root, path)?;
    }

    // chroot is safe here because the process has a private mount namespace,
    // no Agent code has run, every non-stdio FD is closed immediately below,
    // and all namespace capabilities are dropped before exec.
    if unsafe { libc::chroot(root.as_ptr()) } != 0 {
        return Err(Error::last_os_error());
    }
    if unsafe { libc::chdir(c"/".as_ptr()) } != 0 {
        return Err(Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bind_path_into_root(root: &std::path::Path, source: &std::path::Path) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    let relative = source.strip_prefix("/").map_err(|_| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("sandbox path must be absolute: {}", source.display()),
        )
    })?;
    let target = root.join(relative);
    if std::fs::symlink_metadata(&target).is_ok() {
        // A parent hierarchy (for example /usr or /proc) already projects the
        // same absolute source path into the synthetic root.
        return Ok(());
    }
    let metadata = std::fs::metadata(source)?;
    if metadata.is_dir() {
        std::fs::create_dir_all(&target)?;
    } else {
        let parent = target.parent().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("sandbox target has no parent: {}", target.display()),
            )
        })?;
        std::fs::create_dir_all(parent)?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
    }

    let source = path_cstring(source)?;
    let target = path_cstring(&target)?;
    let flags = libc::MS_BIND | if metadata.is_dir() { libc::MS_REC } else { 0 };
    if unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            flags,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn path_cstring(path: &std::path::Path) -> std::io::Result<std::ffi::CString> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::ffi::OsStrExt;

    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("sandbox path contains a NUL byte: {}", path.display()),
        )
    })
}

#[cfg(target_os = "linux")]
fn drop_process_capabilities() -> std::io::Result<()> {
    use std::io::Error;

    const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
    #[repr(C)]
    struct CapabilityHeader {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CapabilityData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }

    let mut header = CapabilityHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [CapabilityData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    if unsafe { libc::syscall(libc::SYS_capset, &mut header, data.as_mut_ptr()) } != 0 {
        return Err(Error::last_os_error());
    }
    if unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        )
    } != 0
    {
        return Err(Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn close_unexpected_file_descriptors() -> std::io::Result<()> {
    let mut descriptors = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd")? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(fd) = name.parse::<libc::c_int>() else {
            continue;
        };
        if fd > libc::STDERR_FILENO {
            descriptors.push(fd);
        }
    }
    descriptors.sort_unstable();
    descriptors.dedup();
    for fd in descriptors {
        unsafe {
            libc::close(fd);
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn seatbelt_profile_uses_parameters_and_rejects_a_writable_host_root() {
        let temporary = tempfile::Builder::new()
            .prefix("pvisor-\")-(deny-default-")
            .tempdir()
            .unwrap();
        let canonical = temporary.path().canonicalize().unwrap();
        let (profile, parameters) =
            seatbelt_profile(&[temporary.path().to_owned()], &[], &[], true).unwrap();

        assert!(!profile.contains(canonical.to_str().unwrap()));
        assert_eq!(parameters, [("PVISOR_WRITABLE_0".into(), canonical)]);
        assert!(profile.contains("(deny default)"));
        assert!(!profile.contains("(allow network-outbound"));

        let error = seatbelt_profile(&[PathBuf::from("/")], &[], &[], false).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}

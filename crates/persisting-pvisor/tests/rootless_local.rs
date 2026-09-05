#![cfg(target_os = "linux")]

use persisting_agentctl::{IsolationKind, RunFailureKind};
use persisting_pvisor::RunBundle;
use persisting_pvisor::sandbox::SANDBOX_SETUP_EXIT_CODE;
use std::fs;
use std::fs::OpenOptions;
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn only_run(root: &Path) -> PathBuf {
    if root.join("run-bundle.json").is_file() {
        return root.to_path_buf();
    }
    let runs = fs::read_dir(root)
        .expect("read Run root")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("run-bundle.json").is_file())
        .collect::<Vec<_>>();
    assert_eq!(runs.len(), 1, "expected one finalized Run in {root:?}");
    runs.into_iter().next().unwrap()
}

fn stage_root(run_home: &Path) -> PathBuf {
    run_home
        .parent()
        .expect("temporary run root has a parent")
        .join("stage")
}

fn setup_failure(root: &Path) -> Option<String> {
    let root = if root.exists() {
        root.to_path_buf()
    } else {
        stage_root(root)
    };
    if root.join("run-bundle.json").is_file() {
        return RunBundle::read(&root).ok()?.run.output.stderr;
    }
    let run = fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.join("run-bundle.json").is_file())?;
    let bundle = RunBundle::read(&run).ok()?;
    bundle.run.output.stderr
}

fn user_namespaces_are_unavailable(stderr: &str) -> bool {
    const CONTEXT: &str = "initialize rootless user and mount namespaces: ";
    stderr.lines().any(|line| {
        let Some((_, error)) = line.split_once(CONTEXT) else {
            return false;
        };
        error == "unshare user namespace: Operation not permitted (os error 1)"
            || error == "unshare user namespace: Permission denied (os error 13)"
    })
}

fn skip_if_user_namespaces_are_explicitly_optional(
    run_home: &Path,
    output: &std::process::Output,
) -> bool {
    if std::env::var_os("PERSISTING_TEST_ALLOW_NO_USERNS").is_none() || output.status.success() {
        return false;
    }
    let combined = String::from_utf8_lossy(&output.stderr);
    // Safe-best-effort intentionally falls back to the host process when the
    // runner cannot create namespaces. Treat that capability result as a
    // skippable environment condition, just like the launcher setup error.
    if combined.contains("falling back to host process") {
        eprintln!("skipping: the test host disables required namespaces");
        return true;
    }
    let Some(stderr) = setup_failure(run_home) else {
        return false;
    };
    if !user_namespaces_are_unavailable(&stderr) {
        if !stderr.is_empty() {
            eprintln!("rootless sandbox failure was not skippable: {stderr}");
        }
        return false;
    }
    eprintln!("skipping: the test host disables unprivileged user namespaces: {stderr}");
    true
}

fn skip_if_rootless_runtime_is_explicitly_optional() -> bool {
    if std::env::var_os("PERSISTING_TEST_ALLOW_NO_USERNS").is_none() {
        return false;
    }
    let available = Command::new("unshare")
        .args(["--user", "--mount", "--pid", "--fork", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !available {
        eprintln!("skipping: the test host disables the rootless PID namespace");
    }
    !available
}

#[test]
fn safe_local_executable_cannot_escape_the_workspace() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let outside = temporary.path().join("outside");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"host-secret").unwrap();
    symlink(&outside, workspace.join("escape-link")).unwrap();

    let script = r#"
set -eu
printf 'staged' > staged.txt
if cat "$OUTSIDE_SECRET" >/dev/null 2>&1; then
  echo 'outside read unexpectedly succeeded' >&2
  exit 40
fi
if printf 'escaped' > "$OUTSIDE_WRITE" 2>/dev/null; then
  echo 'outside write unexpectedly succeeded' >&2
  exit 41
fi
if cat escape-link/secret.txt >/dev/null 2>&1; then
  echo 'symlink escape read unexpectedly succeeded' >&2
  exit 42
fi
if printf 'escaped' > escape-link/symlink-escaped.txt 2>/dev/null; then
  echo 'symlink escape write unexpectedly succeeded' >&2
  exit 43
fi
if cat "/proc/self/root$OUTSIDE_SECRET" >/dev/null 2>&1; then
  echo 'proc root escape unexpectedly succeeded' >&2
  exit 44
fi
printf '%s:%s:%s\n' "$PERSISTING_SANDBOX_FILESYSTEM" "$PERSISTING_SANDBOX_LANDLOCK_ABI" "$PERSISTING_SANDBOX_USER_NAMESPACE"
"#;
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .env("OUTSIDE_SECRET", outside.join("secret.txt"))
        .env("OUTSIDE_WRITE", outside.join("escaped.txt"))
        .args([
            "run",
            "--stdio",
            "capture",
            "--stage",
            temporary.path().join("stage").to_str().unwrap(),
            "--pass-env",
            "OUTSIDE_SECRET",
            "--pass-env",
            "OUTSIDE_WRITE",
        ])
        .current_dir(&workspace)
        .args(["--", "/bin/sh", "-c", script])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!outside.join("escaped.txt").exists());
    assert!(!outside.join("symlink-escaped.txt").exists());
    assert!(!workspace.join("staged.txt").exists());

    let run = only_run(&stage_root(&run_home));
    let bundle = RunBundle::read(&run).unwrap();
    assert_eq!(
        bundle
            .run
            .executor
            .as_ref()
            .map(|executor| executor.isolation),
        Some(IsolationKind::RootlessProcess)
    );
    assert!(bundle.safety.filesystem_non_bypassable);
    assert!(bundle.safety.filesystem_changes_staged);
    assert!(!bundle.safety.network_non_bypassable);
    assert!(
        bundle
            .safety
            .warnings
            .iter()
            .any(|warning| warning.contains("process-tree cleanup"))
    );
    assert!(
        bundle
            .safety
            .warnings
            .iter()
            .all(|warning| !warning.contains("host PID namespace"))
    );
    let filesystem = bundle
        .filesystem
        .as_ref()
        .expect("staged filesystem summary");
    assert!(filesystem.upper.join("staged.txt").is_file());
    assert!(filesystem.changed_files >= 1);
    assert!(
        bundle
            .run
            .output
            .stdout
            .as_deref()
            .is_some_and(|stdout| stdout.contains("landlock:") && stdout.ends_with(":1\n")),
        "captured output: {:?}",
        bundle.run.output.stdout
    );
}

#[test]
fn safe_local_executable_cannot_mutate_outside_metadata() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let outside = temporary.path().join("outside");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let protected = outside.join("protected.txt");
    fs::write(&protected, b"host-content").unwrap();
    fs::set_permissions(&protected, fs::Permissions::from_mode(0o640)).unwrap();

    let script = r#"
set -eu
if chmod 000 "$OUTSIDE_FILE" 2>/dev/null; then exit 60; fi
if rm "$OUTSIDE_FILE" 2>/dev/null; then exit 61; fi
if mv "$OUTSIDE_FILE" stolen.txt 2>/dev/null; then exit 62; fi
if ln "$OUTSIDE_FILE" hardlink.txt 2>/dev/null; then exit 63; fi
if mkdir "$OUTSIDE_DIR/created" 2>/dev/null; then exit 64; fi
if touch -r "$OUTSIDE_FILE" timestamp-copy 2>/dev/null; then exit 65; fi
printf metadata-denied
"#;
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .env("OUTSIDE_FILE", &protected)
        .env("OUTSIDE_DIR", &outside)
        .args([
            "run",
            "--stdio",
            "capture",
            "--stage",
            temporary.path().join("stage").to_str().unwrap(),
            "--pass-env",
            "OUTSIDE_FILE",
            "--pass-env",
            "OUTSIDE_DIR",
        ])
        .current_dir(&workspace)
        .args(["--", "/bin/sh", "-c", script])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&protected).unwrap(), b"host-content");
    assert_eq!(
        fs::metadata(&protected).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(!outside.join("created").exists());
    assert!(!workspace.join("stolen.txt").exists());
    assert!(!workspace.join("hardlink.txt").exists());
    assert!(!workspace.join("timestamp-copy").exists());

    let bundle = RunBundle::read(&only_run(&stage_root(&run_home))).unwrap();
    assert!(bundle.safety.filesystem_read_non_bypassable);
    assert!(bundle.safety.filesystem_write_non_bypassable);
    assert!(
        bundle
            .run
            .output
            .stdout
            .as_deref()
            .is_some_and(|stdout| stdout == "metadata-denied")
    );
}

#[test]
fn safe_run_selectively_applies_then_drops_remaining_changes() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::create_dir_all(workspace.join("tests")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .args(["run", "--stdio", "capture", "--stage"])
        .arg(temporary.path().join("stage"))
        .current_dir(&workspace)
        .args([
            "--",
            "/bin/sh",
            "-c",
            "printf promoted > src/promote.txt; printf deferred > tests/defer.txt",
        ])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!workspace.join("src/promote.txt").exists());
    assert!(!workspace.join("tests/defer.txt").exists());

    let run = only_run(&stage_root(&run_home));
    let apply = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .args(["apply"])
        .arg(&run)
        .args(["--path", "src"])
        .output()
        .unwrap();
    assert!(
        apply.status.success(),
        "selective apply failed: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        fs::read_to_string(workspace.join("src/promote.txt")).unwrap(),
        "promoted"
    );
    assert!(!workspace.join("tests/defer.txt").exists());

    let status = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .args(["status"])
        .arg(&run)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["apply_history"].as_array().unwrap().len(), 1);
    assert_eq!(status["filesystem"]["state"], "staged");
    assert!(
        status["filesystem"]["sample_paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path.as_str().is_some_and(|path| path.contains("defer.txt")))
    );

    let drop = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .arg("drop")
        .arg(&run)
        .output()
        .unwrap();
    assert!(
        drop.status.success(),
        "drop failed: {}",
        String::from_utf8_lossy(&drop.stderr)
    );
    assert!(!workspace.join("tests/defer.txt").exists());

    let status = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .args(["status"])
        .arg(&run)
        .arg("--json")
        .output()
        .unwrap();
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["filesystem"]["state"], "discarded");
    assert_eq!(status["filesystem"]["changed_files"], 0);
    assert_eq!(status["apply_history"].as_array().unwrap().len(), 1);
}

#[test]
fn safe_apply_refuses_to_overwrite_a_concurrently_changed_target() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("value.txt"), b"original").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .args(["run", "--stdio", "capture", "--stage"])
        .arg(temporary.path().join("stage"))
        .current_dir(&workspace)
        .args(["--", "/bin/sh", "-c", "printf staged > value.txt"])
        .output()
        .unwrap();
    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(output.status.success());
    assert_eq!(fs::read(workspace.join("value.txt")).unwrap(), b"original");
    fs::write(workspace.join("value.txt"), b"concurrent").unwrap();

    let run = only_run(&stage_root(&run_home));
    let apply = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .arg("apply")
        .arg(&run)
        .output()
        .unwrap();
    assert!(!apply.status.success());
    assert!(
        String::from_utf8_lossy(&apply.stderr).contains("target changed after staging"),
        "stderr: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        fs::read(workspace.join("value.txt")).unwrap(),
        b"concurrent"
    );
    let bundle = RunBundle::read(&run).unwrap();
    assert_eq!(
        fs::read(bundle.filesystem.as_ref().unwrap().upper.join("value.txt")).unwrap(),
        b"staged"
    );
}

#[test]
fn safe_launcher_closes_inherited_host_file_descriptors() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    let secret_path = temporary.path().join("descriptor-secret.txt");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(&secret_path, b"descriptor-secret").unwrap();

    let secret = OpenOptions::new().read(true).open(&secret_path).unwrap();
    let leaked_fd = secret.as_raw_fd();
    let flags = unsafe { libc::fcntl(leaked_fd, libc::F_GETFD) };
    assert!(flags >= 0, "read descriptor flags");
    let cleared = unsafe { libc::fcntl(leaked_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };
    assert_eq!(cleared, 0, "make the fixture descriptor inheritable");

    // An already-connected socket would bypass pathname and connect-time
    // policy if it survived exec, so exercise it independently from the file.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let inherited_socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let socket_fd = inherited_socket.as_raw_fd();
    let socket_flags = unsafe { libc::fcntl(socket_fd, libc::F_GETFD) };
    assert!(socket_flags >= 0, "read socket descriptor flags");
    let cleared =
        unsafe { libc::fcntl(socket_fd, libc::F_SETFD, socket_flags & !libc::FD_CLOEXEC) };
    assert_eq!(cleared, 0, "make the fixture socket inheritable");

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .env("PERSISTING_LEAKED_FD", leaked_fd.to_string())
        .env("PERSISTING_LEAKED_SOCKET_FD", socket_fd.to_string())
        .args([
            "run",
            "--stdio",
            "capture",
            "--stage",
            temporary.path().join("stage").to_str().unwrap(),
            "--overlaynet-deny-all",
            "--pass-env",
            "PERSISTING_LEAKED_FD",
            "--pass-env",
            "PERSISTING_LEAKED_SOCKET_FD",
        ])
        .current_dir(&workspace)
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "inherited_fd_probe_agent",
            "--nocapture",
        ])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let bundle = RunBundle::read(&only_run(&stage_root(&run_home))).unwrap();
    assert!(bundle.safety.filesystem_non_bypassable);
    assert!(
        bundle
            .run
            .output
            .stdout
            .as_deref()
            .is_some_and(|stdout| stdout.contains("inherited descriptors closed"))
    );
}

/// Re-enter this test binary as the untrusted Agent. The parent deliberately
/// passes an inheritable descriptor through pVisor; the trusted launcher must
/// close it before executing this function.
#[test]
#[ignore]
fn inherited_fd_probe_agent() {
    for name in ["PERSISTING_LEAKED_FD", "PERSISTING_LEAKED_SOCKET_FD"] {
        let fd = std::env::var(name)
            .unwrap_or_else(|_| panic!("missing {name}"))
            .parse::<libc::c_int>()
            .expect("numeric descriptor");
        let result = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_eq!(result, -1, "host descriptor {fd} remained open");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF),
            "closed descriptor should report EBADF"
        );
        assert!(
            !PathBuf::from(format!("/proc/self/fd/{fd}")).exists(),
            "closed descriptor remained reachable through procfs"
        );
    }
    println!("inherited descriptors closed");
}

#[test]
fn denied_network_uses_a_private_network_namespace() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(&workspace).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host_port = listener.local_addr().unwrap().port().to_string();

    let script = r#"
set -eu
test "$PERSISTING_SANDBOX_NETWORK" = deny
if exec 3<>"/dev/tcp/127.0.0.1/${HOST_PORT}"; then
  echo 'host listener unexpectedly reachable' >&2
  exit 50
fi
printf 'network:%s\n' "$PERSISTING_SANDBOX_NETWORK"
"#;
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .env("HOST_PORT", host_port)
        .args([
            "run",
            "--stdio",
            "capture",
            "--stage",
            temporary.path().join("stage").to_str().unwrap(),
            "--overlaynet-deny-all",
            "--pass-env",
            "HOST_PORT",
        ])
        .current_dir(&workspace)
        .args(["--", "/bin/bash", "-c", script])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let run = only_run(&stage_root(&run_home));
    let bundle = RunBundle::read(&run).unwrap();
    assert!(bundle.safety.network_non_bypassable);
    assert!(
        bundle
            .run
            .output
            .stdout
            .as_deref()
            .is_some_and(|stdout| stdout.contains("network:deny")),
        "captured output: {:?}",
        bundle.run.output.stdout
    );
    assert!(
        bundle
            .safety
            .warnings
            .iter()
            .all(|warning| !warning.contains("direct sockets may bypass")),
        "safety warnings: {:?}",
        bundle.safety.warnings
    );
}

#[test]
fn synthetic_root_hides_ungranted_host_unix_sockets() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(&workspace).unwrap();
    let host_socket = temporary.path().join("host-control.sock");
    let _listener = UnixListener::bind(&host_socket).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .env("PERSISTING_SOCKET_PROBE", &host_socket)
        .env("SSH_AUTH_SOCK", &host_socket)
        .args(["run", "--stdio", "capture", "--stage"])
        .arg(temporary.path().join("stage"))
        .current_dir(&workspace)
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "unix_socket_probe_agent"])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let bundle = RunBundle::read(&only_run(&stage_root(&run_home))).unwrap();
    if !bundle.safety.filesystem_non_bypassable
        && String::from_utf8_lossy(&output.stderr).contains("falling back to host process")
    {
        eprintln!("skipping: synthetic root unavailable on this test host");
        return;
    }
    assert!(bundle.safety.filesystem_non_bypassable);
}

#[test]
fn safe_run_reaps_setsid_double_fork_descendants_after_success() {
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(&workspace).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .args(["run", "--stdio", "capture", "--stage"])
        .arg(temporary.path().join("stage"))
        .current_dir(&workspace)
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "daemon_escape_probe_agent"])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let bundle = RunBundle::read(&only_run(&stage_root(&run_home))).unwrap();
    let socket = bundle
        .filesystem
        .as_ref()
        .expect("staged filesystem")
        .upper
        .join("daemon.sock");
    assert!(socket.exists(), "daemon did not publish its socket");
    let error = UnixStream::connect(&socket)
        .expect_err("setsid/double-fork descendant survived successful Run completion");
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
    ));
}

/// Re-enter this integration-test executable as an Agent that deliberately
/// escapes its process group and double-forks a long-lived Unix listener.
#[test]
#[ignore]
fn daemon_escape_probe_agent() {
    let status = Command::new("/usr/bin/setsid")
        .arg("--fork")
        .arg(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "daemon_listener_agent"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("launch setsid daemon");
    assert!(status.success(), "setsid launcher failed: {status}");
    for _ in 0..500 {
        if Path::new("daemon.ready").is_file() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("daemon did not become ready");
}

#[test]
#[ignore]
fn daemon_listener_agent() {
    let listener = UnixListener::bind("daemon.sock").expect("bind daemon socket");
    fs::write("daemon.ready", b"ready").expect("publish daemon readiness");
    loop {
        match listener.accept() {
            Ok((_stream, _address)) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => panic!("daemon listener failed: {error}"),
        }
    }
}

#[test]
fn sandboxed_agent_may_legitimately_exit_with_reserved_launcher_code() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(&workspace).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .args(["run", "--stdio", "capture", "--stage"])
        .arg(temporary.path().join("stage"))
        .current_dir(&workspace)
        .args(["--", "/bin/sh", "-c", "exit 125"])
        .output()
        .unwrap();
    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }

    assert!(!output.status.success());
    let bundle = RunBundle::read(&only_run(&stage_root(&run_home))).unwrap();
    assert_eq!(bundle.run.exit_code, Some(SANDBOX_SETUP_EXIT_CODE));
    assert_eq!(
        bundle.run.failure.as_ref().map(|failure| failure.kind),
        Some(RunFailureKind::ProcessExit)
    );
    assert!(
        bundle
            .run
            .warnings
            .iter()
            .all(|warning| warning != "pvisor.sandbox.setup_failed")
    );
}

/// Re-enter this integration-test executable as the untrusted Agent so the
/// socket regression does not depend on Python, curl, socat, or netcat.
#[test]
#[ignore]
fn unix_socket_probe_agent() {
    let Some(path) = std::env::var_os("PERSISTING_SOCKET_PROBE") else {
        return;
    };
    let error = UnixStream::connect(path).expect_err("ungranted host socket must be hidden");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);

    let inherited_ssh_agent =
        std::env::var_os("SSH_AUTH_SOCK").expect("the Agent should still see inherited metadata");
    let error = UnixStream::connect(inherited_ssh_agent)
        .expect_err("ambient SSH signing authority must not be projected");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);

    let agentctl = std::env::var_os(persisting_pvisor::AGENTCTL_ENDPOINT_ENV)
        .expect("pVisor must inject its Run-scoped AgentCtl endpoint");
    UnixStream::connect(agentctl).expect("the explicitly projected AgentCtl socket must work");
}

#[test]
fn internal_launcher_reports_setup_failure_with_reserved_status() {
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .arg("__pvisor-sandbox-exec")
        .arg("--")
        .arg("/bin/true")
        .env("PERSISTING_INTERNAL_SANDBOX_PLAN", "not-json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(SANDBOX_SETUP_EXIT_CODE));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("decode rootless sandbox plan"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn namespace_setup_failure_classifier_is_narrow() {
    assert!(user_namespaces_are_unavailable(
        "pVisor local sandbox setup failed: initialize rootless user and mount namespaces: unshare user namespace: Operation not permitted (os error 1)"
    ));
    assert!(user_namespaces_are_unavailable(
        "pVisor local sandbox setup failed: initialize rootless user and mount namespaces: unshare user namespace: Permission denied (os error 13)"
    ));
    assert!(!user_namespaces_are_unavailable(
        "apply Landlock rules: Permission denied (os error 13)"
    ));
    assert!(!user_namespaces_are_unavailable(
        "initialize rootless user and mount namespaces: No such file or directory (os error 2)"
    ));
    assert!(!user_namespaces_are_unavailable(
        "initialize rootless user and mount namespaces: unshare mount namespace: Operation not permitted (os error 1)"
    ));
}

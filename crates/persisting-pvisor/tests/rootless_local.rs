#![cfg(target_os = "linux")]

use persisting_control::IsolationKind;
use persisting_pvisor::sandbox::SANDBOX_SETUP_EXIT_CODE;
use persisting_pvisor::RunBundle;
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::symlink;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;

fn only_run(root: &Path) -> PathBuf {
    let runs = fs::read_dir(root)
        .expect("read Run root")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("run-bundle.json").is_file())
        .collect::<Vec<_>>();
    assert_eq!(runs.len(), 1, "expected one finalized Run in {root:?}");
    runs.into_iter().next().unwrap()
}

fn setup_failure(root: &Path) -> Option<String> {
    let run = fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.join("run-bundle.json").is_file())?;
    let bundle = RunBundle::read(&run).ok()?;
    bundle
        .run
        .output
        .stderr
        .filter(|stderr| stderr.contains("rootless sandbox setup failed"))
}

fn skip_if_user_namespaces_are_explicitly_optional(
    run_home: &Path,
    output: &std::process::Output,
) -> bool {
    if std::env::var_os("PERSISTING_TEST_ALLOW_NO_USERNS").is_none() || output.status.success() {
        return false;
    }
    let Some(stderr) = setup_failure(run_home) else {
        return false;
    };
    if !stderr.contains("initialize rootless user and mount namespaces: Operation not permitted") {
        return false;
    }
    eprintln!("skipping: the test host disables unprivileged user namespaces: {stderr}");
    true
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
        .args(["run", "--safe", "--stdio", "capture", "--workspace"])
        .arg(&workspace)
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

    let run = only_run(&run_home);
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
            .any(|warning| warning.contains("host PID namespace")),
        "safety warnings: {:?}",
        bundle.safety.warnings
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
            "--safe",
            "--stdio",
            "capture",
            "--overlaynet-deny-all",
            "--workspace",
        ])
        .arg(&workspace)
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

    let run = only_run(&run_home);
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
        .args(["run", "--safe", "--stdio", "capture", "--workspace"])
        .arg(&workspace)
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
    let bundle = RunBundle::read(&only_run(&run_home)).unwrap();
    assert!(bundle.safety.filesystem_non_bypassable);
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

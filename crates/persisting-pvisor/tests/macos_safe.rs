#![cfg(target_os = "macos")]

use persisting_agentctl::IsolationKind;
use persisting_pvisor::RunBundle;
use std::fs;
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;

fn macfuse_is_installed() -> bool {
    Path::new("/Library/Filesystems/macfuse.fs").is_dir()
}

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

#[test]
fn safe_profile_stages_reviews_and_applies_on_macos() {
    if !macfuse_is_installed() {
        eprintln!(
            "skipping macOS safe-profile smoke test: install macFUSE to exercise staged writes"
        );
        return;
    }

    let temporary = tempfile::Builder::new()
        .prefix("pvmac")
        .tempdir_in("/tmp")
        .expect("create short macOS fixture path");
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    let outside = temporary.path().join("outside.txt");
    let outside_secret = temporary.path().join("outside-secret.txt");
    fs::create_dir(&workspace).unwrap();
    fs::write(&outside_secret, "read-compatible").unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
    command
        .env("PERSISTING_RUN_HOME", &run_home)
        .args(["run", "--stdio", "capture", "--overlayfs-compose"])
        .arg(&workspace)
        .args([
            "--",
            "/bin/sh",
            "-c",
            r#"
                test "$PERSISTING_SANDBOX_FILESYSTEM" = seatbelt-write || exit 38
                test "$PERSISTING_SANDBOX_NETWORK" = ambient || exit 39
                test "$(cat "$2")" = read-compatible || exit 40
                if printf escaped > "$1" 2>/dev/null; then exit 41; fi
                ln -s "$1" outside-link
                if printf escaped > outside-link 2>/dev/null; then exit 42; fi
                if ln "$2" outside-hardlink 2>/dev/null; then
                    printf mutated > outside-hardlink 2>/dev/null || true
                    rm -f outside-hardlink
                fi
                test "$(cat "$2")" = read-compatible || exit 43
                printf scratch > "$TMPDIR/probe"
                printf staged > macos-staged.txt
                printf macos-ok
            "#,
            "pvisor-macos-test",
        ])
        .arg(&outside)
        .arg(&outside_secret);
    let output = command.output().expect("run macOS safe profile");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!workspace.join("macos-staged.txt").exists());
    assert!(
        !outside.exists(),
        "Seatbelt allowed a write outside the stage"
    );
    assert_eq!(
        fs::read_to_string(&outside_secret).unwrap(),
        "read-compatible"
    );

    let run = only_run(&run_home);
    let bundle = RunBundle::read(&run).unwrap();
    assert_eq!(
        bundle
            .run
            .executor
            .as_ref()
            .map(|executor| executor.isolation),
        Some(IsolationKind::SandboxedProcess)
    );
    assert!(bundle.safety.safe_profile_requested);
    assert!(bundle.safety.filesystem_changes_staged);
    assert!(!bundle.safety.filesystem_non_bypassable);
    assert!(!bundle.safety.filesystem_read_non_bypassable);
    assert!(bundle.safety.filesystem_write_non_bypassable);
    assert!(!bundle.safety.network_non_bypassable);
    assert!(
        bundle
            .run
            .output
            .stdout
            .as_deref()
            .is_some_and(|stdout| stdout == "macos-ok")
    );
    let filesystem = bundle.filesystem.as_ref().expect("filesystem summary");
    assert_eq!(filesystem.changed_files, 2);
    assert!(filesystem.upper.join("macos-staged.txt").is_file());
    assert!(filesystem.upper.join("outside-link").is_symlink());

    let review = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .args(["review", "--json"])
        .arg(&run)
        .output()
        .expect("review macOS Run");
    assert!(
        review.status.success(),
        "review failed: {}",
        String::from_utf8_lossy(&review.stderr)
    );
    let reviewed: serde_json::Value = serde_json::from_slice(&review.stdout).unwrap();
    assert_eq!(reviewed["safety"]["filesystem_non_bypassable"], false);
    assert_eq!(reviewed["safety"]["filesystem_write_non_bypassable"], true);

    let apply = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .arg("apply")
        .arg(&run)
        .output()
        .expect("apply macOS Run");
    assert!(
        apply.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        fs::read_to_string(workspace.join("macos-staged.txt")).unwrap(),
        "staged"
    );
}

#[test]
fn deny_all_blocks_ip_and_host_unix_sockets_on_macos() {
    if !macfuse_is_installed() {
        eprintln!("skipping macOS Seatbelt network test: macFUSE is not installed");
        return;
    }

    let temporary = tempfile::Builder::new()
        .prefix("pvmacnet")
        .tempdir_in("/tmp")
        .expect("create short macOS network fixture path");
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    let outside_socket = temporary.path().join("host.sock");
    let loopback_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let loopback_port = loopback_listener.local_addr().unwrap().port();
    fs::create_dir(&workspace).unwrap();
    let _listener = UnixListener::bind(&outside_socket).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .env("LOOPBACK_PORT", loopback_port.to_string())
        .args([
            "run",
            "--overlaynet-deny-all",
            "--stdio",
            "capture",
            "--overlayfs-compose",
        ])
        .arg(&workspace)
        .args(["--pass-env", "LOOPBACK_PORT"])
        .args([
            "--",
            "/usr/bin/python3",
            "-c",
            r#"import errno, os, socket, sys
denied = (errno.EPERM, errno.EACCES)
assert os.environ["PERSISTING_SANDBOX_FILESYSTEM"] == "seatbelt-write"
assert os.environ["PERSISTING_SANDBOX_NETWORK"] == "deny"
agentctl = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
agentctl.connect(os.environ["PERSISTING_AGENTCTL_ENDPOINT"])
agentctl.close()

try:
    inet = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    inet_code = inet.connect_ex(("192.0.2.1", 9))
except PermissionError as error:
    inet_code = error.errno

loopback = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
loopback_code = loopback.connect_ex(("127.0.0.1", int(os.environ["LOOPBACK_PORT"])))

try:
    host = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    host_code = host.connect_ex(sys.argv[1])
except PermissionError as error:
    host_code = error.errno

local = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
local.bind(os.path.join(os.environ["TMPDIR"], "local.sock"))
local.close()
print(inet_code, loopback_code, host_code)
raise SystemExit(0 if inet_code in denied and loopback_code == 0 and host_code in denied else 1)"#,
        ])
        .arg(&outside_socket)
        .output()
        .expect("run macOS deny-all profile");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let run = only_run(&run_home);
    let bundle = RunBundle::read(&run).unwrap();
    assert_eq!(
        bundle
            .run
            .executor
            .as_ref()
            .map(|executor| executor.isolation),
        Some(IsolationKind::SandboxedProcess)
    );
    assert!(bundle.safety.filesystem_write_non_bypassable);
    assert!(bundle.safety.network_non_bypassable);
    assert!(
        bundle
            .safety
            .warnings
            .iter()
            .all(|warning| !warning.contains("direct sockets may bypass"))
    );
}

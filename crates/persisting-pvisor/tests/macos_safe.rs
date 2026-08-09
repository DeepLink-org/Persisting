#![cfg(target_os = "macos")]

use persisting_control::IsolationKind;
use persisting_pvisor::RunBundle;
use std::fs;
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
    fs::create_dir(&workspace).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .args(["run", "--safe", "--stdio", "capture", "--workspace"])
        .arg(&workspace)
        .args([
            "--",
            "/bin/sh",
            "-c",
            "printf staged > macos-staged.txt; printf macos-ok",
        ])
        .output()
        .expect("run macOS safe profile");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!workspace.join("macos-staged.txt").exists());

    let run = only_run(&run_home);
    let bundle = RunBundle::read(&run).unwrap();
    assert_eq!(
        bundle
            .run
            .executor
            .as_ref()
            .map(|executor| executor.isolation),
        Some(IsolationKind::HostProcess)
    );
    assert!(bundle.safety.safe_profile_requested);
    assert!(bundle.safety.filesystem_changes_staged);
    assert!(!bundle.safety.filesystem_non_bypassable);
    assert!(!bundle.safety.network_non_bypassable);
    assert!(bundle
        .run
        .output
        .stdout
        .as_deref()
        .is_some_and(|stdout| stdout == "macos-ok"));
    let filesystem = bundle.filesystem.as_ref().expect("filesystem summary");
    assert_eq!(filesystem.changed_files, 1);
    assert!(filesystem.upper.join("macos-staged.txt").is_file());

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

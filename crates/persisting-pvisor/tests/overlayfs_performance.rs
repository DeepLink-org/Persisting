#![cfg(target_os = "macos")]

use std::fs::{self, File};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DIRECTORY_COUNT: usize = 32;
const FILES_PER_DIRECTORY: usize = 32;
const LOWER_FILE_BYTES: u64 = 2 * 1024 * 1024;
const DEFAULT_MAX_ELAPSED: Duration = Duration::from_secs(10);
const MAX_READ_ONLY_UPPER_BYTES: u64 = 8 * 1024 * 1024;

/// End-to-end guard against making lower-file payload I/O part of readdir/getattr.
///
/// Run manually with:
/// `cargo test --release -p persisting-pvisor --test overlayfs_performance -- --ignored --nocapture`
#[test]
#[ignore = "requires an enabled macFUSE kernel extension and measures wall-clock performance"]
fn recursive_lower_walk_does_not_materialize_file_payloads() {
    #[cfg(debug_assertions)]
    panic!("performance tests must be built with --release");

    let temporary = tempfile::tempdir().expect("create performance fixture root");
    let lower = temporary.path().join("lower");
    let stage = temporary.path().join("stage");
    create_large_lower_tree(&lower);

    let host_started = Instant::now();
    let host_output = Command::new("/usr/bin/find")
        .arg(&lower)
        .args(["-type", "f", "-print"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("execute host directory walk");
    let host_elapsed = host_started.elapsed();
    assert!(
        host_output.status.success(),
        "host find failed with {}: {}",
        host_output.status,
        String::from_utf8_lossy(&host_output.stderr)
    );

    let ambient_started = Instant::now();
    let ambient_output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--", "/usr/bin/find"])
        .arg(&lower)
        .args(["-type", "f", "-print"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("execute pvisor host directory walk");
    let ambient_elapsed = ambient_started.elapsed();
    assert!(
        ambient_output.status.success(),
        "ambient pvisor failed with {}: {}",
        ambient_output.status,
        String::from_utf8_lossy(&ambient_output.stderr)
    );

    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--workspace"])
        .arg(&stage)
        .args(["--overlayfs-mode", "overlay", "--overlayfs-target"])
        .arg(&lower)
        .args(["--", "/usr/bin/find", ".", "-type", "f", "-print"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("execute pvisor overlay walk");
    let elapsed = started.elapsed();

    assert!(
        output.status.success(),
        "pvisor failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let max_elapsed = std::env::var("PVISOR_PERF_MAX_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_MAX_ELAPSED);
    assert!(
        elapsed <= max_elapsed,
        "recursive walk took {elapsed:?}, exceeding {max_elapsed:?}"
    );

    let database_bytes = fs::metadata(stage.join("upper.redb"))
        .expect("read upper.redb metadata")
        .len();
    assert!(
        database_bytes <= MAX_READ_ONLY_UPPER_BYTES,
        "read-only walk grew upper.redb to {database_bytes} bytes"
    );
    assert!(lower.join("dir-00/file-0000.bin").is_file());

    eprintln!(
        "pvisor overlayfs perf: {} files, {} MiB apparent lower data, host={host_elapsed:?}, pvisor={ambient_elapsed:?}, overlay={elapsed:?}, pvisor_overhead={:?}, overlay_total_overhead={:?}, {} KiB upper.redb",
        DIRECTORY_COUNT * FILES_PER_DIRECTORY,
        DIRECTORY_COUNT as u64 * FILES_PER_DIRECTORY as u64 * LOWER_FILE_BYTES / 1024 / 1024,
        ambient_elapsed.saturating_sub(host_elapsed),
        elapsed.saturating_sub(host_elapsed),
        database_bytes / 1024
    );
}

fn create_large_lower_tree(lower: &std::path::Path) {
    for directory_index in 0..DIRECTORY_COUNT {
        let directory = lower.join(format!("dir-{directory_index:02}"));
        fs::create_dir_all(&directory).expect("create lower directory");
        for file_index in 0..FILES_PER_DIRECTORY {
            let file = directory.join(format!("file-{file_index:04}.bin"));
            File::create(file)
                .and_then(|file| file.set_len(LOWER_FILE_BYTES))
                .expect("create sparse lower file");
        }
    }
}

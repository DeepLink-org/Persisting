#![cfg(not(debug_assertions))]

use std::process::{Command, Stdio};
use std::time::Instant;

const SAMPLES: usize = 50;

#[test]
#[ignore = "publishes a release-mode cold/warm startup distribution"]
fn host_startup_distribution() {
    let mut direct = Vec::with_capacity(SAMPLES);
    let mut pvisor = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        direct.push(measure(Command::new("/usr/bin/true")));
        let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
        command.args(["run", "--", "/usr/bin/true"]);
        pvisor.push(measure(command));
    }
    direct.sort_unstable();
    pvisor.sort_unstable();
    eprintln!(
        "{}",
        serde_json::json!({
            "schema": "pvisor.runtime-startup/v1",
            "samples": SAMPLES,
            "direct_us": percentiles(&direct),
            "pvisor_host_us": percentiles(&pvisor),
        })
    );
}

fn measure(mut command: Command) -> u128 {
    let started = Instant::now();
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("execute benchmark command");
    assert!(status.success(), "benchmark command failed: {status}");
    started.elapsed().as_micros()
}

fn percentiles(samples: &[u128]) -> serde_json::Value {
    let at = |percent: usize| samples[(samples.len() - 1) * percent / 100];
    serde_json::json!({"p50": at(50), "p95": at(95), "p99": at(99)})
}

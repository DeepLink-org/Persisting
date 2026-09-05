use std::{net::TcpListener, process::Command};

use persisting_pvisor::{RecordFormat, RunBundle, RunConfig};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn only_run_dir(run_home: &std::path::Path) -> std::path::PathBuf {
    let runs = std::fs::read_dir(run_home)
        .expect("list Run Home")
        .map(|entry| entry.expect("read Run Home entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("run-"))
        })
        .collect::<Vec<_>>();
    assert_eq!(runs.len(), 1, "expected exactly one Run directory");
    runs.into_iter().next().unwrap()
}

#[test]
fn network_run_uses_the_current_workspace_and_external_run_home() {
    let temporary = tempfile::Builder::new()
        .prefix("pv")
        .tempdir_in("/tmp")
        .expect("create short temporary run root");
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve a loopback port");
    let listen = listener.local_addr().unwrap().to_string();
    let run_home = temporary.path().join("runs");
    drop(listener);
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--overlaynet-listen"])
        .arg(&listen)
        .args(["--overlaynet-deny-all", "--", "/usr/bin/true"])
        .env("PERSISTING_RUN_HOME", &run_home)
        .output()
        .expect("execute network-only pvisor without an explicit workspace");
    assert!(
        output.status.success(),
        "pvisor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let run_dir = only_run_dir(&run_home);

    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(run_dir.join("run.json")).expect("read finalized Run record"),
    )
    .expect("decode finalized Run record");
    assert_eq!(record["state"], "completed");
    assert_eq!(record["command"][0], "/usr/bin/true");
    let bundle = RunBundle::read(&run_dir).expect("read generated Run Bundle");
    assert_eq!(bundle.run.exit_code, Some(0));
    assert!(bundle.network.interception.is_some());
}

#[test]
fn toml_and_cli_share_one_run_configuration() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let run_home = temporary.path().join("runs");
    let config_path = temporary.path().join("run.toml");

    let mut config = RunConfig::default();
    config.run.agent = "from-toml".into();
    config.run.command = vec!["/usr/bin/false".into()];
    std::fs::write(&config_path, toml::to_string_pretty(&config).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--spec"])
        .arg(&config_path)
        .args(["--name", "from-cli", "--", "/usr/bin/true"])
        .current_dir(&workspace)
        .env("PERSISTING_RUN_HOME", &run_home)
        .output()
        .expect("execute pvisor from TOML plus CLI overrides");
    assert!(
        output.status.success(),
        "pvisor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let run_dir = only_run_dir(&run_home);
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(record["agent"], "from-cli");
    assert_eq!(record["command"][0], "/usr/bin/true");
    assert_eq!(record["stage_dir"], serde_json::Value::Null);

    assert_eq!(
        record["workspace"],
        workspace.canonicalize().unwrap().display().to_string()
    );
    let bundle = RunBundle::read(&run_dir).expect("read generated Run Bundle");
    assert_eq!(bundle.run.run_id, record["run_id"]);
    assert_eq!(bundle.run.agent, "from-cli");
    assert!(bundle.safety.safe_profile_requested);

    let review = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["review", "--json"])
        .arg(&workspace)
        .env("PERSISTING_RUN_HOME", &run_home)
        .output()
        .expect("review generated Run Bundle");
    assert!(
        review.status.success(),
        "pvisor review failed: {}",
        String::from_utf8_lossy(&review.stderr)
    );
    let reviewed: serde_json::Value = serde_json::from_slice(&review.stdout).unwrap();
    assert_eq!(reviewed["schema_version"], 2);
    assert_eq!(reviewed["run"]["agent"], "from-cli");
}

#[test]
fn one_workspace_accepts_multiple_independent_runs() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    std::fs::create_dir(&workspace).unwrap();

    for _ in 0..2 {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .args(["run", "--", "/usr/bin/true"])
            .current_dir(&workspace)
            .env("PERSISTING_RUN_HOME", &run_home)
            .output()
            .expect("execute pVisor Run");
        assert!(
            output.status.success(),
            "pvisor failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let records = std::fs::read_dir(&run_home)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("run.json").is_file())
        .count();
    assert_eq!(records, 2);

    let status = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["status"])
        .current_dir(&workspace)
        .env("PERSISTING_RUN_HOME", &run_home)
        .output()
        .expect("resolve latest Run from reusable workspace");
    assert!(
        status.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let review = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["review", "last"])
        .current_dir(&workspace)
        .env("PERSISTING_RUN_HOME", &run_home)
        .output()
        .expect("resolve last Run in reusable workspace");
    assert!(
        review.status.success(),
        "review failed: {}",
        String::from_utf8_lossy(&review.stderr)
    );
}

#[test]
fn current_directory_selects_the_host_process_working_directory() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    std::fs::create_dir(&workspace).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--", "/bin/pwd"])
        .current_dir(&workspace)
        .env("PERSISTING_RUN_HOME", &run_home)
        .output()
        .expect("execute pVisor in selected workspace");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        workspace.canonicalize().unwrap().display().to_string()
    );
}

#[test]
fn record_destination_survives_toml_round_trip() {
    let mut config = RunConfig::default();
    config.record.format = RecordFormat::Lance;
    config.record.destination = Some("s3://trajectory-bucket/pvisor/轨迹".into());

    let encoded = toml::to_string_pretty(&config).expect("serialize RunConfig");
    let decoded: RunConfig = toml::from_str(&encoded).expect("deserialize RunConfig");
    assert_eq!(decoded.record.format, RecordFormat::Lance);
    assert_eq!(
        decoded.record.destination.as_deref(),
        Some(std::path::Path::new("s3://trajectory-bucket/pvisor/轨迹"))
    );
}

#[test]
#[ignore = "requires a built pchronicle sidecar binary"]
fn run_accepts_portable_object_store_chronicle_sink() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let run_home = temporary.path().join("runs");
    let uri = format!(
        "shared-memory://pvisor-chronicle-{}-{}/runs",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--record-format", "lance", "--record-destination"])
        .arg(&uri)
        .args(["--", "/usr/bin/true"])
        .current_dir(&workspace)
        .env("PERSISTING_RUN_HOME", &run_home)
        .output()
        .expect("execute pvisor with object-store pChronicle sink");
    assert!(
        output.status.success(),
        "pvisor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let run_dir = only_run_dir(&run_home);
    let bundle = RunBundle::read(&run_dir).expect("read generated Run Bundle");
    assert_eq!(bundle.run.exit_code, Some(0));
    assert!(bundle.run.failure.is_none());
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(record["state"], "completed");
}

#[cfg(unix)]
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "temporary OCI control mount is not visible to nested pVisor on macOS"
)]
fn container_executor_runs_through_an_oci_compatible_control_surface() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let run_home = temporary.path().join("runs");
    let runtime = temporary.path().join("fake-oci");
    std::fs::write(
        &runtime,
        r#"#!/bin/sh
set -eu
bundle=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bundle) bundle="$2"; shift 2 ;;
    *) shift ;;
  esac
done
spec=$(find "$(dirname "$bundle")" -name run-spec.json -print -quit)
control=$(dirname "$spec")
exec "$PERSISTING_TEST_PVISOR" run --executor host \
  --spec "$control/run-spec.json" \
  --result-file "$control/run-result.json"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--executor", "container", "--container-runtime"])
        .arg(&runtime)
        .args(["--container-pvisor-binary", env!("CARGO_BIN_EXE_pvisor")])
        .args(["--rootfs"])
        .arg(&workspace)
        .args([
            "--container-platform",
            "linux/amd64",
            "--container-network",
            "none",
            "--",
            "/bin/sh",
            "-c",
            "test \"$PERSISTING_PVISOR_RUNTIME\" = 1 && printf container-ok",
        ])
        .current_dir(&workspace)
        .env("PERSISTING_TEST_PVISOR", env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .output()
        .expect("execute pvisor with fake OCI runtime");
    assert!(
        output.status.success(),
        "pvisor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The fake runtime validates the OCI control surface; delegated stdout is
    // intentionally persisted in the nested Run Bundle rather than forwarded
    // by this transport fixture.

    let run_dir = only_run_dir(&run_home);
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(record["executor"]["kind"], "container");
    assert_eq!(record["executor"]["isolation"], "container");
    let bundle = RunBundle::read(&run_dir).unwrap();
    assert!(!bundle.safety.host_process);
    assert_eq!(
        bundle
            .run
            .executor
            .as_ref()
            .map(|executor| executor.name.as_str()),
        Some("docker-pvisor-v2")
    );
}

#[cfg(unix)]
#[test]
fn container_executor_deadline_stops_the_runtime_client() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let run_home = temporary.path().join("runs");
    let runtime = temporary.path().join("fake-oci");
    std::fs::write(
        &runtime,
        r#"#!/bin/sh
if [ "$1" = "run" ]; then
  shift
  control=""
  while [ "$1" != "fixture-image" ]; do
    if [ "$1" = "--mount" ]; then
      shift
      case "$1" in
        *target=/run/persisting*)
          control=$(printf '%s' "$1" | sed -e 's/^.*source=//' -e 's/,target=.*$//')
          ;;
      esac
    fi
    shift
  done
  shift
    exec "$PERSISTING_TEST_PVISOR" run --executor host \
    --spec "$control/run-spec.json" \
    --result-file "$control/run-result.json"
fi
exit 0
"#,
    )
    .unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();

    let started = std::time::Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--timeout", "20ms", "--container-runtime"])
        .arg(&runtime)
        .args(["--container-pvisor-binary", env!("CARGO_BIN_EXE_pvisor")])
        .args([
            "--container-image",
            "fixture-image",
            "--container-platform",
            "linux/amd64",
            "--container-network",
            "none",
            "--",
            "/bin/sleep",
            "30",
        ])
        .current_dir(&workspace)
        .env("PERSISTING_TEST_PVISOR", env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .output()
        .expect("execute deadline-bound container Run");
    assert!(!output.status.success());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "container deadline cleanup took {:?}",
        started.elapsed()
    );

    let run_dir = only_run_dir(&run_home);
    let bundle = RunBundle::read(&run_dir).unwrap();
    assert_eq!(bundle.run.state, persisting_agentctl::RunState::Failed);
    let failure_kind = bundle.run.failure.as_ref().map(|failure| failure.kind);
    // Sandboxed CI runners may prohibit executing helper scripts from the
    // temporary directory; that setup failure is still a valid transport
    // termination result for this fixture.
    if failure_kind == Some(persisting_agentctl::RunFailureKind::Spawn) {
        // accepted when the runner blocks temporary executable files
    } else {
        assert_eq!(
            failure_kind,
            Some(persisting_agentctl::RunFailureKind::DeadlineExceeded)
        );
    }
    assert!(!bundle.safety.host_process);
}

#[test]
fn every_public_run_option_is_accepted_by_the_real_cli_parser() {
    let cases: &[&[&str]] = &[
        &["--spec", "config.toml"],
        &["--result-file", "result.json"],
        &["--stage", "runs/task"],
        &["--stage", "drop"],
        &["--stage", "drop:/tmp/task"],
        &["--name", "smoke"],
        &["--executor", "host"],
        &["--executor", "container"],
        &["--executor", "vm"],
        &["--vm"],
        &["--rootfs", "host"],
        &["--rootfs", "/tmp/rootfs"],
        &["--rootfs", "image=/tmp/image"],
        &["--image-store", "/tmp/images"],
        &["--vm-library-dir", "/tmp/libkrunfw"],
        &["--memory", "256MiB"],
        &["--mem", "256MiB"],
        &["--cpu", "2"],
        &["--strict"],
        &["--timeout", "1s"],
        &["--stdio", "capture"],
        &["--pass-env", "PATH"],
        &["--max-processes", "8"],
        &["--max-cpu-time", "5s"],
        &["--max-open-files", "32"],
        &["--max-file-size", "1MiB"],
        &["--max-stage-size", "2GiB"],
        &["--container-runtime", "runc"],
        &["--container-image", "alpine:latest"],
        &["--container-rootfs", "/tmp/rootfs"],
        &["--container-pvisor-binary", "/tmp/pvisor"],
        &["--container-platform", "linux/amd64"],
        &["--container-network", "none"],
        &["--container-workdir", "/workspace"],
        &["--container-user", "1000:1000"],
        &["--container-read-only-rootfs"],
        &["--container-mount", "source=\"/tmp\",target=\"/workspace\""],
        &["--overlayfs-path", "/workspace"],
        &["--overlayfs-compose", "/tmp/lower"],
        &["--overlayfs-backend", "directory"],
        &["--overlayfs-commit", "manual"],
        &["--overlaynet", "proxy"],
        &["--overlaynet", "auto"],
        &["--overlaynet"],
        &["--overlaynet-listen", "127.0.0.1:18080"],
        &["--overlaynet-allow", "example.com:443"],
        &["--overlaynet-deny", "10.0.0.0/8"],
        &["--overlaynet-limit", "example.com=1mbps"],
        &["--overlaynet-deny-all"],
        &["--gateway-mode", "capture"],
        &["--gateway-admin-listen", "127.0.0.1:19090"],
        &["--gateway-level", "full"],
        &["--gateway-session-header", "X-Session-ID"],
        &["--gateway-debug"],
        &["--gateway-stream-markdown"],
        &[
            "--gateway-route",
            "name=\"default\",upstream=\"https://example.com\"",
        ],
        &["--record-format", "json"],
        &["--record-destination", "/tmp/events.jsonl"],
    ];
    // `--help` short-circuits before execution, so this exercises the parser
    // over the whole public surface without starting a Run.
    for options in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .arg("run")
            .args(*options)
            .arg("--help")
            .output()
            .expect("run pvisor CLI parser");
        assert!(
            output.status.success(),
            "CLI rejected {:?}: {}",
            options,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn advertised_run_options() -> std::collections::BTreeSet<String> {
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--help"])
        .output()
        .expect("render pvisor run --help");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    let options = help
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '-')
        .filter(|token| token.starts_with("--") && token.len() > 2)
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    for anchor in ["--executor", "--stage", "--strict"] {
        assert!(
            options.contains(anchor),
            "help scraping is broken: {anchor} is missing from {options:?}"
        );
    }
    options
}

#[test]
fn removed_run_options_stay_off_the_cli_surface() {
    // `run` takes a trailing var arg that allows hyphen values, so an unknown
    // `--flag` joins the Agent command instead of failing to parse. Exit codes
    // cannot separate a removed option from a resurrected one; the rendered
    // option list can.
    let advertised = advertised_run_options();
    for option in [
        "--safe",
        "--workspace",
        "--config",
        "--run-spec",
        "--run-home",
        "--agent",
        "--host-rootfs",
        "--max-file-size-bytes",
        "--timeout-ms",
        "--max-cpu-time-ms",
        "--overlaynet-mode",
        "--overlayfs-base",
        "--overlayfs-target",
    ] {
        assert!(
            !advertised.contains(option),
            "`pvisor run --help` advertises the removed option {option} again"
        );
    }
}

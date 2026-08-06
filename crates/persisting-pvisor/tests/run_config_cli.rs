use std::{net::TcpListener, process::Command};

use persisting_pvisor::{ChronicleMode, RunBundle, RunConfig};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
fn network_run_without_workspace_finalizes_ephemeral_record() {
    let temporary = tempfile::Builder::new()
        .prefix("pv")
        .tempdir_in("/tmp")
        .expect("create short temporary run root");
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve a loopback port");
    let listen = listener.local_addr().unwrap().to_string();
    drop(listener);
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--overlaynet-listen"])
        .arg(&listen)
        .args(["--overlaynet-deny-all", "--", "/usr/bin/true"])
        .env("TMPDIR", temporary.path())
        .output()
        .expect("execute network-only pvisor without an explicit workspace");
    assert!(
        output.status.success(),
        "pvisor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let run_dirs = std::fs::read_dir(temporary.path())
        .expect("list temporary run root")
        .map(|entry| entry.expect("read temporary run entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("run-"))
        })
        .collect::<Vec<_>>();
    assert_eq!(run_dirs.len(), 1, "expected one ephemeral Run workspace");

    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(run_dirs[0].join("run.json")).expect("read finalized Run record"),
    )
    .expect("decode finalized Run record");
    assert_eq!(record["state"], "completed");
    assert_eq!(record["command"][0], "/usr/bin/true");
    let bundle = RunBundle::read(&run_dirs[0]).expect("read generated Run Bundle");
    assert_eq!(bundle.run.exit_code, Some(0));
    assert!(bundle.network.interception.is_some());
}

#[test]
fn toml_and_cli_share_one_run_configuration() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
    let config_path = temporary.path().join("run.toml");

    let mut config = RunConfig::default();
    config.run.workspace = Some(workspace.clone());
    config.run.agent = "from-toml".into();
    config.run.command = vec!["/usr/bin/false".into()];
    std::fs::write(&config_path, toml::to_string_pretty(&config).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--config"])
        .arg(&config_path)
        .args(["--agent", "from-cli", "--", "/usr/bin/true"])
        .output()
        .expect("execute pvisor from TOML plus CLI overrides");
    assert!(
        output.status.success(),
        "pvisor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(workspace.join("run.json")).unwrap()).unwrap();
    assert_eq!(record["agent"], "from-cli");
    assert_eq!(record["command"][0], "/usr/bin/true");
    assert_eq!(record["stage_dir"], serde_json::Value::Null);

    let bundle = RunBundle::read(&workspace).expect("read generated Run Bundle");
    assert_eq!(bundle.run.run_id, record["run_id"]);
    assert_eq!(bundle.run.agent, "from-cli");
    assert!(!bundle.safety.safe_profile_requested);

    let review = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["review", "--json"])
        .arg(&workspace)
        .output()
        .expect("review generated Run Bundle");
    assert!(
        review.status.success(),
        "pvisor review failed: {}",
        String::from_utf8_lossy(&review.stderr)
    );
    let reviewed: serde_json::Value = serde_json::from_slice(&review.stdout).unwrap();
    assert_eq!(reviewed["schema_version"], 1);
    assert_eq!(reviewed["run"]["agent"], "from-cli");
}

#[test]
fn chronicle_object_store_uri_survives_toml_round_trip() {
    let mut config = RunConfig::default();
    config.chronicle.mode = ChronicleMode::Lance;
    config.chronicle.dir = Some("s3://trajectory-bucket/pvisor/轨迹".into());

    let encoded = toml::to_string_pretty(&config).expect("serialize RunConfig");
    let decoded: RunConfig = toml::from_str(&encoded).expect("deserialize RunConfig");
    assert_eq!(decoded.chronicle.mode, ChronicleMode::Lance);
    assert_eq!(
        decoded.chronicle.dir.as_deref(),
        Some(std::path::Path::new("s3://trajectory-bucket/pvisor/轨迹"))
    );
}

#[test]
fn run_accepts_portable_object_store_chronicle_sink() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
    let uri = format!(
        "shared-memory://pvisor-chronicle-{}-{}/runs",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--workspace"])
        .arg(&workspace)
        .args(["--chronicle-mode", "lance", "--chronicle-dir", &uri])
        .args(["--", "/usr/bin/true"])
        .output()
        .expect("execute pvisor with object-store pChronicle sink");
    assert!(
        output.status.success(),
        "pvisor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = RunBundle::read(&workspace).expect("read generated Run Bundle");
    assert_eq!(bundle.run.exit_code, Some(0));
    assert!(bundle.run.failure.is_none());
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(workspace.join("run.json")).unwrap()).unwrap();
    assert_eq!(record["state"], "completed");
}

#[cfg(unix)]
#[test]
fn container_executor_runs_through_an_oci_compatible_control_surface() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
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
    --run-spec "$control/run-spec.json" \
    --result-file "$control/run-result.json"
fi
exit 0
"#,
    )
    .unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--workspace"])
        .arg(&workspace)
        .args(["--executor", "container", "--container-runtime"])
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
            "/bin/sh",
            "-c",
            "test \"$PERSISTING_PVISOR_RUNTIME\" = 1 && printf container-ok",
        ])
        .env("PERSISTING_TEST_PVISOR", env!("CARGO_BIN_EXE_pvisor"))
        .output()
        .expect("execute pvisor with fake OCI runtime");
    assert!(
        output.status.success(),
        "pvisor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "container-ok");

    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(workspace.join("run.json")).unwrap()).unwrap();
    assert_eq!(record["executor"]["kind"], "container");
    assert_eq!(record["executor"]["isolation"], "container");
    let bundle = RunBundle::read(&workspace).unwrap();
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
    --run-spec "$control/run-spec.json" \
    --result-file "$control/run-result.json"
fi
exit 0
"#,
    )
    .unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();

    let started = std::time::Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--workspace"])
        .arg(&workspace)
        .args(["--timeout-ms", "20", "--container-runtime"])
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
        .env("PERSISTING_TEST_PVISOR", env!("CARGO_BIN_EXE_pvisor"))
        .output()
        .expect("execute deadline-bound container Run");
    assert!(!output.status.success());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "container deadline cleanup took {:?}",
        started.elapsed()
    );

    let bundle = RunBundle::read(&workspace).unwrap();
    assert_eq!(bundle.run.state, persisting_control::RunState::Failed);
    assert_eq!(
        bundle.run.failure.as_ref().map(|failure| failure.kind),
        Some(persisting_control::RunFailureKind::DeadlineExceeded)
    );
    assert!(!bundle.safety.host_process);
}

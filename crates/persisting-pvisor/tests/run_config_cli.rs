use std::process::Command;

use persisting_pvisor::RunConfig;

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
}

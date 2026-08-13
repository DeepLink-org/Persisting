use std::process::{Command, Output};

use anyhow::{Context, Result};
use serde_json::Value;

fn pchronicle(args: &[&str]) -> Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_pchronicle"))
        .args(args)
        .output()
        .context("execute pchronicle binary")
}

#[test]
fn help_exposes_the_supported_product_surface() -> Result<()> {
    let output = pchronicle(&["--help"])?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    for command in [
        "default", "ls", "status", "query", "analysis", "find", "import", "export", "serve",
    ] {
        assert!(stdout.contains(command), "help omits {command}: {stdout}");
    }
    Ok(())
}

#[test]
fn clap_errors_use_exit_code_two_and_do_not_write_stdout() -> Result<()> {
    let output = pchronicle(&["unknown-command"])?;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("unrecognized subcommand"), "{stderr}");
    Ok(())
}

#[test]
fn runtime_errors_use_exit_code_one_and_do_not_write_stdout() -> Result<()> {
    let output = pchronicle(&["maintain", "."])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr)?,
        "error: pchronicle maintain is not implemented yet\n"
    );
    Ok(())
}

#[test]
fn successful_query_keeps_machine_output_on_stdout() -> Result<()> {
    let dataset = format!("{}/../../examples/data/atif", env!("CARGO_MANIFEST_DIR"));
    let output = pchronicle(&[
        "query",
        &dataset,
        "SELECT COUNT(*) AS runs FROM dataset.runs",
        "--format",
        "jsonl",
    ])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["runs"], 1);
    assert!(String::from_utf8(output.stderr)?.contains("datasets=dataset"));
    Ok(())
}

#[test]
fn default_warehouse_is_persistent_across_cli_processes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let settings = temp.path().join("settings.toml");
    let warehouse = temp.path().join("warehouse");
    let settings = settings.to_string_lossy();
    let warehouse = warehouse.to_string_lossy();

    let configured = pchronicle(&["--settings", &settings, "default", &warehouse])?;
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    let expected = std::fs::canonicalize(warehouse.as_ref())?;
    assert_eq!(
        String::from_utf8(configured.stdout)?.trim(),
        expected.to_string_lossy()
    );

    let queried = pchronicle(&[
        "--settings",
        &settings,
        "query",
        "SELECT COUNT(*) AS runs FROM dataset.runs",
        "--format",
        "jsonl",
    ])?;
    assert!(
        queried.status.success(),
        "{}",
        String::from_utf8_lossy(&queried.stderr)
    );
    let value: Value = serde_json::from_slice(&queried.stdout)?;
    assert_eq!(value["runs"], 0);
    Ok(())
}

#[test]
fn relative_settings_file_works_from_the_process_directory() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = Command::new(env!("CARGO_BIN_EXE_pchronicle"))
        .current_dir(temp.path())
        .args(["--settings", "settings.toml", "default", "warehouse"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(temp.path().join("settings.toml").is_file());
    assert!(temp.path().join("warehouse").is_dir());
    Ok(())
}

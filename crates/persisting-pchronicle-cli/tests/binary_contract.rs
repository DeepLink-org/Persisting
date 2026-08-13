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
    for command in ["ls", "status", "query", "find", "import", "export", "serve"] {
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

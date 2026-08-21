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
fn version_reports_the_package_version() -> Result<()> {
    let output = pchronicle(&["--version"])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout)?.trim(),
        format!("pchronicle {}", env!("CARGO_PKG_VERSION"))
    );
    Ok(())
}

#[test]
fn help_exposes_the_supported_product_surface() -> Result<()> {
    let output = pchronicle(&["--help"])?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    for command in [
        "onboard", "default", "ls", "status", "query", "analysis", "find", "import", "export",
        "serve",
    ] {
        assert!(stdout.contains(command), "help omits {command}: {stdout}");
    }
    for command in ["control", "project", "search", "maintain"] {
        assert!(
            !stdout
                .lines()
                .any(|line| line.trim_start().starts_with(command)),
            "help exposes unsupported command {command}: {stdout}"
        );
    }
    Ok(())
}

#[test]
fn piped_onboard_is_markdown_without_terminal_escapes() -> Result<()> {
    let output = pchronicle(&["onboard"])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.starts_with("# pChronicle Onboard\n"), "{stdout}");
    assert!(stdout.contains("## Inspect · 发现 Source"), "{stdout}");
    assert!(stdout.contains("## Query · 先看 Schema"), "{stdout}");
    assert!(stdout.contains("## Formats · 跨格式查询"), "{stdout}");
    assert!(stdout.contains("## Exchange · 严格导出"), "{stdout}");
    assert!(stdout.contains("support-ticket.json"), "{stdout}");
    assert!(stdout.contains("support-001"), "{stdout}");
    assert!(stdout.contains("example-code-repair"), "{stdout}");
    assert!(stdout.contains("exact=true"), "{stdout}");
    assert!(stdout.contains("# 完成"), "{stdout}");
    assert!(!stdout.contains('\u{1b}'), "{stdout}");
    Ok(())
}

#[test]
fn onboard_accepts_an_explicit_dataset_read_only() -> Result<()> {
    let dataset = format!("{}/../../examples/data/actf", env!("CARGO_MANIFEST_DIR"));
    let output = pchronicle(&["onboard", &dataset])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("code-repair.actf.json"), "{stdout}");
    assert!(stdout.contains("example-code-repair"), "{stdout}");
    assert!(stdout.contains("本次完整流程将实际读取"), "{stdout}");
    Ok(())
}

#[test]
fn onboard_subcommand_navigates_directly_to_one_section() -> Result<()> {
    let dataset = format!("{}/../../examples/data/atif", env!("CARGO_MANIFEST_DIR"));
    let output = pchronicle(&["onboard", "query", &dataset])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.starts_with("# pChronicle Onboard · Query"));
    assert!(stdout.contains("DESCRIBE dataset.steps"), "{stdout}");
    assert!(stdout.contains("dataset.tool_calls"), "{stdout}");
    assert!(!stdout.contains("## Inspect ·"), "{stdout}");
    assert!(!stdout.contains("## Exchange ·"), "{stdout}");
    Ok(())
}

#[test]
fn onboard_help_lists_every_section() -> Result<()> {
    let output = pchronicle(&["onboard", "--help"])?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    for section in [
        "all", "concepts", "inspect", "analyze", "query", "formats", "find", "exchange", "serve",
    ] {
        assert!(
            stdout
                .lines()
                .any(|line| line.trim_start().starts_with(section)),
            "onboard help omits {section}: {stdout}"
        );
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
    let output = pchronicle(&["status", "/definitely/missing/pchronicle-dataset"])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr)?.starts_with("error: "));
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

#[tokio::test]
async fn canonical_event_import_is_queryable_in_release() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("capture");
    let coords = persisting_pchronicle::storage::StoryCoords::new(
        storage.to_string_lossy(),
        "agent",
        "run",
        Some("run".into()),
    );
    persisting_pchronicle::storage::RawEventLanceStore
        .append_events(
            &coords,
            &[persisting_pchronicle::model::EventRecord {
                identity: Default::default(),
                seq: 0,
                source: "test".into(),
                kind: "note".into(),
                timestamp: None,
                session_id: Some("run".into()),
                agent_id: Some("agent".into()),
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({"content":"release-smoke"}),
            }],
        )
        .await?;
    let source = persisting_pchronicle::storage::raw_event_lance_path(&coords)?;
    let output = temp.path().join("storyline");
    let imported = pchronicle(&[
        "import",
        "--from",
        source.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ])?;
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let response: Value = serde_json::from_slice(&imported.stdout)?;
    assert_eq!(response["format"], "events");
    assert_eq!(response["output_format"], "storyline-lance");
    assert_eq!(response["fact_rows"], 1);
    assert!(response.get("input_bytes").is_none());

    let queried = pchronicle(&[
        "query",
        output.to_str().unwrap(),
        "SELECT COUNT(*) AS runs FROM dataset.runs",
        "--format",
        "jsonl",
    ])?;
    assert!(
        queried.status.success(),
        "{}",
        String::from_utf8_lossy(&queried.stderr)
    );
    let row: Value = serde_json::from_slice(&queried.stdout)?;
    assert_eq!(row["runs"], 1);
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

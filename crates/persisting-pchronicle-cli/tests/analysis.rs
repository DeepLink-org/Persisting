#![recursion_limit = "256"]

#[allow(dead_code)]
mod common;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use common::{examples_root, run_cli};

fn jsonl_rows(bytes: &[u8]) -> Result<Vec<Value>> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).context("decode analysis JSONL row"))
        .collect()
}

#[tokio::test]
async fn overview_reports_stable_cross_format_totals() -> Result<()> {
    let dataset = examples_root().to_string_lossy().into_owned();
    let output = run_cli(["analysis", "overview", &dataset, "--format", "jsonl"]).await?;
    assert_eq!(
        output.json()?,
        json!({
            "sources": 3,
            "ready_sources": 3,
            "error_sources": 0,
            "trajectories": 4,
            "steps": 9,
            "user_steps": 3,
            "agent_steps": 6,
            "tool_calls": 2,
            "agents": 3,
            "models": 1,
        })
    );
    assert!(output.stderr_text()?.contains("analysis=overview"));
    Ok(())
}

#[tokio::test]
async fn grouped_analysis_subcommands_have_deterministic_semantics() -> Result<()> {
    let dataset = examples_root().to_string_lossy().into_owned();

    let agents = run_cli(["analysis", "agents", &dataset, "--format", "jsonl"]).await?;
    assert_eq!(
        jsonl_rows(&agents.stdout)?,
        vec![
            json!({"agent_id":"example-model","agent_name":"example-model","agent_version":"","trajectories":2,"sources":1,"steps":4,"user_steps":2,"agent_steps":2,"tool_calls":0}),
            json!({"agent_id":"actf-agent","agent_name":"ACTF Agent","agent_version":"","trajectories":1,"sources":1,"steps":2,"user_steps":0,"agent_steps":2,"tool_calls":1}),
            json!({"agent_id":"support-agent","agent_name":"support-agent","agent_version":"1.0.0","trajectories":1,"sources":1,"steps":3,"user_steps":1,"agent_steps":2,"tool_calls":1}),
        ]
    );

    let models = run_cli(["analysis", "models", &dataset, "--format", "jsonl"]).await?;
    assert_eq!(
        jsonl_rows(&models.stdout)?,
        vec![json!({
            "model": "example-model",
            "declared_trajectories": 3,
            "observed_steps": 4,
        })]
    );

    let tools = run_cli(["analysis", "tools", &dataset, "--format", "jsonl"]).await?;
    assert_eq!(
        jsonl_rows(&tools.stdout)?,
        vec![
            json!({"function_name":"Bash","calls":1,"trajectories":1,"sources":1,"duration_samples":1,"total_duration_ms":25}),
            json!({"function_name":"deployment_status","calls":1,"trajectories":1,"sources":1,"duration_samples":0,"total_duration_ms":0}),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn analysis_uses_default_warehouse_and_explicit_dataset_overrides_it() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let settings = temp
        .path()
        .join("settings.toml")
        .to_string_lossy()
        .into_owned();
    let warehouse = examples_root().to_string_lossy().into_owned();
    run_cli(["--settings", &settings, "default", &warehouse]).await?;

    let default = run_cli([
        "--settings",
        &settings,
        "analysis",
        "overview",
        "--format",
        "jsonl",
    ])
    .await?
    .json()?;
    assert_eq!(default["trajectories"], 4);

    let atif = examples_root().join("atif").to_string_lossy().into_owned();
    let explicit = run_cli([
        "--settings",
        &settings,
        "analysis",
        "overview",
        &atif,
        "--format",
        "jsonl",
    ])
    .await?
    .json()?;
    assert_eq!(explicit["trajectories"], 1);
    assert_eq!(explicit["steps"], 3);
    Ok(())
}

#[tokio::test]
async fn analysis_supports_table_csv_and_group_limits() -> Result<()> {
    let dataset = examples_root().to_string_lossy().into_owned();
    let table = run_cli(["analysis", "models", &dataset, "--format", "table"]).await?;
    let table = std::str::from_utf8(&table.stdout)?;
    assert!(table.lines().next().unwrap().contains("model"));
    assert!(table.contains("example-model"));

    let csv = run_cli(["analysis", "tools", &dataset, "--format", "csv"]).await?;
    let csv = std::str::from_utf8(&csv.stdout)?;
    assert_eq!(
        csv.lines().next(),
        Some("function_name,calls,trajectories,sources,duration_samples,total_duration_ms")
    );
    assert_eq!(csv.lines().count(), 3);

    let limited = run_cli([
        "analysis", "agents", &dataset, "--format", "jsonl", "--limit", "1",
    ])
    .await?;
    assert_eq!(jsonl_rows(&limited.stdout)?.len(), 1);

    let alias = run_cli([
        "analysis",
        "toolcalls",
        &dataset,
        "--format",
        "jsonl",
        "--limit",
        "1",
    ])
    .await?;
    assert_eq!(jsonl_rows(&alias.stdout)?[0]["function_name"], "Bash");
    Ok(())
}

#[tokio::test]
async fn empty_warehouse_has_an_overview_and_empty_grouped_analyses() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let dataset = temp.path().to_string_lossy().into_owned();
    let overview = run_cli(["analysis", "overview", &dataset, "--format", "jsonl"])
        .await?
        .json()?;
    assert_eq!(overview["sources"], 0);
    assert_eq!(overview["trajectories"], 0);

    for command in ["agents", "models", "tools"] {
        let output = run_cli(["analysis", command, &dataset, "--format", "jsonl"]).await?;
        assert!(output.stdout.is_empty(), "analysis={command}");
    }
    Ok(())
}

#[tokio::test]
async fn analysis_rejects_zero_limits_and_bounded_output_without_partial_stdout() -> Result<()> {
    let dataset = examples_root().to_string_lossy().into_owned();
    for args in [
        vec!["analysis", "agents", &dataset, "--limit", "0"],
        vec!["analysis", "agents", &dataset, "--limit", "10001"],
        vec!["analysis", "overview", &dataset, "--max-output-bytes", "8"],
        vec!["analysis", "overview", &dataset, "--timeout-seconds", "0"],
    ] {
        let error = run_cli(args).await.unwrap_err();
        assert!(!format!("{error:#}").is_empty());
    }
    Ok(())
}

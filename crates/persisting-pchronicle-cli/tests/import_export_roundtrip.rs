use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use persisting_pchronicle_cli::{run, Cli};
use serde_json::Value;

fn example_source(format: &str) -> PathBuf {
    let filename = match format {
        "atif" => "atif/support-ticket.json",
        "openai-messages" => "openai-messages/training.json",
        "actf" => "actf/code-repair.actf.json",
        other => panic!("unknown example format: {other}"),
    };
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/data")
        .join(filename)
}

#[tokio::test]
async fn import_export_roundtrip_is_byte_identical_and_reimportable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for (format, expected_runs) in [("atif", 1), ("openai-messages", 2), ("actf", 1)] {
        let input = example_source(format);
        let dataset = temp.path().join(format!("{format}-dataset"));
        let exported = temp.path().join(format!("{format}-export.json"));

        let cli = Cli::try_parse_from([
            "pchronicle",
            "import",
            "--from",
            input.to_str().unwrap(),
            "--output",
            dataset.to_str().unwrap(),
        ])?;
        run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;

        let cli = Cli::try_parse_from([
            "pchronicle",
            "export",
            "--from",
            dataset.to_str().unwrap(),
            "--output",
            exported.to_str().unwrap(),
            "--format",
            format,
            "--strict",
        ])?;
        let mut stderr = Vec::new();
        run(cli, false, &mut Vec::new(), &mut stderr).await?;
        assert_eq!(
            fs::read(&exported)?,
            fs::read(&input)?,
            "strict import/export must be byte-identical for {format}"
        );
        assert!(String::from_utf8(stderr)?.contains("exact=true"));

        let reimported = temp.path().join(format!("{format}-reimported"));
        let cli = Cli::try_parse_from([
            "pchronicle",
            "import",
            "--from",
            exported.to_str().unwrap(),
            "--output",
            reimported.to_str().unwrap(),
            "--format",
            format,
        ])?;
        run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;

        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            reimported.to_str().unwrap(),
            "SELECT COUNT(*) AS runs FROM dataset.runs",
            "--format",
            "jsonl",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        let count: Value = serde_json::from_slice(&stdout)
            .with_context(|| format!("decode reimported {format} query result"))?;
        assert_eq!(count["runs"], expected_runs, "format={format}");
    }
    Ok(())
}

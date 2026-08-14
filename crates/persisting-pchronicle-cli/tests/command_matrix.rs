mod common;

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use common::{examples_root, run_cli, EXAMPLE_FIXTURES};

#[test]
fn fixture_catalog_covers_every_warehouse_example() -> Result<()> {
    let warehouse: toml::Value = toml::from_str(&std::fs::read_to_string(
        examples_root().join("warehouse.toml"),
    )?)?;
    let configured = warehouse["datasets"]
        .as_array()
        .context("warehouse datasets must be an array")?
        .iter()
        .map(|dataset| {
            dataset["uri"]
                .as_str()
                .context("warehouse Dataset must have a URI")
                .map(|uri| uri.trim_start_matches("./"))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let fixtures = EXAMPLE_FIXTURES
        .iter()
        .map(|fixture| fixture.name)
        .collect::<BTreeSet<_>>();

    assert_eq!(fixtures, configured);
    for fixture in EXAMPLE_FIXTURES {
        assert!(fixture.source().is_file(), "missing fixture: {fixture:?}");
    }
    Ok(())
}

#[tokio::test]
async fn catalog_command_matrix_reports_every_supported_format() -> Result<()> {
    for fixture in EXAMPLE_FIXTURES {
        let dataset = fixture.dataset().to_string_lossy().into_owned();

        let listed = run_cli(["ls", &dataset, "--format", "json"])
            .await?
            .json()?;
        assert_eq!(listed["schema_version"], "pchronicle.ls.v1", "{fixture:?}");
        assert_eq!(
            listed["sources"].as_array().map(Vec::len),
            Some(1),
            "{fixture:?}"
        );
        assert_eq!(
            listed["sources"][0]["source_path"],
            fixture.dataset_source_name()
        );
        assert_eq!(listed["sources"][0]["status"], "ready", "{fixture:?}");

        let status = run_cli(["status", &dataset, "--format", "json"])
            .await?
            .json()?;
        assert_eq!(
            status["schema_version"], "pchronicle.status.v1",
            "{fixture:?}"
        );
        assert_eq!(status["status"], "ready", "{fixture:?}");
        assert_eq!(status["counts_complete"], true, "{fixture:?}");
        assert_eq!(
            status["counts"],
            json!({
                "runs": fixture.runs,
                "trajectories": fixture.trajectories,
                "steps": fixture.steps,
                "tool_calls": fixture.tool_calls,
                "events": 0,
            }),
            "{fixture:?}"
        );

        let queried = run_cli([
            "query",
            &dataset,
            "SELECT (SELECT COUNT(*) FROM dataset.runs) AS runs, \
             (SELECT COUNT(*) FROM dataset.trajectories) AS trajectories, \
             (SELECT COUNT(*) FROM dataset.steps) AS steps, \
             (SELECT COUNT(*) FROM dataset.tool_calls) AS tool_calls",
            "--format",
            "jsonl",
        ])
        .await?;
        assert_eq!(
            queried.json()?,
            json!({
                "runs": fixture.runs,
                "trajectories": fixture.trajectories,
                "steps": fixture.steps,
                "tool_calls": fixture.tool_calls,
            }),
            "{fixture:?}"
        );

        let found = run_cli([
            "find",
            &dataset,
            fixture.identity_flag,
            fixture.identity,
            "--format",
            "json",
        ])
        .await?
        .json()?;
        let matches = found["matches"]
            .as_array()
            .context("find response must contain matches")?;
        assert!(!matches.is_empty(), "{fixture:?}");
        assert!(
            matches
                .iter()
                .all(|item| item["source_path"] == fixture.dataset_source_name()),
            "{fixture:?}: {matches:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn import_matrix_preserves_sources_and_produces_queryable_datasets() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for fixture in EXAMPLE_FIXTURES {
        let source = fixture.source().to_string_lossy().into_owned();
        let dataset_path = temp.path().join(fixture.name);
        let dataset = dataset_path.to_string_lossy().into_owned();

        let response = run_cli(["import", "--from", &source, "--output", &dataset])
            .await?
            .json()?;
        assert_eq!(
            response["schema_version"], "pchronicle.import.v1",
            "{fixture:?}"
        );
        assert_eq!(response["format"], fixture.detected_format, "{fixture:?}");
        assert_eq!(
            response["source_path"], fixture.imported_source,
            "{fixture:?}"
        );
        assert_eq!(
            response["trajectories"], fixture.trajectories,
            "{fixture:?}"
        );
        assert_eq!(
            std::fs::read(dataset_path.join(fixture.imported_source))?,
            std::fs::read(fixture.source())?,
            "import changed source bytes for {fixture:?}"
        );

        let queried = run_cli([
            "query",
            &dataset,
            "SELECT COUNT(*) AS runs FROM dataset.runs",
            "--format",
            "jsonl",
        ])
        .await?;
        assert_eq!(queried.json()?["runs"], fixture.runs, "{fixture:?}");
    }
    Ok(())
}

#[tokio::test]
async fn query_output_matrix_encodes_every_supported_input_format() -> Result<()> {
    for fixture in EXAMPLE_FIXTURES {
        let dataset = fixture.dataset().to_string_lossy().into_owned();
        for output_format in ["jsonl", "csv", "table"] {
            let output = run_cli([
                "query",
                &dataset,
                "SELECT session_id FROM dataset.runs ORDER BY session_id",
                "--format",
                output_format,
            ])
            .await?;
            match output_format {
                "jsonl" => {
                    let rows = output
                        .stdout
                        .split(|byte| *byte == b'\n')
                        .filter(|line| !line.is_empty())
                        .map(serde_json::from_slice::<Value>)
                        .collect::<Result<Vec<_>, _>>()?;
                    assert_eq!(rows.len() as u64, fixture.runs, "{fixture:?}");
                }
                "csv" => {
                    let text = std::str::from_utf8(&output.stdout)?;
                    assert_eq!(text.lines().next(), Some("session_id"), "{fixture:?}");
                    assert_eq!(text.lines().count() as u64, fixture.runs + 1, "{fixture:?}");
                }
                "table" => {
                    let text = std::str::from_utf8(&output.stdout)?;
                    assert_eq!(
                        text.lines().next().map(str::trim),
                        Some("session_id"),
                        "{fixture:?}: {text}"
                    );
                    assert!(text.contains(fixture.session_id), "{fixture:?}: {text}");
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn every_example_exports_complete_storyline_documents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for fixture in EXAMPLE_FIXTURES {
        let source = fixture.source().to_string_lossy().into_owned();
        let dataset = temp.path().join(format!("{}-source", fixture.name));
        let dataset_arg = dataset.to_string_lossy().into_owned();
        run_cli(["import", "--from", &source, "--output", &dataset_arg]).await?;

        let storyline = temp.path().join(format!("{}.storyline.json", fixture.name));
        let storyline_arg = storyline.to_string_lossy().into_owned();
        let exported = run_cli([
            "export",
            "--from",
            &dataset_arg,
            "--output",
            &storyline_arg,
            "--format",
            "storyline",
        ])
        .await?;
        assert!(exported.stdout.is_empty(), "{fixture:?}");
        assert!(
            exported.stderr_text()?.contains("exact=false"),
            "{fixture:?}"
        );

        let value: Value = serde_json::from_slice(&std::fs::read(&storyline)?)?;
        let documents = match value {
            Value::Array(documents) => documents,
            document => vec![document],
        };
        assert_eq!(documents.len() as u64, fixture.trajectories, "{fixture:?}");
        assert!(
            documents
                .iter()
                .any(|document| document["session"] == fixture.session_id),
            "{fixture:?}: {documents:?}"
        );
        assert!(
            documents.iter().all(|document| document["turns"]
                .as_array()
                .is_some_and(|turns| !turns.is_empty())),
            "{fixture:?}: {documents:?}"
        );
    }
    Ok(())
}

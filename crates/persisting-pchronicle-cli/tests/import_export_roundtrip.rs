#![recursion_limit = "256"]

#[allow(dead_code)]
mod common;

use std::fs;

use anyhow::{Context, Result};
use serde_json::Value;

use common::{EXAMPLE_FIXTURES, run_cli};

fn canonical_json_bytes(path: &std::path::Path) -> Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    Ok(serde_json::to_vec_pretty(&value)?)
}

#[tokio::test]
async fn import_export_roundtrip_is_byte_identical_and_reimportable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for fixture in EXAMPLE_FIXTURES {
        let format = fixture.name;
        let expected_runs = fixture.runs;
        let input = fixture.source();
        let dataset = temp.path().join(format!("{format}-dataset"));
        let exported = temp.path().join(format!("{format}-export.json"));

        run_cli([
            "import",
            "--from",
            input.to_str().unwrap(),
            "--output",
            dataset.to_str().unwrap(),
        ])
        .await?;

        let exported_output = run_cli([
            "export",
            "--from",
            dataset.to_str().unwrap(),
            "--output",
            exported.to_str().unwrap(),
            "--format",
            format,
            "--strict",
        ])
        .await?;
        assert_eq!(
            fs::read(&exported)?,
            fs::read(&input)?,
            "strict import/export must be byte-identical for {format}"
        );
        assert!(exported_output.stderr_text()?.contains("exact=true"));

        let reimported = temp.path().join(format!("{format}-reimported"));
        run_cli([
            "import",
            "--from",
            exported.to_str().unwrap(),
            "--output",
            reimported.to_str().unwrap(),
            "--format",
            format,
        ])
        .await?;

        let queried = run_cli([
            "query",
            reimported.to_str().unwrap(),
            "SELECT COUNT(*) AS runs FROM dataset.runs",
            "--format",
            "jsonl",
        ])
        .await?;
        let count: Value = serde_json::from_slice(&queried.stdout)
            .with_context(|| format!("decode reimported {format} query result"))?;
        assert_eq!(count["runs"], expected_runs, "format={format}");
    }
    Ok(())
}

#[tokio::test]
async fn forced_storyline_roundtrip_is_canonical_and_reimport_stable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for fixture in EXAMPLE_FIXTURES {
        let format = fixture.name;
        let input = fixture.source();
        let dataset = temp.path().join(format!("{format}-storyline-dataset"));
        let exported = temp
            .path()
            .join(format!("{format}-storyline-roundtrip.json"));

        run_cli([
            "import",
            "--from",
            input.to_str().unwrap(),
            "--output",
            dataset.to_str().unwrap(),
        ])
        .await?;

        let exported_output = run_cli([
            "export",
            "--from",
            dataset.to_str().unwrap(),
            "--output",
            exported.to_str().unwrap(),
            "--format",
            format,
            // Any Trajectory filter disables the exact-source fast path, so this
            // exercises Source -> Storyline -> original format.
            "--where",
            "TRUE",
        ])
        .await?;
        assert!(exported_output.stderr_text()?.contains("exact=false"));

        let reimported = temp.path().join(format!("{format}-storyline-reimported"));
        run_cli([
            "import",
            "--from",
            exported.to_str().unwrap(),
            "--output",
            reimported.to_str().unwrap(),
            "--format",
            format,
        ])
        .await?;

        let reexported = temp
            .path()
            .join(format!("{format}-storyline-reexport.json"));
        let reexported_output = run_cli([
            "export",
            "--from",
            reimported.to_str().unwrap(),
            "--output",
            reexported.to_str().unwrap(),
            "--format",
            format,
            "--where",
            "TRUE",
        ])
        .await?;
        assert!(reexported_output.stderr_text()?.contains("exact=false"));

        assert_eq!(
            canonical_json_bytes(&reexported)?,
            canonical_json_bytes(&exported)?,
            "Storyline canonical JSON is not reimport-stable for {format}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn storyline_json_import_is_queryable_and_reexport_stable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input.storyline.json");
    let dataset = temp.path().join("storyline-dataset");
    let exported = temp.path().join("exported.storyline.json");
    let document = serde_json::json!({
        "schema_version": "storyline/v1",
        "session": "storyline-session",
        "agent": {"id": "storyline-agent"},
        "turns": [{
            "id": 3,
            "src": "user",
            "msg": "hello"
        }]
    });
    fs::write(&input, serde_json::to_vec_pretty(&document)?)?;

    run_cli([
        "import",
        "--from",
        input.to_str().unwrap(),
        "--output",
        dataset.to_str().unwrap(),
        "--format",
        "storyline",
    ])
    .await?;
    let queried = run_cli([
        "query",
        dataset.to_str().unwrap(),
        "SELECT COUNT(*) AS runs FROM dataset.runs",
        "--format",
        "jsonl",
    ])
    .await?;
    let count: Value = serde_json::from_slice(&queried.stdout)?;
    assert_eq!(count["runs"], 1);

    run_cli([
        "export",
        "--from",
        dataset.to_str().unwrap(),
        "--output",
        exported.to_str().unwrap(),
        "--format",
        "storyline",
        "--where",
        "TRUE",
    ])
    .await?;

    assert_eq!(
        canonical_json_bytes(&exported)?,
        canonical_json_bytes(&input)?
    );
    Ok(())
}

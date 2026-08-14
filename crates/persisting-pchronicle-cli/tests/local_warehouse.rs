#[allow(dead_code)]
mod common;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use common::{examples_root, run_cli, EXAMPLE_FIXTURES};

fn settings_arg(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[tokio::test]
async fn default_initializes_and_reports_a_local_warehouse() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let settings = temp.path().join("config/pchronicle.toml");
    let warehouse = temp.path().join("warehouse");
    let settings = settings_arg(&settings);
    let warehouse_arg = warehouse.to_string_lossy().into_owned();

    let configured = run_cli(["--settings", &settings, "default", &warehouse_arg]).await?;
    assert!(warehouse.is_dir());
    assert_eq!(
        configured.stdout,
        format!("{}\n", warehouse.canonicalize()?.display()).as_bytes()
    );
    assert!(configured.stderr_text()?.contains("updated=true"));

    let stored = std::fs::read_to_string(&settings)?;
    assert!(stored.contains("schema_version = 1"));
    assert!(stored.contains(warehouse.canonicalize()?.to_string_lossy().as_ref()));

    let reported = run_cli(["--settings", &settings, "default"]).await?;
    assert_eq!(reported.stdout, configured.stdout);
    assert!(reported.stderr.is_empty());

    let replacement = temp.path().join("replacement");
    let replacement_arg = replacement.to_string_lossy().into_owned();
    let updated = run_cli(["--settings", &settings, "default", &replacement_arg]).await?;
    assert_eq!(
        updated.stdout,
        format!("{}\n", replacement.canonicalize()?.display()).as_bytes()
    );
    assert_eq!(
        run_cli(["--settings", &settings, "default"]).await?.stdout,
        updated.stdout
    );
    assert!(!std::fs::read_to_string(&settings)?.contains(&warehouse_arg));
    Ok(())
}

#[tokio::test]
async fn default_warehouse_exercises_catalog_query_find_and_export_without_a_server() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let settings = settings_arg(&temp.path().join("settings.toml"));
    let warehouse = examples_root();
    let warehouse_arg = warehouse.to_string_lossy().into_owned();
    run_cli(["--settings", &settings, "default", &warehouse_arg]).await?;

    let listed = run_cli(["--settings", &settings, "ls", "--format", "json"])
        .await?
        .json()?;
    let sources = listed["sources"]
        .as_array()
        .context("Warehouse list must contain Sources")?;
    assert_eq!(sources.len(), 3);
    assert_eq!(
        sources
            .iter()
            .map(|source| source["source_path"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "actf/code-repair.actf.json",
            "atif/support-ticket.json",
            "openai-messages/training.json",
        ]
        .into_iter()
        .collect()
    );

    let status = run_cli(["--settings", &settings, "status", "--format", "json"])
        .await?
        .json()?;
    assert_eq!(status["status"], "ready");
    assert_eq!(
        status["counts"],
        json!({
            "runs": 4,
            "trajectories": 4,
            "steps": 9,
            "tool_calls": 2,
            "events": 0,
        })
    );

    let queried = run_cli([
        "--settings",
        &settings,
        "query",
        "SELECT COUNT(*) AS runs, COUNT(DISTINCT _file_) AS sources FROM dataset.runs",
        "--format",
        "jsonl",
    ])
    .await?
    .json()?;
    assert_eq!(queried, json!({"runs": 4, "sources": 3}));

    let found = run_cli([
        "--settings",
        &settings,
        "find",
        "--session-id",
        "support-001",
        "--format",
        "json",
    ])
    .await?
    .json()?;
    assert_eq!(found["matches"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        found["matches"][0]["source_path"],
        "atif/support-ticket.json"
    );

    let export = temp.path().join("warehouse.storyline.json");
    let export_arg = export.to_string_lossy().into_owned();
    run_cli([
        "--settings",
        &settings,
        "export",
        "--output",
        &export_arg,
        "--format",
        "storyline",
    ])
    .await?;
    let documents: Vec<Value> = serde_json::from_slice(&std::fs::read(export)?)?;
    assert_eq!(documents.len(), 4);
    assert!(documents.iter().all(|document| document["turns"]
        .as_array()
        .is_some_and(|turns| !turns.is_empty())));
    Ok(())
}

#[tokio::test]
async fn explicit_dataset_overrides_the_default_warehouse() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let settings = settings_arg(&temp.path().join("settings.toml"));
    let warehouse = examples_root().to_string_lossy().into_owned();
    run_cli(["--settings", &settings, "default", &warehouse]).await?;

    let atif = examples_root().join("atif").to_string_lossy().into_owned();
    let queried = run_cli([
        "--settings",
        &settings,
        "query",
        &atif,
        "SELECT COUNT(*) AS runs FROM dataset.runs",
        "--format",
        "jsonl",
    ])
    .await?
    .json()?;
    assert_eq!(queried["runs"], 1);
    Ok(())
}

#[tokio::test]
async fn empty_default_warehouse_can_be_populated_and_queried_without_output_paths() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let settings = settings_arg(&temp.path().join("settings.toml"));
    let warehouse = temp.path().join("warehouse");
    let warehouse_arg = warehouse.to_string_lossy().into_owned();
    run_cli(["--settings", &settings, "default", &warehouse_arg]).await?;
    let warehouse = warehouse.canonicalize()?;

    for fixture in EXAMPLE_FIXTURES {
        let source = fixture.source().to_string_lossy().into_owned();
        let imported = run_cli(["--settings", &settings, "import", "--from", &source])
            .await?
            .json()?;
        let dataset = std::path::PathBuf::from(
            imported["dataset_uri"]
                .as_str()
                .context("import response must contain Dataset URI")?,
        );
        assert_eq!(dataset.parent(), Some(warehouse.as_path()), "{fixture:?}");
        assert!(
            dataset.join(fixture.imported_source).is_file(),
            "{fixture:?}"
        );
    }

    let status = run_cli(["--settings", &settings, "status", "--format", "json"])
        .await?
        .json()?;
    assert_eq!(status["counts"]["runs"], 4);
    assert_eq!(status["sources"]["ready"], 3);

    let query = run_cli([
        "--settings",
        &settings,
        "query",
        "SELECT COUNT(*) AS trajectories FROM dataset.trajectories",
        "--format",
        "jsonl",
    ])
    .await?
    .json()?;
    assert_eq!(query["trajectories"], 4);

    let source = EXAMPLE_FIXTURES[0].source().to_string_lossy().into_owned();
    let error = run_cli(["--settings", &settings, "import", "--from", &source])
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("already exists"));
    Ok(())
}

#[tokio::test]
async fn omitted_dataset_fails_closed_without_default_settings() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let settings = settings_arg(&temp.path().join("missing.toml"));

    for args in [
        vec!["--settings", &settings, "ls"],
        vec!["--settings", &settings, "status"],
        vec!["--settings", &settings, "query", "SELECT 1"],
    ] {
        let error = run_cli(args).await.unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("default Warehouse is not configured"),
            "{message}"
        );
        assert!(message.contains("pchronicle default"), "{message}");
    }
    Ok(())
}

#[tokio::test]
async fn invalid_or_stale_settings_fail_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for (content, expected) in [
        ("not toml = [", "parse pChronicle settings"),
        (
            "schema_version = 2\ndefault_warehouse = '/tmp'\n",
            "unsupported settings schema_version",
        ),
        (
            "schema_version = 1\ndefault_warehouse = 's3://bucket/path'\n",
            "configured default Warehouse must be a local directory",
        ),
    ] {
        let settings_path = temp.path().join(format!(
            "settings-{}.toml",
            blake3::hash(content.as_bytes()).to_hex()
        ));
        std::fs::write(&settings_path, content)?;
        let settings = settings_arg(&settings_path);
        let error = run_cli(["--settings", &settings, "default"])
            .await
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains(expected), "{message}");
    }

    let settings_path = temp.path().join("stale.toml");
    let warehouse = temp.path().join("stale-warehouse");
    let settings = settings_arg(&settings_path);
    let warehouse_arg = warehouse.to_string_lossy().into_owned();
    run_cli(["--settings", &settings, "default", &warehouse_arg]).await?;
    std::fs::remove_dir(&warehouse)?;
    let error = run_cli(["--settings", &settings, "status"])
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("configured default Warehouse"));
    Ok(())
}

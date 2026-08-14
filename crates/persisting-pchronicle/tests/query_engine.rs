//! Public SQL engine tests shared by Lance and ATIF datasources.

use std::path::{Path, PathBuf};

use anyhow::Result;
use datafusion::prelude::SessionContext;
use persisting_pchronicle::{
    into_storyline, AtifDataSource, AtifDataSourceOptions, AtifReader, AtifTrajectory,
    ChronicleFormat, ChronicleQueryBackend, ChronicleQueryEngine, ChronicleQueryExecutionOptions,
    EventIdentity, EventRecord, ExternalTableFormat, ExternalTableSpec,
    FileTrajectoryDataSourceOptions, LocalQueryManifest, RawEventLanceStore, StoryCoords,
    StorylineDataFusionTableNames, StorylineLanceStore, StructuredStore,
};

const SHARED_SQL: &str =
    "SELECT s.session_id, s.step_id, s.source, t.tool_call_id, t.function_name \
     FROM steps s LEFT JOIN tool_calls t \
       ON s.session_id = t.session_id AND s.step_id = t.step_id \
     WHERE s.session_id IN ('fixture-parallel_tools_14', 'fixture-reasoning_16') \
     ORDER BY s.session_id, s.step_id, t.call_index";

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif")
}

fn fixture_paths() -> Result<Vec<PathBuf>> {
    let mut paths = std::fs::read_dir(fixture_root())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|path| path.extension().and_then(|value| value.to_str()) == Some("json"));
    paths.sort();
    Ok(paths)
}

fn load_trajectories() -> Result<Vec<AtifTrajectory>> {
    fixture_paths()?
        .into_iter()
        .map(|path| {
            AtifTrajectory::from_json_str(&std::fs::read_to_string(path)?).map_err(Into::into)
        })
        .collect()
}

fn write_ndjson(path: &Path, trajectories: &[AtifTrajectory]) -> Result<()> {
    let mut lines = trajectories
        .iter()
        .map(serde_json::to_string)
        .collect::<serde_json::Result<Vec<_>>>()?
        .join("\n");
    lines.push('\n');
    std::fs::write(path, lines)?;
    Ok(())
}

#[test]
fn atif_datasource_accepts_json_array_jsonl_and_directory() -> Result<()> {
    let trajectories = load_trajectories()?;
    let array = serde_json::to_string(&trajectories)?;
    let from_array = AtifDataSource::from_json(&array)?;
    assert_eq!(from_array.document_count(), Some(8));
    assert_eq!(from_array.step_count(), Some(118));
    assert_eq!(from_array.tool_call_count(), Some(23));

    let dir = tempfile::tempdir()?;
    let ndjson = dir.path().join("atif.ndjson");
    write_ndjson(&ndjson, &trajectories)?;
    let from_jsonl = AtifDataSource::open(&ndjson)?;
    assert_eq!(from_jsonl.document_count(), None);
    assert_eq!(from_jsonl.step_count(), None);

    let from_directory = AtifDataSource::open(fixture_root())?;
    assert_eq!(from_directory.document_count(), None);
    assert_eq!(from_directory.step_count(), None);
    Ok(())
}

#[test]
fn atif_datasource_and_reader_share_the_recursive_manifest() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let nested = temp.path().join("nested");
    std::fs::create_dir(&nested)?;
    std::fs::copy(
        fixture_root().join("dialogue_10.json"),
        nested.join("input.json"),
    )?;

    let source = AtifDataSource::open(temp.path())?;
    assert_eq!(source.document_count(), None);
    assert_eq!(AtifReader::open(temp.path())?.count(), 1);
    Ok(())
}

#[tokio::test]
async fn default_atif_file_datasource_uses_repeatable_streaming_plan() -> Result<()> {
    let source = AtifDataSource::open(fixture_root())?;
    let context = source.session_context()?;
    let dataframe = context.sql("SELECT COUNT(*) AS steps FROM steps").await?;
    let plan = dataframe.clone().create_physical_plan().await?;
    let plan_text = datafusion::physical_plan::displayable(plan.as_ref())
        .indent(true)
        .to_string();
    assert!(plan_text.contains("StreamingTableExec"), "{plan_text}");
    assert!(!plan_text.contains("MemoryExec"), "{plan_text}");

    for _ in 0..2 {
        let output = dataframe.clone().collect().await?;
        assert_eq!(
            output.iter().map(|batch| batch.num_rows()).sum::<usize>(),
            1
        );
    }
    Ok(())
}

#[tokio::test]
async fn atif_file_filter_prunes_before_validation_and_exposes_relative_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::copy(
        fixture_root().join("dialogue_10.json"),
        temp.path().join("good.json"),
    )?;
    std::fs::write(temp.path().join("unmatched.json"), "not-json")?;
    let engine = ChronicleQueryEngine::open_atif(temp.path())?;
    let output = engine
        .query_jsonl("SELECT _file_ FROM runs WHERE _file_ = 'good.json'")
        .await?;
    assert!(output.contains("good.json"));
    let error = engine.query("SELECT * FROM runs").await.unwrap_err();
    assert!(format!("{error:#}").contains("unmatched.json"));
    Ok(())
}

#[test]
fn atif_reader_streams_ndjson_and_directories_in_path_order() -> Result<()> {
    let mut trajectories = load_trajectories()?;
    let mut first = trajectories.remove(0);
    first.session_id = Some("first-file".into());
    let mut second = trajectories.remove(0);
    second.session_id = Some("second-file".into());
    let dir = tempfile::tempdir()?;
    write_ndjson(&dir.path().join("02.ndjson"), &[second])?;
    write_ndjson(&dir.path().join("01.ndjson"), &[first])?;

    let ids = AtifReader::open(dir.path())?
        .map(|trajectory| {
            trajectory.map(|trajectory| trajectory.session_id.expect("fixture session_id"))
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(ids, ["first-file", "second-file"]);

    let invalid = dir.path().join("03.ndjson");
    let mut valid = serde_json::to_string(&load_trajectories()?[0])?;
    valid.push_str("\nnot-json\n");
    std::fs::write(&invalid, valid)?;
    let error = AtifReader::open(&invalid)?
        .nth(1)
        .expect("second line")
        .unwrap_err();
    assert!(format!("{error:#}").contains("line 2"), "{error:#}");
    Ok(())
}

#[tokio::test]
async fn atif_datasource_validates_inputs_and_custom_table_names() -> Result<()> {
    assert!(AtifDataSource::from_json("").is_err());
    assert!(AtifDataSource::from_json("[]").is_err());
    assert!(
        AtifDataSource::from_json_with_options("{}", AtifDataSourceOptions { batch_size: 0 })
            .unwrap_err()
            .to_string()
            .contains("batch_size")
    );

    let trajectories = load_trajectories()?;
    let duplicate = vec![trajectories[0].clone(), trajectories[0].clone()];
    assert!(AtifDataSource::from_trajectories(&duplicate)
        .unwrap_err()
        .to_string()
        .contains("duplicate ATIF session_id"));

    let dir = tempfile::tempdir()?;
    assert!(AtifDataSource::open(dir.path()).is_err());
    let duplicate_jsonl = dir.path().join("duplicate.ndjson");
    let duplicate_line = serde_json::to_string(&trajectories[0])?;
    std::fs::write(
        &duplicate_jsonl,
        format!("{duplicate_line}\n{duplicate_line}\n"),
    )?;
    let duplicate_source = AtifDataSource::open(&duplicate_jsonl)?;
    let duplicate_error = duplicate_source
        .session_context()?
        .sql("SELECT * FROM runs")
        .await?
        .collect()
        .await
        .unwrap_err();
    assert!(format!("{duplicate_error:#}").contains("duplicate atif session_id"));
    let invalid_jsonl = dir.path().join("invalid.jsonl");
    std::fs::write(&invalid_jsonl, "{}\nnot-json\n")?;
    let invalid_source = AtifDataSource::open(&invalid_jsonl)?;
    let invalid_error = invalid_source
        .session_context()?
        .sql("SELECT * FROM runs")
        .await?
        .collect()
        .await
        .unwrap_err();
    assert!(
        format!("{invalid_error:#}").contains("line 1"),
        "{invalid_error:#}"
    );

    let source = AtifDataSource::from_trajectories_with_options(
        &trajectories[..2],
        AtifDataSourceOptions { batch_size: 3 },
    )?;
    let context = SessionContext::new();
    source.register_as(
        &context,
        &StorylineDataFusionTableNames {
            runs: "atif_runs".into(),
            steps: "atif_steps".into(),
            tool_calls: "atif_tools".into(),
        },
    )?;
    let batches = context
        .sql("SELECT COUNT(*) AS count FROM atif_steps")
        .await?
        .collect()
        .await?;
    assert_eq!(
        batches.iter().map(|batch| batch.num_rows()).sum::<usize>(),
        1
    );

    let duplicate_names = StorylineDataFusionTableNames {
        runs: "same".into(),
        steps: "same".into(),
        tool_calls: "tools".into(),
    };
    assert!(source.register_as(&context, &duplicate_names).is_err());
    let empty_name = StorylineDataFusionTableNames {
        runs: "".into(),
        steps: "steps".into(),
        tool_calls: "tools".into(),
    };
    assert!(source.register_as(&context, &empty_name).is_err());

    let missing = dir.path().join("missing.json");
    assert!(AtifDataSource::open(missing).is_err());
    assert_eq!(AtifDataSource::open(dir.path())?.file_count(), 2);
    Ok(())
}

#[tokio::test]
async fn same_sql_returns_identical_results_for_lance_and_atif() -> Result<()> {
    let trajectories = load_trajectories()?;
    let atif_engine =
        ChronicleQueryEngine::from_atif_source(AtifDataSource::from_trajectories(&trajectories)?)?;
    assert!(matches!(
        atif_engine.backend(),
        ChronicleQueryBackend::Atif {
            files: 0,
            documents: Some(8),
            steps: Some(118),
            tool_calls: Some(23)
        }
    ));

    let dir = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(dir.path()).await?;
    let stories = trajectories
        .iter()
        .map(|trajectory| {
            into_storyline(
                ChronicleFormat::Atif,
                &serde_json::to_string(trajectory).unwrap(),
            )
        })
        .collect::<persisting_pchronicle::Result<Vec<_>>>()?;
    store.replace_storylines(&stories).await?;
    let lance_engine = ChronicleQueryEngine::open_lance(dir.path()).await?;
    assert!(matches!(
        lance_engine.backend(),
        ChronicleQueryBackend::Lance { .. }
    ));

    let atif_jsonl = atif_engine.query_jsonl(SHARED_SQL).await?;
    let lance_jsonl = lance_engine.query_jsonl(SHARED_SQL).await?;
    assert_eq!(lance_jsonl, atif_jsonl);
    assert_eq!(atif_jsonl.lines().count(), 33);

    let aggregate = lance_engine
        .query_jsonl("SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source")
        .await?;
    assert_eq!(aggregate.lines().count(), 3);
    for line in aggregate.lines() {
        let _: serde_json::Value = serde_json::from_str(line)?;
    }
    Ok(())
}

#[tokio::test]
async fn timestamp_milliseconds_match_for_lance_and_direct_atif_queries() -> Result<()> {
    let trajectories = load_trajectories()?;
    let dir = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(dir.path()).await?;
    let stories = trajectories
        .iter()
        .map(|trajectory| {
            into_storyline(
                ChronicleFormat::Atif,
                &serde_json::to_string(trajectory).unwrap(),
            )
        })
        .collect::<persisting_pchronicle::Result<Vec<_>>>()?;
    store.replace_storylines(&stories).await?;

    let lance = ChronicleQueryEngine::open_lance(dir.path()).await?;
    let atif = ChronicleQueryEngine::open_atif(fixture_root())?;
    let sql = "SELECT session_id, step_id, timestamp \
               FROM steps \
               WHERE timestamp >= TIMESTAMP '2026-06-15T09:00:10Z' \
               ORDER BY timestamp, session_id, step_id";
    let lance_rows = lance.query_jsonl(sql).await?;
    let atif_rows = atif.query_jsonl(sql).await?;

    assert_eq!(lance_rows, atif_rows);
    assert!(!lance_rows.is_empty());
    Ok(())
}

#[tokio::test]
async fn streaming_jsonl_matches_collected_jsonl() -> Result<()> {
    let trajectories = load_trajectories()?;
    let engine = ChronicleQueryEngine::from_atif_source(AtifDataSource::from_trajectories(
        &trajectories[..1],
    )?)?;
    let sql = "SELECT session_id, step_id FROM steps ORDER BY step_id";
    let collected = engine.query_jsonl(sql).await?;
    let mut streamed = Vec::new();
    engine.write_query_jsonl(sql, &mut streamed).await?;
    assert_eq!(String::from_utf8(streamed)?, collected);

    let mut limited = Vec::new();
    let error = engine
        .write_query_jsonl_with_max_rows(sql, &mut limited, Some(1))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("max_output_rows"));
    Ok(())
}

#[tokio::test]
async fn query_runtime_validates_memory_and_spill_limits() -> Result<()> {
    let input = fixture_root().join("dialogue_10.json");
    let manifest = LocalQueryManifest::for_format(&input, ChronicleFormat::Atif)?;
    let invalid = ChronicleQueryEngine::open_local_manifest_with_execution_options(
        manifest.clone(),
        FileTrajectoryDataSourceOptions::default(),
        ChronicleQueryExecutionOptions {
            memory_limit_bytes: Some(0),
            ..ChronicleQueryExecutionOptions::default()
        },
    )
    .unwrap_err();
    assert!(invalid.to_string().contains("memory_limit_bytes"));

    let spill = tempfile::tempdir()?;
    let engine = ChronicleQueryEngine::open_local_manifest_with_execution_options(
        manifest,
        FileTrajectoryDataSourceOptions::default(),
        ChronicleQueryExecutionOptions {
            memory_limit_bytes: Some(64 * 1024 * 1024),
            spill_path: Some(spill.path().to_path_buf()),
            max_spill_bytes: Some(256 * 1024 * 1024),
        },
    )?;
    assert!(!engine
        .query_jsonl("SELECT COUNT(*) FROM steps")
        .await?
        .is_empty());
    Ok(())
}

#[tokio::test]
async fn query_engine_joins_csv_and_json_external_tables() -> Result<()> {
    let engine = ChronicleQueryEngine::open_atif(fixture_root())?;
    let temp = tempfile::tempdir()?;
    let labels = temp.path().join("labels.csv");
    std::fs::write(
        &labels,
        "session_id,score\nfixture-parallel_tools_14,7\nfixture-reasoning_16,9\n",
    )?;
    let metadata = temp.path().join("metadata.json");
    std::fs::write(
        &metadata,
        r#"[
  {"session_id":"fixture-parallel_tools_14","category":"tools"},
  {"session_id":"fixture-reasoning_16","category":"reasoning"}
]"#,
    )?;
    let activity = temp.path().join("activity.jsonl");
    std::fs::write(
        &activity,
        concat!(
            "{\"session_id\":\"fixture-parallel_tools_14\",\"active\":true}\n",
            "{\"session_id\":\"fixture-reasoning_16\",\"active\":false}\n"
        ),
    )?;

    engine
        .register_external_table(&ExternalTableSpec::new(
            "labels",
            ExternalTableFormat::Csv,
            labels.to_string_lossy(),
        ))
        .await?;
    engine
        .register_external_table(&ExternalTableSpec::new(
            "metadata",
            ExternalTableFormat::Json,
            metadata.to_string_lossy(),
        ))
        .await?;
    engine
        .register_external_table(&ExternalTableSpec::new(
            "activity",
            ExternalTableFormat::JsonLines,
            activity.to_string_lossy(),
        ))
        .await?;

    let output = engine
        .query_jsonl(
            "SELECT r.session_id, l.score, m.category, a.active \
             FROM runs r \
             JOIN labels l ON r.session_id = l.session_id \
             JOIN metadata m ON r.session_id = m.session_id \
             JOIN activity a ON r.session_id = a.session_id \
             ORDER BY r.session_id",
        )
        .await?;
    let rows = output
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["session_id"], "fixture-parallel_tools_14");
    assert_eq!(rows[0]["score"], 7);
    assert_eq!(rows[0]["category"], "tools");
    assert_eq!(rows[0]["active"], true);
    assert_eq!(rows[1]["session_id"], "fixture-reasoning_16");
    assert_eq!(rows[1]["active"], false);

    let collision = engine
        .register_external_table(&ExternalTableSpec::new(
            "runs",
            ExternalTableFormat::Csv,
            labels.to_string_lossy(),
        ))
        .await
        .unwrap_err();
    assert!(collision.to_string().contains("already registered"));
    Ok(())
}

#[tokio::test]
async fn query_engine_opens_object_store_uri() -> Result<()> {
    let uri = format!(
        "shared-memory://pchronicle-query-{}-{}/storylines",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let trajectories = load_trajectories()?;
    let stories = trajectories[..2]
        .iter()
        .map(|trajectory| {
            into_storyline(
                ChronicleFormat::Atif,
                &serde_json::to_string(trajectory).unwrap(),
            )
        })
        .collect::<persisting_pchronicle::Result<Vec<_>>>()?;
    StorylineLanceStore::open_uri(&uri)
        .await?
        .replace_storylines(&stories)
        .await?;

    let engine = ChronicleQueryEngine::open_lance_uri(&uri).await?;
    let output = engine
        .query_jsonl("SELECT COUNT(*) AS runs FROM runs")
        .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(output.trim())?["runs"],
        2
    );

    // An engine pins one immutable version tuple. Moving CURRENT must not change
    // the result of an already planned federated/long-running query.
    let pinned_generation = engine.backend().clone();
    StorylineLanceStore::open_uri(&uri)
        .await?
        .replace_storyline(&into_storyline(
            ChronicleFormat::Atif,
            &serde_json::to_string(&trajectories[2])?,
        )?)
        .await?;
    let pinned_output = engine
        .query_jsonl("SELECT COUNT(*) AS runs FROM runs")
        .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(pinned_output.trim())?["runs"],
        2
    );
    assert_eq!(engine.backend(), &pinned_generation);

    let reopened = ChronicleQueryEngine::open_lance_uri(&uri).await?;
    let current_output = reopened
        .query_jsonl("SELECT COUNT(*) AS runs FROM runs")
        .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(current_output.trim())?["runs"],
        3
    );
    assert_ne!(reopened.backend(), &pinned_generation);
    Ok(())
}

#[tokio::test]
async fn query_engine_rejects_empty_object_store_without_current() -> Result<()> {
    let uri = format!(
        "shared-memory://pchronicle-query-empty-{}-{}/storylines",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let error = ChronicleQueryEngine::open_lance_uri(&uri)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("no committed generation"),
        "{error:#}"
    );
    Ok(())
}

#[tokio::test]
async fn query_engine_exposes_canonical_events_table() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = dir.path().join("store");
    let session = StoryCoords::new(storage.to_string_lossy(), "agent", "story", None);
    let records = [("event-a", 9_u64, "first"), ("event-b", 3_u64, "second")]
        .into_iter()
        .map(|(event_id, seq, content)| EventRecord {
            identity: EventIdentity {
                event_id: Some(event_id.into()),
                ..Default::default()
            },
            seq,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"content": content}),
        })
        .collect::<Vec<_>>();
    RawEventLanceStore.append_events(&session, &records).await?;

    let path = persisting_pchronicle::raw_event_lance_path(&session)?;
    let engine = ChronicleQueryEngine::open_events(&path).await?;
    assert!(matches!(
        engine.backend(),
        ChronicleQueryBackend::Events { version } if *version > 0
    ));
    let output = engine
        .query_jsonl("SELECT seq, session_id, kind, payload_json FROM events ORDER BY seq")
        .await?;
    let rows = output
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["seq"], 3);
    assert_eq!(rows[1]["seq"], 9);
    let first: EventRecord = serde_json::from_str(rows[0]["payload_json"].as_str().unwrap())?;
    let second: EventRecord = serde_json::from_str(rows[1]["payload_json"].as_str().unwrap())?;
    assert_eq!([first.seq, second.seq], [3, 9]);
    Ok(())
}

#[test]
fn query_engine_rejects_writes_and_multiple_statements() -> Result<()> {
    let engine = ChronicleQueryEngine::open_atif(fixture_root())?;
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;

    let copy_error = runtime
        .block_on(engine.dataframe("COPY steps TO '/tmp/pchronicle.parquet' STORED AS PARQUET"))
        .expect_err("COPY must be rejected");
    assert!(copy_error.to_string().contains("only accepts"));

    let multi_error = runtime
        .block_on(engine.dataframe("SELECT 1; SELECT 2"))
        .expect_err("multiple statements must be rejected");
    assert!(multi_error.to_string().contains("exactly one"));

    let empty_error = runtime
        .block_on(engine.dataframe(""))
        .expect_err("empty SQL must be rejected");
    assert!(empty_error.to_string().contains("exactly one"));

    let insert_error = runtime
        .block_on(engine.dataframe("INSERT INTO steps VALUES (1)"))
        .expect_err("INSERT must be rejected");
    assert!(insert_error.to_string().contains("only accepts"));

    let values = runtime.block_on(engine.query_jsonl("VALUES (1), (2)"))?;
    assert_eq!(values.lines().count(), 2);
    let explain = runtime.block_on(engine.query("EXPLAIN SELECT * FROM runs"))?;
    assert!(!explain.is_empty());
    let empty_result = runtime.block_on(engine.query_jsonl("SELECT * FROM runs WHERE 1 = 0"))?;
    assert!(empty_result.is_empty());
    Ok(())
}

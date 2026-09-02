//! ATIF corpus conformance tests for the Storyline three-table store.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use lance_index::scalar::FullTextSearchQuery;
use persisting_pchronicle::document::{
    DocumentFormat, decode_agenticmd, decode_json_storylines, encode_agenticmd,
    encode_json_storylines,
};
use persisting_pchronicle::model::StorylineDocument;
use persisting_pchronicle::query::{
    ChronicleQueryEngine, ChronicleQueryExecutionOptions, QuerySnapshot,
};
use persisting_pchronicle::search::{
    search_storyline_step_matches_fts_in_columns, storyline_steps_fts_available,
};
use persisting_pchronicle::storage::StorylineLanceStore;

mod support;

use support::fixture_path;

#[derive(Clone, Copy)]
enum TestFormat {
    Storyline,
    AgenticMd,
    OpenaiMsg,
    Atif,
}

fn into_storyline(
    format: TestFormat,
    input: &str,
) -> persisting_pchronicle::document::Result<StorylineDocument> {
    match format {
        TestFormat::Storyline => serde_json::from_str(input).map_err(Into::into),
        TestFormat::AgenticMd => Ok(decode_agenticmd(input)?),
        TestFormat::OpenaiMsg => {
            let mut stories =
                decode_json_storylines(DocumentFormat::OpenaiMsg, input, "corpus.json")?;
            if stories.len() != 1 {
                anyhow::bail!(
                    "{} document cannot represent {} storylines",
                    DocumentFormat::OpenaiMsg,
                    stories.len()
                );
            }
            Ok(stories.remove(0))
        }
        TestFormat::Atif => {
            let mut stories = decode_json_storylines(DocumentFormat::Atif, input, "corpus.json")?;
            if stories.len() != 1 {
                anyhow::bail!(
                    "{} document cannot represent {} storylines",
                    DocumentFormat::Atif,
                    stories.len()
                );
            }
            Ok(stories.remove(0))
        }
    }
}

fn from_storyline(
    format: TestFormat,
    story: &StorylineDocument,
) -> persisting_pchronicle::document::Result<String> {
    match format {
        TestFormat::Storyline => story.to_json_string_pretty(),
        TestFormat::AgenticMd => encode_agenticmd(story),
        TestFormat::OpenaiMsg => Ok(serde_json::to_string_pretty(&encode_json_storylines(
            DocumentFormat::OpenaiMsg,
            std::slice::from_ref(story),
        )?)?),
        TestFormat::Atif => Ok(serde_json::to_string_pretty(&encode_json_storylines(
            DocumentFormat::Atif,
            std::slice::from_ref(story),
        )?)?),
    }
}

fn fixture_root() -> PathBuf {
    fixture_path("atif")
}

fn fixture_paths() -> Result<Vec<PathBuf>> {
    let mut paths = std::fs::read_dir(fixture_root())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|path| path.extension().and_then(|value| value.to_str()) == Some("json"));
    paths.sort();
    Ok(paths)
}

fn load(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read ATIF fixture {}", path.display()))?;
    Ok(raw)
}

#[test]
fn corpus_has_expected_size_and_step_range() -> Result<()> {
    let paths = fixture_paths()?;
    assert_eq!(paths.len(), 8, "fixture corpus size changed");
    let mut counts = Vec::new();
    for path in paths {
        let raw = load(&path)?;
        let story = into_storyline(TestFormat::Atif, &raw)?;
        assert!((10..=20).contains(&story.turns.len()));
        counts.push(story.turns.len());
    }
    counts.sort_unstable();
    assert_eq!(counts, vec![10, 12, 13, 14, 15, 16, 18, 20]);
    Ok(())
}

#[test]
fn corpus_round_trips_through_storyline_and_atif() -> Result<()> {
    for path in fixture_paths()? {
        let raw = load(&path)?;
        let story = into_storyline(TestFormat::Atif, &raw)?;
        let encoded = from_storyline(TestFormat::Atif, &story)?;
        let reconstructed = into_storyline(TestFormat::Atif, &encoded)?;
        assert_eq!(
            reconstructed,
            story,
            "round-trip mismatch: {}",
            path.display()
        );
    }
    Ok(())
}

#[test]
fn corpus_converts_to_all_text_formats() -> Result<()> {
    for path in fixture_paths()? {
        let raw = load(&path)?;
        let story = into_storyline(TestFormat::Atif, &raw)?;
        for format in [TestFormat::Storyline, TestFormat::AgenticMd] {
            let encoded = from_storyline(format, &story)?;
            let parsed = into_storyline(format, &encoded)?;
            parsed.validate()?;
        }
        match from_storyline(TestFormat::OpenaiMsg, &story) {
            Ok(encoded) => into_storyline(TestFormat::OpenaiMsg, &encoded)?.validate()?,
            Err(error) => assert!(error.to_string().contains("OpenAI synthesis")),
        }
    }
    Ok(())
}

#[tokio::test]
async fn corpus_round_trips_through_three_lance_tables() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(dir.path()).await?;
    let mut expected = Vec::new();
    for path in fixture_paths()? {
        let raw = load(&path)?;
        let story = into_storyline(TestFormat::Atif, &raw)?;
        store.replace_storyline(&story).await?;
        expected.push(story);
    }

    for story in expected {
        assert_eq!(
            store.get_storyline_full(&story.session_id).await?,
            Some(story)
        );
    }
    let paths = store.current_table_paths().await?.unwrap();
    assert!(paths.runs.is_dir());
    assert!(paths.steps.is_dir());
    assert!(paths.tool_calls.is_dir());
    Ok(())
}

#[tokio::test]
async fn atif_null_and_missing_canonicalization_is_stable_through_lance() -> Result<()> {
    let input = serde_json::json!({
        "schema_version": "ATIF-v1.7",
        "trajectory_id": "canonical-trajectory",
        "agent": {
            "name": "agent-1",
            "version": "1",
            "model_name": null,
            "extra": null
        },
        "steps": [{
            "step_id": 1,
            "timestamp": null,
            "source": "agent",
            "message": "done",
            "reasoning_content": null,
            "tool_calls": [
                {
                    "tool_call_id": "missing",
                    "function_name": "a",
                    "arguments": {}
                },
                {
                    "tool_call_id": "null",
                    "function_name": "b",
                    "arguments": {},
                    "result": null
                },
                {
                    "tool_call_id": "value",
                    "function_name": "c",
                    "arguments": {},
                    "result": {"ok": true}
                }
            ],
            "observation": null,
            "metrics": null,
            "extra": null,
            "llm_call_count": null,
            "is_copied_context": null
        }],
        "notes": null,
        "final_metrics": null,
        "continued_trajectory_ref": null,
        "extra": null,
        "subagent_trajectories": null
    });
    let story = into_storyline(TestFormat::Atif, &input.to_string())?;
    let expected: serde_json::Value =
        serde_json::from_str(&from_storyline(TestFormat::Atif, &story)?)?;
    let dir = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(dir.path()).await?;
    store.replace_storyline(&story).await?;

    let restored = store
        .get_storyline_full(&story.session_id)
        .await?
        .context("missing canonical Storyline after Lance write")?;
    let output: serde_json::Value =
        serde_json::from_str(&from_storyline(TestFormat::Atif, &restored)?)?;
    assert_eq!(output, expected);
    Ok(())
}

#[tokio::test]
async fn datafusion_datasource_filters_joins_and_pins_generation() -> Result<()> {
    use lance::index::DatasetIndexExt;

    let dir = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(dir.path()).await?;
    for path in fixture_paths()? {
        let raw = load(&path)?;
        store
            .replace_storyline(&into_storyline(TestFormat::Atif, &raw)?)
            .await?;
    }

    let engine = ChronicleQueryEngine::open(
        DocumentFormat::StorylineLance,
        dir.path(),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let pinned_generation = match &engine.backend_info().unwrap().snapshot {
        Some(QuerySnapshot::Storyline { generation }) => generation.clone(),
        other => anyhow::bail!("unexpected Storyline snapshot: {other:?}"),
    };
    let context = engine.context();
    let filtered = context
        .sql(
            "SELECT step_id, source FROM steps \
             WHERE session_id = 'fixture-reasoning_16' AND step_id BETWEEN 5 AND 10 \
             ORDER BY step_id",
        )
        .await?
        .collect()
        .await?;
    assert_eq!(
        filtered.iter().map(|batch| batch.num_rows()).sum::<usize>(),
        6
    );
    assert_eq!(filtered[0].num_columns(), 2, "projection was not applied");
    let physical_plan = context
        .sql(
            "SELECT step_id FROM steps \
             WHERE session_id = 'fixture-reasoning_16' AND step_id >= 5 LIMIT 2",
        )
        .await?
        .create_physical_plan()
        .await?;
    let plan_text = datafusion::physical_plan::displayable(physical_plan.as_ref())
        .indent(true)
        .to_string();
    assert!(
        plan_text.contains("projection=[session_id, step_id]"),
        "{plan_text}"
    );
    assert!(plan_text.contains("ScalarIndexQuery"), "{plan_text}");
    assert!(
        plan_text.contains("pchronicle_session_id_idx(BTree)"),
        "{plan_text}"
    );
    assert!(!plan_text.contains("pchronicle_step_id_idx"), "{plan_text}");
    let joined = context
        .sql(
            "SELECT t.tool_call_id, t.results, s.effective_kind \
             FROM tool_calls t JOIN steps s \
               ON t.session_id = s.session_id AND t.step_id = s.step_id \
             WHERE t.session_id = 'fixture-parallel_tools_14' \
             ORDER BY t.step_id, t.call_index",
        )
        .await?
        .collect()
        .await?;
    assert_eq!(
        joined.iter().map(|batch| batch.num_rows()).sum::<usize>(),
        6
    );

    let paths = store.current_table_paths().await?.unwrap();
    let step_indices = lance::Dataset::open(paths.steps.to_string_lossy().as_ref())
        .await?
        .load_indices()
        .await?;
    let names = step_indices
        .iter()
        .map(|index| index.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"pchronicle_session_id_idx"));
    assert!(!names.contains(&"pchronicle_step_id_idx"));
    assert!(names.contains(&"pchronicle_effective_kind_idx"));
    assert!(names.contains(&"pchronicle_fts_message_value_idx"));
    assert!(names.contains(&"pchronicle_json_metrics_idx"));

    let steps = lance::Dataset::open(paths.steps.to_string_lossy().as_ref()).await?;
    let mut fts_scan = steps.scan();
    fts_scan.full_text_search(
        FullTextSearchQuery::new("deterministic".to_string())
            .with_column("message_value".to_string())?,
    )?;
    let fts_batch = fts_scan.try_into_batch().await?;
    assert!(
        fts_batch.num_rows() > 0,
        "Storyline FTS returned no matches"
    );

    let mut zh_fts_scan = steps.scan();
    zh_fts_scan.full_text_search(
        FullTextSearchQuery::new("验证中文".to_string())
            .with_column("message_value".to_string())?,
    )?;
    let zh_fts_batch = zh_fts_scan.try_into_batch().await?;
    assert!(
        zh_fts_batch.num_rows() > 0,
        "bundled Jieba tokenizer returned no Chinese matches"
    );

    assert!(storyline_steps_fts_available(&paths).await?);
    let search_hits =
        search_storyline_step_matches_fts_in_columns(&paths, "deterministic", &["message_value"])
            .await?;
    assert!(!search_hits.is_empty());
    assert_eq!(
        persisting_pchronicle::storage::search_storyline_step_matches_fts_in_columns(
            &paths,
            "deterministic",
            &["message_value"],
        )
        .await?,
        search_hits,
        "storage compatibility export must use the search kernel"
    );

    // A registered datasource remains a consistent snapshot after CURRENT moves.
    let raw = load(&fixture_root().join("dialogue_10.json"))?;
    let mut additional = into_storyline(TestFormat::Atif, &raw)?;
    additional.session_id = "fixture-added-after-open".into();
    additional.trajectory_id = Some("fixture-added-after-open".into());
    additional.run_id = Some("run-added-after-open".into());
    store.replace_storyline(&additional).await?;
    assert_ne!(
        store.current_table_paths().await?.unwrap().generation,
        pinned_generation
    );
    let old_rows = context
        .sql("SELECT session_id FROM runs")
        .await?
        .collect()
        .await?;
    assert_eq!(
        old_rows.iter().map(|batch| batch.num_rows()).sum::<usize>(),
        8
    );
    let new_engine = ChronicleQueryEngine::open(
        DocumentFormat::StorylineLance,
        dir.path(),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let new_context = new_engine.context();
    let new_rows = new_context
        .sql("SELECT session_id FROM runs")
        .await?
        .collect()
        .await?;
    assert_eq!(
        new_rows.iter().map(|batch| batch.num_rows()).sum::<usize>(),
        9
    );
    Ok(())
}

#[tokio::test]
async fn query_engine_validates_storyline_store_state() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(dir.path()).await?;
    assert!(
        ChronicleQueryEngine::open(
            DocumentFormat::StorylineLance,
            dir.path(),
            ChronicleQueryExecutionOptions::default(),
        )
        .await
        .is_err()
    );

    let raw = load(&fixture_root().join("dialogue_10.json"))?;
    store
        .replace_storyline(&into_storyline(TestFormat::Atif, &raw)?)
        .await?;
    let engine = ChronicleQueryEngine::open(
        DocumentFormat::StorylineLance,
        dir.path(),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let batches = engine.query("SELECT COUNT(*) AS count FROM steps").await?;
    assert_eq!(
        batches.iter().map(|batch| batch.num_rows()).sum::<usize>(),
        1
    );

    Ok(())
}

//! ATIF corpus conformance tests for the Storyline three-table store.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use persisting_pchronicle::document::{
    decode_agenticmd, decode_json_storylines, encode_agenticmd, encode_json_storylines,
    DocumentFormat,
};
use persisting_pchronicle::model::StorylineDocument;
use persisting_pchronicle::query::{
    ChronicleQueryEngine, ChronicleQueryExecutionOptions, QuerySnapshot,
};
use persisting_pchronicle::storage::StorylineLanceStore;

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
        TestFormat::AgenticMd => decode_agenticmd(input),
        TestFormat::OpenaiMsg => {
            let mut stories =
                decode_json_storylines(DocumentFormat::OpenaiMsg, input, "corpus.json")?;
            if stories.len() != 1 {
                return Err(
                    persisting_pchronicle::document::Error::UnsupportedCardinality {
                        format: persisting_pchronicle::document::DocumentFormat::OpenaiMsg,
                        stories: stories.len(),
                    },
                );
            }
            Ok(stories.remove(0))
        }
        TestFormat::Atif => {
            let mut stories = decode_json_storylines(DocumentFormat::Atif, input, "corpus.json")?;
            if stories.len() != 1 {
                return Err(
                    persisting_pchronicle::document::Error::UnsupportedCardinality {
                        format: DocumentFormat::Atif,
                        stories: stories.len(),
                    },
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
        DocumentFormat::Storyline,
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
            "SELECT t.tool_call_id, t.results_json, s.effective_kind \
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
        DocumentFormat::Storyline,
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
    assert!(ChronicleQueryEngine::open(
        DocumentFormat::Storyline,
        dir.path(),
        ChronicleQueryExecutionOptions::default(),
    )
    .await
    .is_err());

    let raw = load(&fixture_root().join("dialogue_10.json"))?;
    store
        .replace_storyline(&into_storyline(TestFormat::Atif, &raw)?)
        .await?;
    let engine = ChronicleQueryEngine::open(
        DocumentFormat::Storyline,
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

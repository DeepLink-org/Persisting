//! ATIF corpus conformance tests for the Storyline three-table store.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use persisting_pchronicle::{
    from_storyline, into_storyline, split_storyline, AtifTrajectory, ChronicleFormat,
    StorylineDataFusionTableNames, StorylineDataSource, StorylineDataSourceOptions,
    StorylineLanceStore, StorylineTableKind,
};

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

fn load(path: &Path) -> Result<(String, AtifTrajectory)> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read ATIF fixture {}", path.display()))?;
    let trajectory = AtifTrajectory::from_json_str(&raw)
        .with_context(|| format!("parse ATIF fixture {}", path.display()))?;
    Ok((raw, trajectory))
}

#[test]
fn corpus_has_expected_size_and_step_range() -> Result<()> {
    let paths = fixture_paths()?;
    assert_eq!(paths.len(), 8, "fixture corpus size changed");
    let mut counts = Vec::new();
    for path in paths {
        let (_, trajectory) = load(&path)?;
        assert!((10..=20).contains(&trajectory.steps.len()));
        counts.push(trajectory.steps.len());
    }
    counts.sort_unstable();
    assert_eq!(counts, vec![10, 12, 13, 14, 15, 16, 18, 20]);
    Ok(())
}

#[test]
fn corpus_round_trips_through_storyline_and_atif() -> Result<()> {
    for path in fixture_paths()? {
        let (raw, _) = load(&path)?;
        let story = into_storyline(ChronicleFormat::Atif, &raw)?;
        let encoded = from_storyline(ChronicleFormat::Atif, &story)?;
        let reconstructed = into_storyline(ChronicleFormat::Atif, &encoded)?;
        assert_eq!(
            reconstructed,
            story,
            "round-trip mismatch: {}",
            path.display()
        );

        // Every ATIF observation result must have been attached to a tool row.
        let tables = split_storyline(&story)?;
        let expected_results = story
            .turns
            .iter()
            .filter_map(|turn| turn.observation.as_ref())
            .filter_map(|observation| observation.get("results"))
            .filter_map(|results| results.as_array())
            .map(Vec::len)
            .sum::<usize>();
        assert_eq!(
            tables
                .tool_calls
                .iter()
                .map(|call| call.results.len())
                .sum::<usize>(),
            expected_results,
            "tool result mismatch: {}",
            path.display()
        );
    }
    Ok(())
}

#[test]
fn corpus_converts_to_all_text_formats() -> Result<()> {
    for path in fixture_paths()? {
        let (raw, _) = load(&path)?;
        let story = into_storyline(ChronicleFormat::Atif, &raw)?;
        for format in [
            ChronicleFormat::Storyline,
            ChronicleFormat::Agenticmd,
            ChronicleFormat::OpenaiMsg,
        ] {
            let encoded = from_storyline(format, &story)?;
            let parsed = into_storyline(format, &encoded)?;
            parsed.validate()?;
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
        let (raw, _) = load(&path)?;
        let story = into_storyline(ChronicleFormat::Atif, &raw)?;
        store.replace_storyline(&story).await?;
        expected.push(story);
    }

    assert_eq!(store.list_runs().await?.len(), expected.len());
    for story in expected {
        assert_eq!(store.get_storyline(&story.session_id).await?, Some(story));
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
        let (raw, _) = load(&path)?;
        store
            .replace_storyline(&into_storyline(ChronicleFormat::Atif, &raw)?)
            .await?;
    }

    let source = StorylineDataSource::from_store(&store).await?;
    let pinned_generation = source.generation().to_string();
    let context = source.session_context()?;
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
    let full_scan_context = StorylineDataSource::from_store_with_options(
        &store,
        StorylineDataSourceOptions {
            use_scalar_indexes: false,
            ..Default::default()
        },
    )
    .await?
    .session_context()?;
    let full_scan_plan = full_scan_context
        .sql(
            "SELECT step_id FROM steps \
             WHERE session_id = 'fixture-reasoning_16' AND step_id >= 5 LIMIT 2",
        )
        .await?
        .create_physical_plan()
        .await?;
    let full_scan_plan_text = datafusion::physical_plan::displayable(full_scan_plan.as_ref())
        .indent(true)
        .to_string();
    assert!(!full_scan_plan_text.contains("ScalarIndexQuery"));

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

    let step_indices = source
        .provider(StorylineTableKind::Steps)
        .dataset()
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
    let (raw, _) = load(&fixture_root().join("dialogue_10.json"))?;
    let mut additional = into_storyline(ChronicleFormat::Atif, &raw)?;
    additional.session_id = "fixture-added-after-open".into();
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
    let new_context = StorylineDataSource::from_store(&store)
        .await?
        .session_context()?;
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
async fn lance_datasource_validates_store_state_and_custom_names() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(dir.path()).await?;
    assert!(StorylineDataSource::from_store(&store).await.is_err());

    let (raw, _) = load(&fixture_root().join("dialogue_10.json"))?;
    store
        .replace_storyline(&into_storyline(ChronicleFormat::Atif, &raw)?)
        .await?;
    let source = StorylineDataSource::from_store(&store).await?;
    let context = datafusion::prelude::SessionContext::new();
    source.register_as(
        &context,
        &StorylineDataFusionTableNames {
            runs: "story_runs".into(),
            steps: "story_steps".into(),
            tool_calls: "story_tools".into(),
        },
    )?;
    let batches = context
        .sql("SELECT COUNT(*) AS count FROM story_steps")
        .await?
        .collect()
        .await?;
    assert_eq!(
        batches.iter().map(|batch| batch.num_rows()).sum::<usize>(),
        1
    );

    assert!(source
        .register_as(
            &context,
            &StorylineDataFusionTableNames {
                runs: "same".into(),
                steps: "same".into(),
                tool_calls: "tools".into(),
            },
        )
        .is_err());
    assert!(source
        .register_as(
            &context,
            &StorylineDataFusionTableNames {
                runs: "".into(),
                steps: "steps".into(),
                tool_calls: "tools".into(),
            },
        )
        .is_err());
    Ok(())
}

use super::*;
use crate::layout::{story_lance_event_path, StoryCoords};
use crate::projection::{
    build_storyline_projection, rebuild_storyline_projection, sync_storyline_projection,
    StorylineProjectionBuildOutcome, StorylineProjectionSyncMode, StorylineProjectionSyncOutcome,
};
use crate::store::{RawEventLanceStore, StorylineLanceStore};
use crate::{EventIdentity, StorylineAgent, StorylineTurn};
use object_store::ObjectStoreExt;

fn write_openai_source(path: &Path, event_id: &str) -> Result<()> {
    fs::write(
        path,
        format!(
            r#"[{{"id":"{event_id}","session_id":"shared-session","step_id":1,"agent_model":"model","messages":[{{"role":"user","content":"hello"}}],"response":{{"role":"assistant","content":"world"}}}}]"#
        ),
    )?;
    Ok(())
}

fn storyline(session_id: &str, run_id: &str) -> StorylineDocument {
    StorylineDocument {
        schema_version: crate::model::STORYLINE_SCHEMA_VERSION.into(),
        origin: None,
        run_id: Some(run_id.into()),
        trajectory_id: None,
        attempt_id: None,
        session_id: session_id.into(),
        agent: StorylineAgent {
            id: "agent".into(),
            name: None,
            version: None,
            model_name: Some("model".into()),
            tool_definitions: None,
            extra: None,
        },
        parent: None,
        child_session_ids: None,
        notes: None,
        final_metrics: None,
        continued_trajectory_ref: None,
        extra: None,
        unknown_fields: Default::default(),
        unknown_key_counts: Default::default(),
        turns: vec![StorylineTurn {
            id: 1,
            kind: None,
            timestamp: None,
            source: "user".into(),
            message: serde_json::json!("hello"),
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: None,
            observation: None,
            metrics: None,
            model_name: None,
            llm_call_count: None,
            is_copied_context: None,
            latency_ms: None,
            ttft_ms: None,
            extra: None,
        }],
    }
}

#[test]
fn dataset_names_are_normalized_and_validated() {
    assert_eq!(
        DatasetMount::new("DataSet", "/tmp/x").unwrap().name,
        "dataset"
    );
    assert!(DatasetMount::new("bad-name", "/tmp/x").is_err());
    assert!(DatasetMount::new("public", "/tmp/x").is_err());

    let mount = DatasetMount::namespaced(
        NamespacePath::new(["prod", "agent-data"]).unwrap(),
        "prod_agents",
        "/tmp/x",
    )
    .unwrap();
    assert_eq!(mount.namespace.display_name(), "prod/agent-data");
    assert_eq!(mount.name, "prod_agents");
}

#[tokio::test]
async fn namespace_listing_is_hierarchical_paginated_and_snapshot_bound() -> Result<()> {
    let first = tempfile::tempdir()?;
    let second = tempfile::tempdir()?;
    write_openai_source(&first.path().join("first.json"), "event-first")?;
    write_openai_source(&first.path().join("second.json"), "event-second")?;
    let snapshot = DatasetCatalogSnapshot::discover(
        vec![
            DatasetMount::namespaced(
                NamespacePath::new(["prod", "agents"])?,
                "prod_agents",
                first.path().to_string_lossy(),
            )?,
            DatasetMount::namespaced(
                NamespacePath::new(["staging", "agents"])?,
                "staging_agents",
                second.path().to_string_lossy(),
            )?,
        ],
        None,
        CatalogSnapshotOptions::default(),
    )
    .await?;

    let first_page = snapshot.list_namespaces(None, None, Some(1))?;
    assert_eq!(first_page.items.len(), 1);
    let token = first_page
        .next_page_token
        .as_deref()
        .context("two root namespaces require a second page")?;
    let second_page = snapshot.list_namespaces(None, Some(token), Some(1))?;
    assert_eq!(second_page.items.len(), 1);
    assert!(second_page.next_page_token.is_none());

    let prod = NamespacePath::single("prod")?;
    let children = snapshot.list_namespaces(Some(&prod), None, None)?;
    assert_eq!(children.items.len(), 1);
    assert!(children.items[0].mounted);
    assert_eq!(children.items[0].sql_alias.as_deref(), Some("prod_agents"));

    let prod_agents = NamespacePath::new(["prod", "agents"])?;
    let sources = snapshot.list_sources(&prod_agents, None, Some(1))?;
    assert_eq!(sources.items.len(), 1);
    assert!(sources.next_page_token.is_some());
    let source = snapshot
        .describe_source(&prod_agents, "first.json")?
        .context("mounted namespace must describe its frozen source")?;
    assert_eq!(source.sql_alias, "prod_agents");
    assert!(matches!(
        source.source.revision,
        Some(CatalogSourceRevision::LocalFile { .. })
    ));

    let changed = DatasetCatalogSnapshot::discover(
        vec![DatasetMount::default(first.path().to_string_lossy())?],
        None,
        CatalogSnapshotOptions::default(),
    )
    .await?;
    assert!(changed.list_namespaces(None, Some(token), Some(1)).is_err());
    Ok(())
}

#[tokio::test]
async fn discovers_mixed_local_files_and_exposes_sources() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir(temp.path().join("nested"))?;
    fs::write(
        temp.path().join("openai.json"),
        r#"[{"session_id":"s1","step_id":0,"messages":[]}]"#,
    )?;
    fs::write(
        temp.path().join("nested/atif.jsonl"),
        r#"{"schema_version":"ATIF-v1.4","session_id":"s2","steps":[],"agent":{"id":"a"}}"#,
    )?;
    let snapshot = DatasetCatalogSnapshot::discover(
        vec![DatasetMount::default(temp.path().to_string_lossy())?],
        Some(DEFAULT_DATASET_NAME.into()),
        CatalogSnapshotOptions::default(),
    )
    .await?;
    assert_eq!(snapshot.datasets()[0].ready_source_count(), 2);
    assert_eq!(snapshot.datasets()[0].sources[0].file, "nested/atif.jsonl");

    let context = SessionContext::new();
    snapshot.register(&context).await?;
    let rows = context
        .sql("SELECT _file_, format FROM dataset.sources ORDER BY _file_")
        .await?
        .collect()
        .await?;
    assert_eq!(rows.iter().map(RecordBatch::num_rows).sum::<usize>(), 2);
    let compatibility_rows = context
        .sql("SELECT _file_ FROM sources ORDER BY _file_")
        .await?
        .collect()
        .await?;
    assert_eq!(
        compatibility_rows
            .iter()
            .map(RecordBatch::num_rows)
            .sum::<usize>(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn ignores_derived_lance_sidecars_during_discovery() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("run/derived-metrics.lance/_versions"))?;
    fs::write(
        temp.path()
            .join("run/derived-metrics.lance/_versions/latest_version_hint.json"),
        "{}",
    )?;
    write_openai_source(&temp.path().join("trajectory.json"), "event-1")?;

    let snapshot = DatasetCatalogSnapshot::discover(
        vec![DatasetMount::default(temp.path().to_string_lossy())?],
        Some(DEFAULT_DATASET_NAME.into()),
        CatalogSnapshotOptions::default(),
    )
    .await?;
    assert_eq!(snapshot.datasets()[0].sources.len(), 1);
    assert_eq!(snapshot.datasets()[0].sources[0].file, "trajectory.json");
    Ok(())
}

#[tokio::test]
async fn report_mode_keeps_late_local_format_errors_lazy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("broken.json"), "{")?;
    let snapshot = Arc::new(
        DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(temp.path().to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions {
                error_policy: CatalogErrorPolicy::Report,
                ..CatalogSnapshotOptions::default()
            },
        )
        .await?,
    );
    assert_eq!(snapshot.datasets()[0].ready_source_count(), 1);
    assert_eq!(snapshot.datasets()[0].error_source_count(), 0);
    assert_eq!(snapshot.datasets()[0].sources[0].format, None);
    assert_eq!(
        snapshot.prepared[0].sources[0]
            .resolution_count
            .load(Ordering::Relaxed),
        0
    );

    let engine = snapshot.clone().query_engine(Default::default()).await?;
    let error = engine
        .query("SELECT run_id FROM dataset.runs WHERE _file_ = 'broken.json'")
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("broken.json"));
    assert_eq!(
        snapshot.prepared[0].sources[0]
            .resolution_count
            .load(Ordering::Relaxed),
        1
    );
    Ok(())
}

#[tokio::test]
async fn empty_dataset_still_exposes_the_stable_catalog_tables() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let snapshot = Arc::new(
        DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(temp.path().to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?,
    );
    assert_eq!(snapshot.datasets()[0].sources.len(), 0);
    let engine = snapshot.query_engine(Default::default()).await?;
    let output = engine
        .query_jsonl("SELECT COUNT(*) AS runs FROM runs")
        .await?;
    assert_eq!(output.trim(), r#"{"runs":0}"#);
    Ok(())
}

#[tokio::test]
async fn catalog_prunes_file_sources_before_lazy_resolution() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_openai_source(&temp.path().join("one.json"), "event-1")?;
    fs::write(temp.path().join("two.json"), "{")?;
    let snapshot = Arc::new(
        DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(temp.path().to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?,
    );
    assert!(snapshot.prepared[0]
        .sources
        .iter()
        .all(|source| source.resolution_count.load(Ordering::Relaxed) == 0));

    let engine = snapshot.clone().query_engine(Default::default()).await?;
    assert!(snapshot.prepared[0]
        .sources
        .iter()
        .all(|source| source.resolution_count.load(Ordering::Relaxed) == 0));
    assert_eq!(
        engine
            .query_jsonl("SELECT COUNT(*) AS sources FROM dataset.sources")
            .await?
            .trim(),
        r#"{"sources":2}"#
    );
    assert!(snapshot.prepared[0]
        .sources
        .iter()
        .all(|source| source.resolution_count.load(Ordering::Relaxed) == 0));
    let unsafe_mixed_join = engine
        .query(
            "SELECT * FROM runs r JOIN dataset.steps s \
                 ON r.run_id = s.run_id",
        )
        .await
        .unwrap_err();
    assert!(format!("{unsafe_mixed_join:#}").contains("must include"));
    let rows = engine
        .query_jsonl(
            "SELECT document_id, step_count FROM dataset.trajectories \
                 WHERE _file_ LIKE 'one.%' AND document_id = 'shared-session'",
        )
        .await?;
    assert_eq!(
        rows.trim(),
        r#"{"document_id":"shared-session","step_count":2}"#
    );
    assert_eq!(
        snapshot.prepared[0]
            .sources
            .iter()
            .map(|source| source.resolution_count.load(Ordering::Relaxed))
            .collect::<Vec<_>>(),
        vec![1, 0]
    );
    let explain = engine
        .query_jsonl("EXPLAIN SELECT document_id FROM dataset.runs WHERE _file_ = 'one.json'")
        .await?;
    assert!(!explain.contains("UnionExec"));
    assert_eq!(engine.local_file_metrics().unwrap().files_parsed, 1);
    let error = engine
        .query("SELECT document_id FROM dataset.runs WHERE _file_ = 'two.json'")
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("two.json"));
    assert_eq!(
        snapshot.prepared[0]
            .sources
            .iter()
            .map(|source| source.resolution_count.load(Ordering::Relaxed))
            .collect::<Vec<_>>(),
        vec![1, 1]
    );
    Ok(())
}

#[tokio::test]
async fn catalog_downloads_only_selected_remote_file_source() -> Result<()> {
    let uri = format!(
        "shared-memory://pchronicle-catalog-lazy-{}/root",
        uuid::Uuid::new_v4().simple()
    );
    let (store, root) = LanceObjectStore::from_uri(&uri).await?;
    for (file, content) in [
        (
            "one.json",
            r#"[{"id":"event-1","session_id":"shared-session","step_id":1,"agent_model":"model","messages":[],"response":{"role":"assistant","content":"world"}}]"#,
        ),
        ("two.json", "{"),
    ] {
        store
            .inner
            .put(&root.clone().join(file), content.to_string().into())
            .await?;
    }
    let snapshot = Arc::new(
        DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(uri)?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions {
                error_policy: CatalogErrorPolicy::Report,
                ..CatalogSnapshotOptions::default()
            },
        )
        .await?,
    );
    assert!(snapshot.prepared[0]
        .sources
        .iter()
        .all(|source| source.resolution_count.load(Ordering::Relaxed) == 0));
    let engine = snapshot.clone().query_engine(Default::default()).await?;
    let rows = engine
        .query_jsonl("SELECT document_id FROM dataset.runs WHERE _file_ = 'one.json'")
        .await?;
    assert_eq!(rows.trim(), r#"{"document_id":"shared-session"}"#);
    assert_eq!(
        snapshot.prepared[0]
            .sources
            .iter()
            .map(|source| source.resolution_count.load(Ordering::Relaxed))
            .collect::<Vec<_>>(),
        vec![1, 0]
    );
    let error = engine
        .query("SELECT document_id FROM dataset.runs WHERE _file_ = 'two.json'")
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("two.json"));
    let chain = error.chain().map(ToString::to_string).collect::<Vec<_>>();
    let logical_context = chain
        .iter()
        .position(|source| source == "detect format for remote trajectory object two.json")
        .expect("logical remote source context must remain a distinct error-chain entry");
    let detector_source = chain
        .iter()
        .position(|source| source.starts_with("cannot detect trajectory format:"))
        .expect("format detector failure must remain in the error source chain");
    assert!(
        detector_source > logical_context,
        "format detector failure must be below the logical remote source context: {chain:?}"
    );
    assert_eq!(
        snapshot.prepared[0]
            .sources
            .iter()
            .map(|source| source.resolution_count.load(Ordering::Relaxed))
            .collect::<Vec<_>>(),
        vec![1, 1]
    );
    Ok(())
}

#[tokio::test]
async fn catalog_prunes_storyline_sources_before_opening_lance() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for (name, session_id) in [("a", "session-a"), ("b", "session-b")] {
        let store = StorylineLanceStore::open(temp.path().join(name)).await?;
        store
            .replace_storyline(&storyline(session_id, &format!("run-{name}")))
            .await?;
    }
    let snapshot = Arc::new(
        DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(temp.path().to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?,
    );
    // Lazy resolution must open the generation pinned above, not follow a
    // newer CURRENT pointer published before the first query.
    StorylineLanceStore::open(temp.path().join("a"))
        .await?
        .replace_storyline(&storyline("session-a-new", "run-a-new"))
        .await?;
    let engine = snapshot.clone().query_engine(Default::default()).await?;
    assert!(snapshot.prepared[0]
        .sources
        .iter()
        .all(|source| source.resolution_count.load(Ordering::Relaxed) == 0));

    let rows = engine
        .query_jsonl(
            "SELECT run_id FROM dataset.runs \
                 WHERE _file_ = 'a' AND run_id = 'run-a'",
        )
        .await?;
    assert_eq!(rows.trim(), r#"{"run_id":"run-a"}"#);
    assert_eq!(
        snapshot.prepared[0]
            .sources
            .iter()
            .map(|source| source.resolution_count.load(Ordering::Relaxed))
            .collect::<Vec<_>>(),
        vec![1, 0]
    );
    Ok(())
}

#[tokio::test]
async fn trajectory_bundle_derives_events_from_one_storyline_source_resolution() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let expected = storyline("bundle-session", "bundle-run");
    StorylineLanceStore::open(temp.path())
        .await?
        .replace_storyline(&expected)
        .await?;
    let snapshot = DatasetCatalogSnapshot::discover(
        vec![DatasetMount::default(temp.path().to_string_lossy())?],
        Some(DEFAULT_DATASET_NAME.into()),
        CatalogSnapshotOptions::default(),
    )
    .await?;
    let key = CatalogStorylineKey {
        dataset: DEFAULT_DATASET_NAME.into(),
        file: ".".into(),
        document_id: expected.session_id.clone(),
        session_id: expected.session_id.clone(),
    };

    let bundle = snapshot
        .load_trajectory_bundle(&key)
        .await?
        .context("trajectory bundle must exist")?;

    assert_eq!(bundle.storyline, expected);
    assert_eq!(bundle.events, storyline_to_events(&bundle.storyline)?);
    assert_eq!(
        snapshot.prepared[0].sources[0]
            .resolution_count
            .load(Ordering::Relaxed),
        1
    );
    Ok(())
}

#[tokio::test]
async fn one_source_keeps_storylines_with_a_shared_run_id_independent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(temp.path()).await?;
    store
        .replace_storylines(&[
            storyline("root-session", "shared-run"),
            storyline("child-session", "shared-run"),
        ])
        .await?;
    let snapshot = Arc::new(
        DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(temp.path().to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?,
    );
    let engine = snapshot.clone().query_engine(Default::default()).await?;

    let rows = engine
        .query_jsonl(
            "SELECT session_id, run_id, step_count FROM dataset.trajectories \
                 ORDER BY session_id",
        )
        .await?;
    assert_eq!(
        rows.lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<serde_json::Result<Vec<_>>>()?,
        vec![
            serde_json::json!({
                "session_id": "child-session",
                "run_id": "shared-run",
                "step_count": 1
            }),
            serde_json::json!({
                "session_id": "root-session",
                "run_id": "shared-run",
                "step_count": 1
            }),
        ]
    );

    for session_id in ["root-session", "child-session"] {
        let story = snapshot
            .load_storyline(&CatalogStorylineKey {
                dataset: DEFAULT_DATASET_NAME.into(),
                file: ".".into(),
                document_id: session_id.into(),
                session_id: session_id.into(),
            })
            .await?
            .context("Catalog Storyline must resolve by session_id")?;
        assert_eq!(story.session_id, session_id);
        assert_eq!(story.run_id.as_deref(), Some("shared-run"));
        assert_eq!(story.turns.len(), 1);
    }
    Ok(())
}

#[tokio::test]
async fn catalog_point_load_uses_document_id_when_sessions_are_shared() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(temp.path()).await?;
    let mut first = storyline("shared-session", "run-a");
    first.trajectory_id = Some("document-a".into());
    let mut second = storyline("shared-session", "run-b");
    second.trajectory_id = Some("document-b".into());
    store.replace_storylines(&[first, second]).await?;
    let snapshot = DatasetCatalogSnapshot::discover(
        vec![DatasetMount::default(temp.path().to_string_lossy())?],
        Some(DEFAULT_DATASET_NAME.into()),
        CatalogSnapshotOptions::default(),
    )
    .await?;

    for (document_id, run_id) in [("document-a", "run-a"), ("document-b", "run-b")] {
        let story = snapshot
            .load_storyline(&CatalogStorylineKey {
                dataset: DEFAULT_DATASET_NAME.into(),
                file: ".".into(),
                document_id: document_id.into(),
                session_id: "shared-session".into(),
            })
            .await?
            .context("Catalog Storyline must resolve by document_id")?;
        assert_eq!(story.trajectory_id.as_deref(), Some(document_id));
        assert_eq!(story.run_id.as_deref(), Some(run_id));
    }
    Ok(())
}

#[tokio::test]
async fn catalog_joins_require_file_keys_only_within_one_dataset() -> Result<()> {
    let left = tempfile::tempdir()?;
    let right = tempfile::tempdir()?;
    for root in [left.path(), right.path()] {
        write_openai_source(&root.join("one.json"), "event-1")?;
        write_openai_source(&root.join("two.json"), "event-2")?;
    }
    let snapshot = Arc::new(
        DatasetCatalogSnapshot::discover(
            vec![
                DatasetMount::new("left_data", left.path().to_string_lossy())?,
                DatasetMount::new("right_data", right.path().to_string_lossy())?,
            ],
            None,
            CatalogSnapshotOptions::default(),
        )
        .await?,
    );
    let engine = snapshot.query_engine(Default::default()).await?;

    let unsafe_join = engine
        .query(
            "SELECT * FROM left_data.runs r JOIN left_data.steps s \
                 ON r.run_id = s.run_id",
        )
        .await
        .unwrap_err();
    assert!(format!("{unsafe_join:#}").contains("must include"));

    let cross_dataset = engine
        .query(
            "SELECT count(*) FROM left_data.runs l JOIN right_data.runs r \
                 ON l.run_id = r.run_id",
        )
        .await?;
    assert_eq!(
        cross_dataset
            .iter()
            .map(RecordBatch::num_rows)
            .sum::<usize>(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn canonical_event_source_exposes_and_loads_each_storyline_independently() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("capture");
    for session_id in ["root", "child"] {
        let coords = StoryCoords::new(
            storage.to_string_lossy(),
            "agent",
            session_id,
            Some("run-1".into()),
        );
        RawEventLanceStore
            .append_events(
                &coords,
                &[EventRecord {
                    identity: EventIdentity::default(),
                    seq: 0,
                    source: "test".into(),
                    kind: "note".into(),
                    timestamp: None,
                    session_id: Some(session_id.into()),
                    agent_id: Some("agent".into()),
                    parent_uuid: None,
                    trace_id: None,
                    call_id: None,
                    subagent_id: None,
                    parent_agent_id: None,
                    branch: None,
                    parent_call_id: None,
                    payload: serde_json::json!({"session": session_id}),
                }],
            )
            .await?;
    }

    let events_uri =
        story_lance_event_path(&storage.to_string_lossy(), "agent", "root", Some("run-1"))?;
    let projection_uri = storage.join("agent/run-1/storyline");
    let StorylineProjectionBuildOutcome::Built(_) = build_storyline_projection(
        events_uri.to_string_lossy(),
        projection_uri.to_string_lossy(),
        "agent/run-1/events.lance",
    )
    .await?
    else {
        panic!("initial catalog projection build unexpectedly reported nonempty output")
    };

    let snapshot = Arc::new(
        DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(storage.to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?,
    );
    assert_eq!(snapshot.datasets()[0].sources.len(), 1);
    assert_eq!(
        snapshot.datasets()[0].sources[0].file,
        "agent/run-1/events.lance"
    );
    assert_eq!(
        snapshot.datasets()[0].sources[0].projection_status,
        Some(CatalogProjectionStatus::Fresh)
    );
    let updated_coords = StoryCoords::new(
        storage.to_string_lossy(),
        "agent",
        "root",
        Some("run-1".into()),
    );
    RawEventLanceStore
        .append_events(
            &updated_coords,
            &[EventRecord {
                identity: EventIdentity::default(),
                seq: 1,
                source: "test".into(),
                kind: "note".into(),
                timestamp: None,
                session_id: Some("root".into()),
                agent_id: Some("agent".into()),
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({"after": "snapshot"}),
            }],
        )
        .await?;
    let lazy = &snapshot.prepared[0].sources[0];
    assert_eq!(lazy.resolution_count.load(Ordering::Relaxed), 0);
    let engine = snapshot.clone().query_engine(Default::default()).await?;
    assert_eq!(lazy.resolution_count.load(Ordering::Relaxed), 0);
    let event_count = engine
        .query_jsonl(
            "SELECT COUNT(*) AS rows FROM dataset.events \
                 WHERE _file_ = 'agent/run-1/events.lance' AND seq = 0",
        )
        .await?;
    assert_eq!(event_count.trim(), r#"{"rows":2}"#);
    assert_eq!(lazy.resolution_count.load(Ordering::Relaxed), 1);
    let resolved = lazy.resolved.get().unwrap().as_ref().unwrap();
    let ResolvedSource::Events(events) = resolved.as_ref() else {
        panic!("canonical event source resolved to the wrong adapter");
    };
    assert_eq!(events.normalization_count.load(Ordering::Relaxed), 0);
    let runs = engine
        .query_jsonl(
            "SELECT _file_, document_id, run_id, session_id FROM dataset.runs ORDER BY session_id",
        )
        .await?;
    assert_eq!(events.normalization_count.load(Ordering::Relaxed), 0);
    let keys = runs
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert_eq!(keys.len(), 2);
    for row in keys {
        let events = snapshot
            .load_events(&CatalogStorylineKey {
                dataset: DEFAULT_DATASET_NAME.into(),
                file: row["_file_"].as_str().unwrap().into(),
                document_id: row["document_id"].as_str().unwrap().into(),
                session_id: row["session_id"].as_str().unwrap().into(),
            })
            .await?
            .context("Catalog Storyline must resolve canonical events")?;
        assert_eq!(events.events.len(), 1);
    }
    assert_eq!(events.normalization_count.load(Ordering::Relaxed), 0);

    let stale_snapshot = Arc::new(
        DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(storage.to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?,
    );
    assert_eq!(stale_snapshot.datasets()[0].sources.len(), 1);
    assert_eq!(
        stale_snapshot.datasets()[0].sources[0].projection_status,
        Some(CatalogProjectionStatus::Stale)
    );
    let stale_engine = stale_snapshot
        .clone()
        .query_engine(Default::default())
        .await?;
    let stale_resolved = stale_snapshot.prepared[0].sources[0].resolve().await?;
    let ResolvedSource::Events(stale_events) = stale_resolved.as_ref() else {
        panic!("stale projection did not fall back to canonical events");
    };
    stale_snapshot
        .load_events(&CatalogStorylineKey {
            dataset: DEFAULT_DATASET_NAME.into(),
            file: "agent/run-1/events.lance".into(),
            document_id: "root".into(),
            session_id: "root".into(),
        })
        .await?
        .context("point load must resolve the selected Storyline")?;
    assert_eq!(stale_events.normalization_count.load(Ordering::Relaxed), 0);
    stale_engine
        .query("SELECT * FROM dataset.runs WHERE session_id = 'root'")
        .await?;
    assert_eq!(stale_events.normalization_count.load(Ordering::Relaxed), 1);
    let broad_fallback = stale_engine
        .query_jsonl("SELECT session_id FROM dataset.runs ORDER BY session_id")
        .await?;
    assert_eq!(
        broad_fallback.lines().collect::<Vec<_>>(),
        [r#"{"session_id":"child"}"#, r#"{"session_id":"root"}"#]
    );
    assert_eq!(stale_events.normalization_count.load(Ordering::Relaxed), 2);

    let limited_snapshot = Arc::new(
        DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(storage.to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions {
                max_event_fallback_rows: 1,
                ..CatalogSnapshotOptions::default()
            },
        )
        .await?,
    );
    let limited_engine = limited_snapshot.query_engine(Default::default()).await?;
    let limit_error = limited_engine
        .query("SELECT * FROM dataset.runs")
        .await
        .unwrap_err();
    assert!(format!("{limit_error:#}").contains("max_event_fallback_rows 1"));

    let StorylineProjectionSyncOutcome::Synced(sync) = sync_storyline_projection(
        events_uri.to_string_lossy(),
        projection_uri.to_string_lossy(),
    )
    .await?
    else {
        panic!("incremental sync returned a non-success outcome")
    };
    assert_eq!(sync.mode, StorylineProjectionSyncMode::Incremental);
    assert_eq!(sync.affected_storylines, 1);
    assert_eq!(sync.suffix_rows_scanned, 1);
    assert_eq!(sync.history_rows_scanned, 2);
    let StorylineProjectionSyncOutcome::Synced(noop) = sync_storyline_projection(
        events_uri.to_string_lossy(),
        projection_uri.to_string_lossy(),
    )
    .await?
    else {
        panic!("noop sync returned a non-success outcome")
    };
    assert_eq!(noop.mode, StorylineProjectionSyncMode::Noop);
    assert_eq!(noop.generation, sync.generation);
    assert_eq!(noop.suffix_rows_scanned, 0);
    assert_eq!(noop.history_rows_scanned, 0);

    let projection_store = StorylineLanceStore::open(&projection_uri).await?;
    let before_rebuild = projection_store
        .current_table_paths()
        .await?
        .context("synced projection must have CURRENT")?;
    let rebuild = rebuild_storyline_projection(
        events_uri.to_string_lossy(),
        projection_uri.to_string_lossy(),
        "agent/run-1/events.lance",
    )
    .await?;
    assert_eq!(rebuild.mode, StorylineProjectionSyncMode::Rebuild);
    let after_rebuild = projection_store
        .current_table_paths()
        .await?
        .context("rebuilt projection must have CURRENT")?;
    assert_ne!(
        before_rebuild.table_generation,
        after_rebuild.table_generation
    );
    assert_ne!(before_rebuild.generation, after_rebuild.generation);
    Ok(())
}

#[tokio::test]
async fn multiple_fresh_projections_choose_one_without_hiding_canonical_events() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("capture");
    let coords = StoryCoords::new(
        storage.to_string_lossy(),
        "agent",
        "root",
        Some("run-1".into()),
    );
    RawEventLanceStore
        .append_events(
            &coords,
            &[EventRecord {
                identity: EventIdentity::default(),
                seq: 0,
                source: "test".into(),
                kind: "note".into(),
                timestamp: None,
                session_id: Some("root".into()),
                agent_id: Some("agent".into()),
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({"content": "kept"}),
            }],
        )
        .await?;
    let events_uri =
        story_lance_event_path(&storage.to_string_lossy(), "agent", "root", Some("run-1"))?;
    for name in ["storyline-a", "storyline-b"] {
        let StorylineProjectionBuildOutcome::Built(_) = build_storyline_projection(
            events_uri.to_string_lossy(),
            storage.join("agent/run-1").join(name).to_string_lossy(),
            "agent/run-1/events.lance",
        )
        .await?
        else {
            panic!("catalog projection candidate build unexpectedly reported nonempty output")
        };
    }

    let snapshot = DatasetCatalogSnapshot::discover(
        vec![DatasetMount::default(storage.to_string_lossy())?],
        Some(DEFAULT_DATASET_NAME.into()),
        CatalogSnapshotOptions::default(),
    )
    .await?;
    assert_eq!(snapshot.datasets()[0].sources.len(), 1);
    let source = &snapshot.datasets()[0].sources[0];
    assert_eq!(source.file, "agent/run-1/events.lance");
    assert_eq!(
        source.projection_status,
        Some(CatalogProjectionStatus::Fresh)
    );
    assert_eq!(source.projection_candidates, 2);
    assert!(matches!(
        source.revision,
        Some(CatalogSourceRevision::Events { .. })
    ));
    Ok(())
}

#[test]
fn report_mode_source_status_does_not_serialize_operational_diagnostics() -> Result<()> {
    let stub = DiscoveredSource {
        file: "broken.json".into(),
        format: None,
        kind: CatalogSourceKind::File,
        revision: None,
        projection_status: None,
        projection_generation: None,
        projection_candidates: 0,
        size_bytes: None,
        last_modified: None,
        status: CatalogSourceStatus::Ready,
        error: None,
    };
    let source = reported_source_failure(
        stub,
        anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "catalog-secret-sentinel /private/catalog/path",
        ))
        .context("freeze catalog source"),
    );

    let output = serde_json::to_string(&source)?;
    assert_eq!(source.error.as_deref(), Some("Source discovery failed"));
    assert!(!output.contains("catalog-secret-sentinel"), "{output}");
    assert!(!output.contains("/private/catalog/path"), "{output}");
    Ok(())
}

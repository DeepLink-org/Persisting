use super::content::CONTENT_REF_MAGIC;
use super::*;
use crate::{StorylineAgent, StorylineToolCall, StorylineTurn};

fn remote_uri(label: &str) -> String {
    format!(
        "shared-memory://pchronicle-storyline-{}-{label}-{}/root",
        std::process::id(),
        NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
    )
}

fn projection_lineage() -> StorylineProjectionLineage {
    StorylineProjectionLineage {
        source_id: "source-1".into(),
        source_file: "agent/run/events.lance".into(),
        source: ProjectionSourceSnapshot::CanonicalEvents {
            source_uri: "/tmp/events.lance".into(),
            fact_version: 7,
            fact_rows: 42,
            layout_revision: 9,
        },
        projector_name: "events-storyline".into(),
        recipe_hash: "blake3:recipe".into(),
        completeness: "full".into(),
    }
}

#[test]
fn non_create_publication_mismatch_is_an_operational_error() {
    let error = published_storyline_report(StorylineProjectionPublicationOutcome::OutputNotEmpty)
        .unwrap_err();

    assert!(error
        .to_string()
        .contains("non-create Storyline publication reported nonempty output"));
}

async fn put_remote_object(uri: &str, relative: &str, contents: &[u8]) {
    let (store, root) = ObjectStore::from_uri(uri).await.unwrap();
    store.put(&root.join(relative), contents).await.unwrap();
}

struct CreateAfterEmptyReadBarrier;

impl CreateAfterEmptyReadBarrier {
    fn install(root_uri: &str, parties: usize) -> Self {
        *CREATE_AFTER_EMPTY_READ_BARRIER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(CreateAfterEmptyReadBarrierHook {
                root_uri: root_uri.to_string(),
                barrier: Arc::new(tokio::sync::Barrier::new(parties)),
                content_arrivals: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                content_created: Arc::new(tokio::sync::Notify::new()),
            });
        Self
    }
}

struct ReplacementAfterCurrentReadBarrier;

impl ReplacementAfterCurrentReadBarrier {
    fn install(root_uri: &str, parties: usize) -> Self {
        *REPLACEMENT_AFTER_CURRENT_READ_BARRIER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(ReplacementAfterCurrentReadBarrierHook {
                root_uri: root_uri.to_string(),
                barrier: Arc::new(tokio::sync::Barrier::new(parties)),
            });
        Self
    }
}

impl Drop for ReplacementAfterCurrentReadBarrier {
    fn drop(&mut self) {
        *REPLACEMENT_AFTER_CURRENT_READ_BARRIER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

impl Drop for CreateAfterEmptyReadBarrier {
    fn drop(&mut self) {
        *CREATE_AFTER_EMPTY_READ_BARRIER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

fn story(session_id: &str) -> StorylineDocument {
    StorylineDocument {
        schema_version: crate::model::STORYLINE_SCHEMA_VERSION.into(),
        origin: None,
        run_id: Some("run-1".into()),
        trajectory_id: None,
        attempt_id: None,
        session_id: session_id.into(),
        agent: StorylineAgent {
            id: "agent-1".into(),
            name: Some("Agent".into()),
            version: Some("1".into()),
            model_name: Some("model".into()),
            tool_definitions: Some(serde_json::json!([{"name": "lookup"}])),
            extra: None,
        },
        parent: None,
        child_session_ids: None,
        notes: Some("test".into()),
        final_metrics: None,
        continued_trajectory_ref: None,
        extra: None,
        unknown_fields: Default::default(),
        unknown_key_counts: Default::default(),
        turns: vec![
            StorylineTurn {
                id: 1,
                kind: None,
                timestamp: Some(
                    crate::model::StorylineTimestamp::from_rfc3339("2026-01-01T00:00:00Z").unwrap(),
                ),
                source: "user".into(),
                message: serde_json::json!("price?"),
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
            },
            StorylineTurn {
                id: 2,
                kind: Some("autonomous".into()),
                timestamp: None,
                source: "agent".into(),
                message: serde_json::json!("checking"),
                reasoning_content: Some("need tool".into()),
                reasoning_effort: None,
                tool_calls: Some(vec![StorylineToolCall {
                    tool_call_id: "call-1".into(),
                    function_name: "lookup".into(),
                    arguments: serde_json::json!({"symbol": "ACME"}),
                    result: Default::default(),
                    duration_ms: Some(12),
                    extra: None,
                }]),
                observation: Some(serde_json::json!({
                    "results": [{"source_call_id": "call-1", "content": "42"}]
                })),
                metrics: None,
                model_name: Some("model".into()),
                llm_call_count: Some(1),
                is_copied_context: Some(false),
                latency_ms: Some(20),
                ttft_ms: Some(5),
                extra: None,
            },
        ],
    }
}

#[test]
fn unknown_field_limit_options_allow_unbounded_count_and_bytes() {
    for options in [
        StorylineContentOptions {
            max_unknown_fields: 0,
            ..Default::default()
        },
        StorylineContentOptions {
            max_unknown_bytes: 0,
            ..Default::default()
        },
    ] {
        assert!(options.validate().is_err());
    }
    assert!(StorylineContentOptions {
        max_unknown_fields: usize::MAX,
        max_unknown_bytes: usize::MAX,
        ..Default::default()
    }
    .validate()
    .is_ok());
}

#[tokio::test]
async fn configured_unknown_field_limit_reports_actual_and_limit() {
    let temporary = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open_with_content_options(
        temporary.path(),
        StorylineContentOptions {
            max_unknown_fields: 1,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut oversized = story("unknown-count-limit");
    oversized
        .unknown_fields
        .insert("atif", "source", "/one", serde_json::json!(1))
        .unwrap();
    oversized
        .unknown_fields
        .insert("atif", "source", "/two", serde_json::json!(2))
        .unwrap();
    oversized.refresh_unknown_key_counts().unwrap();

    let error = store.replace_storyline(&oversized).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unknown field count 2 exceeds configured limit 1"),
        "{error:#}"
    );
}

#[tokio::test]
async fn repeated_unknown_value_is_stored_once() {
    let large = serde_json::json!({
        "payload": "x".repeat(DEFAULT_CONTENT_OFFLOAD_THRESHOLD)
    });
    let mut first = story("unknown-first");
    first
        .unknown_fields
        .insert("actf", "task-1", "/shared", large.clone())
        .unwrap();
    first.refresh_unknown_key_counts().unwrap();
    let mut second = story("unknown-second");
    second
        .unknown_fields
        .insert("actf", "task-1", "/shared", large.clone())
        .unwrap();
    second.refresh_unknown_key_counts().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(temporary.path()).await.unwrap();
    store.replace_storylines(&[first, second]).await.unwrap();

    let paths = store.current_table_paths().await.unwrap().unwrap();
    let objects = open_objects(&paths.objects, paths.objects_version)
        .await
        .unwrap();
    assert_eq!(objects.count_rows(None).await.unwrap(), 1);
    let hydrated = store
        .get_storyline_full("unknown-first")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        hydrated.unknown_fields.sources["actf"].fields["/shared"],
        large
    );
}

#[tokio::test]
async fn unknown_content_ref_magic_string_round_trips_as_literal() {
    let literal = format!("{CONTENT_REF_MAGIC}user-controlled-not-a-descriptor");
    let mut expected = story("unknown-magic");
    expected
        .unknown_fields
        .insert("atif", "source", "/literal", serde_json::json!(literal))
        .unwrap();
    expected.refresh_unknown_key_counts().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open_with_content_options(
        temporary.path(),
        StorylineContentOptions {
            offload_threshold: usize::MAX,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store.replace_storyline(&expected).await.unwrap();
    assert_eq!(
        store.get_storyline_full("unknown-magic").await.unwrap(),
        Some(expected)
    );
}

#[tokio::test]
async fn default_store_accepts_large_compressible_unknown_value() {
    let mut expected = story("large-logical-unknown");
    expected
        .unknown_fields
        .insert(
            "atif",
            "source",
            "/payload",
            serde_json::json!("x".repeat(1024 * 1024 + 1)),
        )
        .unwrap();
    expected.refresh_unknown_key_counts().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(temporary.path()).await.unwrap();

    store.replace_storyline(&expected).await.unwrap();
    assert!(
        store.current_table_paths().await.unwrap().is_some(),
        "large unknown field should be committed"
    );
    assert_eq!(
        store
            .get_storyline_full("large-logical-unknown")
            .await
            .unwrap(),
        Some(expected)
    );
}

#[tokio::test]
async fn logical_unknown_limit_rejects_hydrated_value_on_read() {
    let temporary = tempfile::tempdir().unwrap();
    let writer = StorylineLanceStore::open_with_content_options(
        temporary.path(),
        StorylineContentOptions {
            offload_threshold: 1,
            max_unknown_bytes: 4096,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut stored = story("logical-read-limit");
    stored
        .unknown_fields
        .insert(
            "atif",
            "source",
            "/payload",
            serde_json::json!("x".repeat(128)),
        )
        .unwrap();
    stored.refresh_unknown_key_counts().unwrap();
    writer.replace_storyline(&stored).await.unwrap();

    let reader = StorylineLanceStore::open_with_content_options(
        temporary.path(),
        StorylineContentOptions {
            max_unknown_bytes: 16,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let error = reader
        .get_storyline_full("logical-read-limit")
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("unknown field byte size")
            && error.to_string().contains("exceeds configured limit 16"),
        "{error:#}"
    );
}

#[tokio::test]
async fn persists_three_tables_and_round_trips_storyline() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();
    let expected = story("session-1");
    store.replace_storyline(&expected).await.unwrap();

    let paths = store.current_table_paths().await.unwrap().unwrap();
    assert!(paths.runs.is_dir());
    assert!(paths.steps.is_dir());
    assert!(paths.tool_calls.is_dir());
    assert_eq!(
        store.get_storyline_full("session-1").await.unwrap(),
        Some(expected)
    );
}

#[tokio::test]
async fn projection_lineage_is_atomic_preserved_by_maintenance_and_cleared_by_direct_write() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();
    let lineage = projection_lineage();
    store
        .replace_projected_storylines(&[story("projected")], lineage.clone())
        .await
        .unwrap();
    assert_eq!(
        store
            .current_table_paths()
            .await
            .unwrap()
            .unwrap()
            .projection,
        Some(lineage.clone())
    );

    store
        .maintain(&LanceMaintenanceOptions {
            compact: false,
            optimize_indices: false,
            vacuum_older_than: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .current_table_paths()
            .await
            .unwrap()
            .unwrap()
            .projection,
        Some(lineage)
    );

    store.replace_storyline(&story("direct")).await.unwrap();
    assert!(store
        .current_table_paths()
        .await
        .unwrap()
        .unwrap()
        .projection
        .is_none());
}

#[tokio::test]
async fn large_content_is_lossless_deduplicated_and_never_exposed_as_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    let options = StorylineContentOptions {
        offload_threshold: 64,
        preview_bytes: 24,
        ..Default::default()
    };
    let store = StorylineLanceStore::open_with_content_options(dir.path(), options)
        .await
        .unwrap();
    let large = "shared large content ".repeat(128);
    let mut first = story("large-a");
    first.notes = Some(large.clone());
    let mut second = story("large-b");
    second.notes = Some(large.clone());
    store
        .replace_storylines(&[first.clone(), second.clone()])
        .await
        .unwrap();

    let paths = store.current_table_paths().await.unwrap().unwrap();
    let objects = open_objects(&paths.objects, paths.objects_version)
        .await
        .unwrap();
    assert_eq!(objects.count_rows(None).await.unwrap(), 1);

    let raw_runs = read_projected_batches(&paths.runs, paths.runs_version, &[], None)
        .await
        .unwrap();
    let raw_notes = raw_runs[0]
        .column_by_name("notes")
        .unwrap()
        .as_any()
        .downcast_ref::<lance::deps::arrow_array::StringArray>()
        .unwrap();
    assert!(raw_notes.value(0).starts_with(CONTENT_REF_MAGIC));
    assert_eq!(
        store.get_storyline_full("large-a").await.unwrap(),
        Some(first)
    );
    assert_eq!(
        store.get_storyline_full("large-b").await.unwrap(),
        Some(second)
    );

    let source = super::datafusion::StorylineDataSource::open(dir.path())
        .await
        .unwrap();
    let context = source.session_context().unwrap();
    let metadata = context
        .sql("SELECT session_id FROM runs ORDER BY session_id")
        .await
        .unwrap();
    let metadata_plan = metadata.clone().create_physical_plan().await.unwrap();
    let metadata_plan = ::datafusion::physical_plan::displayable(metadata_plan.as_ref())
        .indent(true)
        .to_string();
    assert!(
        !metadata_plan.contains("ContentHydrationExec"),
        "{metadata_plan}"
    );

    let escaped = large.replace('\'', "''");
    let filtered = context
        .sql(&format!(
            "SELECT notes FROM runs WHERE notes = '{escaped}' ORDER BY session_id"
        ))
        .await
        .unwrap();
    let filtered_plan = filtered.clone().create_physical_plan().await.unwrap();
    let filtered_plan = ::datafusion::physical_plan::displayable(filtered_plan.as_ref())
        .indent(true)
        .to_string();
    assert!(
        filtered_plan.contains("ContentHydrationExec"),
        "{filtered_plan}"
    );
    let batches = filtered.collect().await.unwrap();
    assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 2);
    for batch in batches {
        let notes = batch
            .column_by_name("notes")
            .unwrap()
            .as_any()
            .downcast_ref::<lance::deps::arrow_array::StringArray>()
            .unwrap();
        assert!(notes.iter().flatten().all(|value| value == large));
    }
    let count = context
        .sql(&format!(
            "SELECT COUNT(*) AS matches FROM runs WHERE notes = '{escaped}'"
        ))
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    let matches = count[0]
        .column_by_name("matches")
        .unwrap()
        .as_any()
        .downcast_ref::<lance::deps::arrow_array::Int64Array>()
        .unwrap();
    assert_eq!(matches.value(0), 2);

    let preview_source = super::datafusion::StorylineDataSource::open_with_options(
        dir.path(),
        super::datafusion::StorylineDataSourceOptions {
            content_read_mode: super::datafusion::StorylineContentReadMode::Preview,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let preview_context = preview_source.session_context().unwrap();
    let preview = preview_context
        .sql("SELECT notes FROM runs WHERE session_id = 'large-a'")
        .await
        .unwrap();
    let preview_plan = preview.clone().create_physical_plan().await.unwrap();
    let preview_plan = ::datafusion::physical_plan::displayable(preview_plan.as_ref())
        .indent(true)
        .to_string();
    assert!(preview_plan.contains("mode=preview"), "{preview_plan}");
    let preview = preview.collect().await.unwrap();
    let notes = preview[0]
        .column_by_name("notes")
        .unwrap()
        .as_any()
        .downcast_ref::<lance::deps::arrow_array::StringArray>()
        .unwrap();
    assert_eq!(notes.value(0), &large[..24]);
    let preview_filter_error = preview_context
        .sql(&format!(
            "SELECT session_id FROM runs WHERE notes = '{escaped}'"
        ))
        .await
        .unwrap()
        .collect()
        .await
        .unwrap_err();
    assert!(
        preview_filter_error
            .to_string()
            .contains("content predicates require full"),
        "{preview_filter_error}"
    );
}

#[tokio::test]
async fn maintenance_prunes_objects_unreachable_from_current_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let options = StorylineContentOptions {
        offload_threshold: 32,
        ..Default::default()
    };
    let store = StorylineLanceStore::open_with_content_options(dir.path(), options)
        .await
        .unwrap();
    let mut document = story("gc");
    document.notes = Some("old unreachable content ".repeat(64));
    store.replace_storyline(&document).await.unwrap();
    document.notes = Some("new live content ".repeat(64));
    store.replace_storyline(&document).await.unwrap();

    let before = store.current_table_paths().await.unwrap().unwrap();
    let before_objects = open_objects(&before.objects, before.objects_version)
        .await
        .unwrap()
        .count_rows(None)
        .await
        .unwrap();
    let report = store
        .maintain(&LanceMaintenanceOptions {
            vacuum_older_than: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(report.objects_removed, 1);
    let after = store.current_table_paths().await.unwrap().unwrap();
    let after_objects = open_objects(&after.objects, after.objects_version)
        .await
        .unwrap()
        .count_rows(None)
        .await
        .unwrap();
    assert_eq!(after_objects + 1, before_objects);
    assert_eq!(
        store.get_storyline_full("gc").await.unwrap(),
        Some(document)
    );
}

#[tokio::test]
async fn maintenance_vacuums_unreferenced_objects() {
    let dir = tempfile::tempdir().unwrap();
    let options = StorylineContentOptions {
        offload_threshold: 32,
        ..Default::default()
    };
    let store = StorylineLanceStore::open_with_content_options(dir.path(), options)
        .await
        .unwrap();
    let mut document = story("vacuum-objects");
    document.notes = Some("old unreachable object ".repeat(64));
    store.replace_storyline(&document).await.unwrap();
    document.notes = Some("new live object ".repeat(64));
    store.replace_storyline(&document).await.unwrap();

    let report = store
        .maintain(&LanceMaintenanceOptions {
            vacuum_older_than: Some(std::time::Duration::ZERO),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(report.objects_removed, 1);
    assert!(report.objects.old_versions_removed > 0, "{report:?}");
    assert!(report.objects.bytes_removed > 0, "{report:?}");
    assert_eq!(
        store.get_storyline_full("vacuum-objects").await.unwrap(),
        Some(document)
    );
}

#[tokio::test]
async fn maintenance_prunes_expired_physical_generations() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();
    store
        .rebuild_projected_storyline_stream(vec![Ok(story("generation-one"))], projection_lineage())
        .await
        .unwrap();
    let first_table_generation = store
        .current_table_paths()
        .await
        .unwrap()
        .unwrap()
        .table_generation;
    store
        .rebuild_projected_storyline_stream(vec![Ok(story("generation-two"))], projection_lineage())
        .await
        .unwrap();
    let current_table_generation = store
        .current_table_paths()
        .await
        .unwrap()
        .unwrap()
        .table_generation;
    assert_ne!(first_table_generation, current_table_generation);

    let malformed = dir
        .path()
        .join(GENERATIONS_DIR)
        .join("not-owned-by-storyline");
    std::fs::create_dir_all(&malformed).unwrap();
    std::fs::write(malformed.join("keep"), b"not a Storyline generation").unwrap();

    let no_vacuum = store
        .maintain(&LanceMaintenanceOptions {
            vacuum_older_than: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(no_vacuum.generations_removed, 0);
    assert!(dir
        .path()
        .join(GENERATIONS_DIR)
        .join(&first_table_generation)
        .exists());

    let vacuumed = store
        .maintain(&LanceMaintenanceOptions {
            vacuum_older_than: Some(std::time::Duration::ZERO),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(vacuumed.generations_removed, 1);
    assert!(!dir
        .path()
        .join(GENERATIONS_DIR)
        .join(first_table_generation)
        .exists());
    assert!(dir
        .path()
        .join(GENERATIONS_DIR)
        .join(current_table_generation)
        .exists());
    assert!(malformed.exists());
}

#[tokio::test]
async fn content_descriptor_magic_in_user_text_round_trips_as_literal() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open_with_content_options(
        dir.path(),
        StorylineContentOptions {
            offload_threshold: usize::MAX,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let literal = format!("{CONTENT_REF_MAGIC}user-controlled-not-a-descriptor");
    let mut expected = story("magic");
    expected.notes = Some(literal);
    store.replace_storyline(&expected).await.unwrap();
    assert_eq!(
        store.get_storyline_full("magic").await.unwrap(),
        Some(expected)
    );
}

#[tokio::test]
async fn empty_storyline_still_creates_queryable_tables() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();
    let mut expected = story("empty");
    expected.turns.clear();
    store.replace_storyline(&expected).await.unwrap();

    let paths = store.current_table_paths().await.unwrap().unwrap();
    assert_eq!(
        Dataset::open(paths.steps.to_string_lossy().as_ref())
            .await
            .unwrap()
            .count_rows(None)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        Dataset::open(paths.tool_calls.to_string_lossy().as_ref())
            .await
            .unwrap()
            .count_rows(None)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store.get_storyline_full("empty").await.unwrap(),
        Some(expected)
    );
}

#[tokio::test]
async fn replacement_is_session_scoped_and_switches_generation() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();
    store.replace_storyline(&story("a")).await.unwrap();
    let first = store.current_table_paths().await.unwrap().unwrap();
    store.replace_storyline(&story("b")).await.unwrap();
    let second = store.current_table_paths().await.unwrap().unwrap();
    assert_ne!(first.generation, second.generation);
    assert_eq!(first.table_generation, second.table_generation);
    assert_eq!(first.runs, second.runs);
    assert_eq!(first.steps, second.steps);
    assert_eq!(first.tool_calls, second.tool_calls);
    assert!(second.runs_version > first.runs_version);
    assert!(second.steps_version > first.steps_version);
    assert!(second.tool_calls_version > first.tool_calls_version);
    assert!(store.get_storyline_full("a").await.unwrap().is_some());
    assert!(store.get_storyline_full("b").await.unwrap().is_some());

    let mut updated = story("a");
    updated.notes = Some("updated".into());
    updated.turns.truncate(1);
    store.replace_storyline(&updated).await.unwrap();
    assert_eq!(store.get_storyline_full("a").await.unwrap(), Some(updated));
}

#[tokio::test]
async fn batch_replace_commits_once_and_rejects_duplicate_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();
    let stories = vec![story("a"), story("b")];
    store.replace_storylines(&stories).await.unwrap();
    let committed = store
        .current_table_paths()
        .await
        .unwrap()
        .unwrap()
        .generation;
    assert!(store.get_storyline_full("a").await.unwrap().is_some());
    assert!(store.get_storyline_full("b").await.unwrap().is_some());

    let duplicate = vec![story("same"), story("same")];
    assert!(store.replace_storylines(&duplicate).await.is_err());
    assert_eq!(
        store
            .current_table_paths()
            .await
            .unwrap()
            .unwrap()
            .generation,
        committed
    );
}

#[tokio::test]
async fn batch_get_preserves_request_order_and_missing_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();
    let first = story("a");
    let second = story("b");
    store
        .replace_storylines(&[first.clone(), second.clone()])
        .await
        .unwrap();

    let actual = store
        .get_storylines_full(&["b".into(), "missing".into(), "a".into()])
        .await
        .unwrap();
    assert_eq!(actual, [Some(second), None, Some(first)]);
    assert!(store
        .get_storylines_full(&["a".into(), "a".into()])
        .await
        .unwrap_err()
        .to_string()
        .contains("duplicate session_id"));
}

#[test]
fn storyline_import_limits_reject_zero() {
    let cases = [
        (
            StorylineContentOptions {
                max_document_rows: Some(0),
                ..Default::default()
            },
            "max_document_rows",
        ),
        (
            StorylineContentOptions {
                max_document_bytes: Some(0),
                ..Default::default()
            },
            "max_document_bytes",
        ),
        (
            StorylineContentOptions {
                max_chunk_rows: Some(0),
                ..Default::default()
            },
            "max_chunk_rows",
        ),
        (
            StorylineContentOptions {
                max_chunk_bytes: Some(0),
                ..Default::default()
            },
            "max_chunk_bytes",
        ),
        (
            StorylineContentOptions {
                max_import_documents: Some(0),
                ..Default::default()
            },
            "max_import_documents",
        ),
    ];

    for (options, name) in cases {
        let error = options.validate().unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("{name} must be positive")),
            "{error:#}"
        );
    }
}

#[tokio::test]
async fn document_limit_failure_keeps_current_generation() {
    let dir = tempfile::tempdir().unwrap();
    let baseline = StorylineLanceStore::open(dir.path()).await.unwrap();
    baseline
        .replace_storyline(&story("baseline"))
        .await
        .unwrap();
    let generation = baseline
        .current_table_paths()
        .await
        .unwrap()
        .unwrap()
        .generation;
    let limited = StorylineLanceStore::open_with_content_options(
        dir.path(),
        StorylineContentOptions {
            max_document_rows: Some(3),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let error = limited
        .replace_storyline(&story("oversized"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("max_document_rows"), "{error:#}");
    assert_eq!(
        limited
            .current_table_paths()
            .await
            .unwrap()
            .unwrap()
            .generation,
        generation
    );
    assert!(limited
        .get_storyline_full("oversized")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn document_byte_limit_rejects_oversized_storyline() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open_with_content_options(
        dir.path(),
        StorylineContentOptions {
            max_document_bytes: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let error = store
        .replace_storyline(&story("byte-limited"))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("max_document_bytes"),
        "{error:#}"
    );
    assert!(store.current_table_paths().await.unwrap().is_none());
}

#[test]
fn single_document_must_fit_chunk_limits() {
    let document = story("single-chunk-limit");
    let mut iterator = vec![Ok(document)].into_iter();
    let mut state = StorylineChunkState::default();
    let mut ordinal = 0;

    let error = next_storyline_stream_chunk(
        &mut iterator,
        &mut state,
        &mut ordinal,
        StorylineContentOptions {
            max_chunk_rows: Some(3),
            ..Default::default()
        },
    )
    .err()
    .expect("single document must exceed max_chunk_rows");
    assert!(error.to_string().contains("max_chunk_rows"), "{error:#}");

    let document = story("single-byte-chunk-limit");
    let mut iterator = vec![Ok(document)].into_iter();
    let mut state = StorylineChunkState::default();
    let error = next_storyline_stream_chunk(
        &mut iterator,
        &mut state,
        &mut ordinal,
        StorylineContentOptions {
            max_chunk_bytes: Some(1),
            ..Default::default()
        },
    )
    .err()
    .expect("single document must exceed max_chunk_bytes");
    assert!(error.to_string().contains("max_chunk_bytes"), "{error:#}");
}

#[test]
fn chunk_limits_preserve_the_next_document() {
    let mut first = story("chunk-a");
    first.turns.clear();
    let mut second = story("chunk-b");
    second.turns.clear();
    let mut third = story("chunk-c");
    third.turns.clear();
    let document_bytes = serde_json::to_vec(&first).unwrap().len();
    let mut iterator = vec![Ok(first), Ok(second), Ok(third)].into_iter();
    let mut state = StorylineChunkState::default();
    let mut ordinal = 0;
    let options = StorylineContentOptions {
        max_document_rows: Some(1),
        max_document_bytes: Some(document_bytes + 8),
        max_chunk_rows: Some(2),
        max_chunk_bytes: Some((document_bytes + 8) * 2),
        max_import_documents: Some(3),
        ..Default::default()
    };

    let first_chunk = next_storyline_stream_chunk(&mut iterator, &mut state, &mut ordinal, options)
        .unwrap()
        .unwrap();
    assert_eq!(first_chunk.runs.len(), 2);
    assert!(state.pending.is_some());

    let second_chunk =
        next_storyline_stream_chunk(&mut iterator, &mut state, &mut ordinal, options)
            .unwrap()
            .unwrap();
    assert_eq!(second_chunk.runs.len(), 1);
    assert!(state.pending.is_none());
    assert!(
        next_storyline_stream_chunk(&mut iterator, &mut state, &mut ordinal, options)
            .unwrap()
            .is_none()
    );
}

#[test]
fn chunk_byte_limit_preserves_the_next_document() {
    let mut first = story("byte-chunk-a");
    first.turns.clear();
    let mut second = story("byte-chunk-b");
    second.turns.clear();
    let document_bytes = serde_json::to_vec(&first).unwrap().len();
    let mut iterator = vec![Ok(first), Ok(second)].into_iter();
    let mut state = StorylineChunkState::default();
    let mut ordinal = 0;
    let options = StorylineContentOptions {
        max_chunk_bytes: Some(document_bytes + 8),
        ..Default::default()
    };

    let first_chunk = next_storyline_stream_chunk(&mut iterator, &mut state, &mut ordinal, options)
        .unwrap()
        .unwrap();
    assert_eq!(first_chunk.runs.len(), 1);
    assert!(state.pending.is_some());

    let second_chunk =
        next_storyline_stream_chunk(&mut iterator, &mut state, &mut ordinal, options)
            .unwrap()
            .unwrap();
    assert_eq!(second_chunk.runs.len(), 1);
    assert!(state.pending.is_none());
}

#[tokio::test]
async fn import_document_limit_failure_keeps_current_generation() {
    let dir = tempfile::tempdir().unwrap();
    let baseline = StorylineLanceStore::open(dir.path()).await.unwrap();
    baseline
        .replace_storyline(&story("baseline"))
        .await
        .unwrap();
    let generation = baseline
        .current_table_paths()
        .await
        .unwrap()
        .unwrap()
        .generation;
    let limited = StorylineLanceStore::open_with_content_options(
        dir.path(),
        StorylineContentOptions {
            max_import_documents: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let error = limited
        .replace_storyline_stream(
            [story("limited-a"), story("limited-b")]
                .into_iter()
                .map(Ok::<_, anyhow::Error>),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("max_import_documents"),
        "{error:#}"
    );
    assert_eq!(
        limited
            .current_table_paths()
            .await
            .unwrap()
            .unwrap()
            .generation,
        generation
    );
}

#[tokio::test]
async fn streamed_replace_is_bounded_and_commits_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();
    let stories = (0..300).map(|index| Ok(story(&format!("stream-{index:03}"))));
    let report = store.replace_storyline_stream(stories).await.unwrap();
    assert_eq!(report.storylines, 300);
    assert_eq!(report.steps, 600);
    assert_eq!(report.tool_calls, 300);
    assert!(!report.generation.is_empty());
    let current = store.current_table_paths().await.unwrap().unwrap();
    assert_eq!(current.generation, report.generation);
    assert_eq!(
        open_table_version(&current.runs, current.runs_version)
            .await
            .unwrap()
            .count_rows(None)
            .await
            .unwrap(),
        300
    );
}

#[tokio::test]
async fn atif_stream_create_writes_one_fragment_per_table() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif");
    let report = store.import_atif_stream(input).await.unwrap();
    assert_eq!(report.storylines, 8);
    assert_eq!(report.steps, 118);
    assert_eq!(report.tool_calls, 23);
    let paths = store.current_table_paths().await.unwrap().unwrap();
    for path in [&paths.runs, &paths.steps, &paths.tool_calls] {
        assert_eq!(
            Dataset::open(path.to_string_lossy().as_ref())
                .await
                .unwrap()
                .get_fragments()
                .len(),
            1
        );
    }

    let empty_tools = tempfile::tempdir().unwrap();
    let empty_tools_store = StorylineLanceStore::open(empty_tools.path()).await.unwrap();
    let dialogue =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif/dialogue_10.json");
    let report = empty_tools_store
        .import_atif_stream(dialogue)
        .await
        .unwrap();
    assert_eq!(report.tool_calls, 0);
    assert!(empty_tools_store
        .get_storyline_full("fixture-dialogue_10")
        .await
        .unwrap()
        .unwrap()
        .turns
        .iter()
        .all(|turn| turn.tool_calls.as_ref().is_none_or(Vec::is_empty)));
}

#[tokio::test]
async fn atif_stream_preserves_nested_documents_with_shared_session() {
    let input = serde_json::json!({
        "schema_version": "ATIF-v1.7",
        "session_id": "shared-run",
        "trajectory_id": "root",
        "agent": {"name": "root", "version": "1"},
        "steps": [],
        "subagent_trajectories": [{
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "child",
            "agent": {"name": "child", "version": "1"},
            "steps": [{
                "step_id": 1,
                "timestamp": "2026-08-14T12:34:56.789123+08:00",
                "source": "agent",
                "message": "done",
                "metrics": null
            }]
        }]
    });
    let file = tempfile::NamedTempFile::with_suffix(".json").unwrap();
    std::fs::write(file.path(), serde_json::to_vec(&input).unwrap()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();

    let canonical_stories = crate::convert::atif_collection_to_storylines(input.clone()).unwrap();
    let canonical = crate::convert::storylines_to_atif(&canonical_stories).unwrap();

    let report = store.import_atif_stream(file.path()).await.unwrap();
    assert_eq!(report.storylines, 2);
    let stories = store
        .get_storylines_by_document_ids(&["root".into(), "child".into()])
        .await
        .unwrap()
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .unwrap();
    let rebuilt = crate::convert::storylines_to_atif(&stories).unwrap();
    assert_eq!(rebuilt, canonical);
}

#[tokio::test]
async fn streamed_replace_error_after_first_chunk_keeps_current() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();
    store.replace_storyline(&story("committed")).await.unwrap();
    let before = store.current_table_paths().await.unwrap().unwrap();
    let stories = (0..257)
        .map(|index| Ok(story(&format!("pending-{index:03}"))))
        .chain(std::iter::once(Err(anyhow::anyhow!("broken stream"))));
    let error = store.replace_storyline_stream(stories).await.unwrap_err();
    assert!(error.to_string().contains("broken stream"));
    let after = store.current_table_paths().await.unwrap().unwrap();
    assert_eq!(before.generation, after.generation);
    assert!(store
        .get_storyline_full("committed")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn invalid_storyline_does_not_move_current_generation() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();
    store.replace_storyline(&story("a")).await.unwrap();
    let before = store
        .current_table_paths()
        .await
        .unwrap()
        .unwrap()
        .generation;
    let mut invalid = story("a");
    invalid.turns[1].id = invalid.turns[0].id;
    assert!(store.replace_storyline(&invalid).await.is_err());
    let after = store
        .current_table_paths()
        .await
        .unwrap()
        .unwrap()
        .generation;
    assert_eq!(before, after);
}

#[tokio::test]
async fn replace_defers_compaction_until_explicit_maintenance() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();
    let expected = story("a");
    store
        .replace_storylines(&[expected.clone(), story("b")])
        .await
        .unwrap();
    let before = store.current_table_paths().await.unwrap().unwrap();
    for session in 0..36 {
        store
            .replace_storyline(&story(&format!("fragment-{session}")))
            .await
            .unwrap();
    }
    let fragmented = store.resolve_current_table_paths().await.unwrap().unwrap();
    assert!(
        open_table_version(&fragmented.runs, fragmented.runs_version)
            .await
            .unwrap()
            .get_fragments()
            .len()
            > 32
    );
    let report = store
        .maintain(&LanceMaintenanceOptions {
            vacuum_older_than: None,
            ..Default::default()
        })
        .await
        .unwrap();
    let after = store.current_table_paths().await.unwrap().unwrap();
    assert_ne!(before.generation, after.generation);
    assert_eq!(
        report.generation.as_deref(),
        Some(after.generation.as_str())
    );
    assert!(report.runs.fragments_removed > 0);
    assert_eq!(store.get_storyline_full("a").await.unwrap(), Some(expected));
}

#[tokio::test]
async fn empty_store_is_queryable_and_empty_batch_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(dir.path()).await.unwrap();
    assert!(store.current_table_paths().await.unwrap().is_none());
    assert!(store.get_storyline_full("missing").await.unwrap().is_none());

    store.replace_storylines(&[]).await.unwrap();
    assert!(store.current_table_paths().await.unwrap().is_none());
}

#[tokio::test]
async fn open_rejects_malformed_or_incomplete_commit_pointer() {
    let invalid = tempfile::tempdir().unwrap();
    tokio::fs::write(invalid.path().join(CURRENT_FILE), "../outside\n")
        .await
        .unwrap();
    let error = StorylineLanceStore::open(invalid.path()).await.unwrap_err();
    assert!(error.to_string().contains("invalid Storyline generation"));

    let incomplete = tempfile::tempdir().unwrap();
    tokio::fs::write(incomplete.path().join(CURRENT_FILE), "gen-missing\n")
        .await
        .unwrap();
    let error = StorylineLanceStore::open(incomplete.path())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("is incomplete"));

    let complete_pointer = tempfile::tempdir().unwrap();
    tokio::fs::write(
        complete_pointer.path().join(CURRENT_FILE),
        serde_json::to_vec(&serde_json::json!({
            "generation": "gen-1-1-1",
            "table_generation": "gen-1-1-1",
            "runs_version": 1,
            "steps_version": 1,
            "tool_calls_version": 1,
            "objects_version": 1
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let error = StorylineLanceStore::open(complete_pointer.path())
        .await
        .unwrap_err();
    assert!(!error.to_string().is_empty());
}

#[tokio::test]
async fn open_rejects_missing_or_unsupported_snapshot_schema_version() {
    for (schema_version, expected) in [
        (None, "schema_version"),
        (
            Some(1),
            "unsupported Storyline Lance schema_version 1; expected 2",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut pointer = serde_json::json!({
            "generation": "gen-1-1-1",
            "table_generation": "gen-1-1-1",
            "runs_version": 1,
            "steps_version": 1,
            "tool_calls_version": 1,
            "objects_version": 1
        });
        if let Some(schema_version) = schema_version {
            pointer["schema_version"] = serde_json::json!(schema_version);
        }
        tokio::fs::write(
            root.path().join(CURRENT_FILE),
            serde_json::to_vec(&pointer).unwrap(),
        )
        .await
        .unwrap();

        let error = StorylineLanceStore::open(root.path()).await.unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
    }
}

#[tokio::test]
async fn object_store_uri_round_trips_across_store_instances() {
    let uri = format!("{}/", remote_uri("round-trip"));
    let store = StorylineLanceStore::open_uri(&uri).await.unwrap();
    assert_eq!(store.storage_scheme(), "shared-memory");
    assert!(!store.root_uri().ends_with('/'));
    store.replace_storyline(&story("remote-1")).await.unwrap();

    let reopened = StorylineLanceStore::open_uri(store.root_uri())
        .await
        .unwrap();
    assert_eq!(
        reopened
            .get_storyline_full("remote-1")
            .await
            .unwrap()
            .unwrap()
            .session_id,
        "remote-1"
    );
    let paths = reopened.current_table_paths().await.unwrap().unwrap();
    assert!(paths
        .runs
        .to_string_lossy()
        .starts_with("shared-memory://pchronicle-storyline-"));
}

#[tokio::test]
async fn object_store_rejects_invalid_utf8_unsafe_and_dangling_current() {
    let cases: [(&str, &[u8], &str); 3] = [
        ("utf8", &[0xff], "not valid UTF-8"),
        ("unsafe", b"../outside\n", "invalid Storyline generation"),
        ("dangling", b"gen-missing\n", "is incomplete"),
    ];
    for (label, contents, expected) in cases {
        let uri = remote_uri(label);
        put_remote_object(&uri, CURRENT_FILE, contents).await;
        let error = StorylineLanceStore::open_uri(&uri).await.unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "unexpected error for {label}: {error:#}"
        );
    }
}

#[tokio::test]
async fn object_store_detects_partially_deleted_generation() {
    let uri = remote_uri("partial-generation");
    let store = StorylineLanceStore::open_uri(&uri).await.unwrap();
    store.replace_storyline(&story("session")).await.unwrap();
    let paths = store.current_table_paths().await.unwrap().unwrap();
    let steps_uri = paths.steps.to_string_lossy().into_owned();
    let (object_store, steps_root) = ObjectStore::from_uri(&steps_uri).await.unwrap();
    object_store.remove_dir_all(steps_root).await.unwrap();

    let error = StorylineLanceStore::open_uri(&uri).await.unwrap_err();
    assert!(error.to_string().contains("is incomplete"), "{error:#}");
}

#[tokio::test]
async fn object_store_prefixes_are_isolated() {
    let left_uri = remote_uri("isolation-left");
    let right_uri = remote_uri("isolation-right");
    let left = StorylineLanceStore::open_uri(&left_uri).await.unwrap();
    let right = StorylineLanceStore::open_uri(&right_uri).await.unwrap();

    left.replace_storyline(&story("left")).await.unwrap();
    assert!(left.get_storyline_full("left").await.unwrap().is_some());
    assert!(right.current_table_paths().await.unwrap().is_none());
    assert!(right.get_storyline_full("left").await.unwrap().is_none());
}

#[tokio::test]
async fn concurrent_object_store_replacements_do_not_lose_sessions() {
    let uri = remote_uri("concurrent");
    let stores = futures::future::join_all((0..6).map(|_| StorylineLanceStore::open_uri(&uri)))
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()
        .unwrap();
    let writes = stores
        .into_iter()
        .enumerate()
        .map(|(index, store)| async move {
            store
                .replace_storyline(&story(&format!("session-{index}")))
                .await
        });
    for result in futures::future::join_all(writes).await {
        result.unwrap();
    }

    let reopened = StorylineLanceStore::open_uri(&uri).await.unwrap();
    let expected = (0..6)
        .map(|index| format!("session-{index}"))
        .collect::<Vec<_>>();
    let sessions = reopened
        .get_storylines_full(&expected)
        .await
        .unwrap()
        .into_iter()
        .map(|story| story.unwrap().session_id)
        .collect::<Vec<_>>();
    assert_eq!(sessions, expected);
}

#[tokio::test]
async fn independent_replacements_conflict_at_current_cas_and_retry_cleanly() {
    let uri = remote_uri("independent-replacement-cas");
    let baseline = story("replacement-baseline");
    let seed = StorylineLanceStore::open_uri(&uri).await.unwrap();
    seed.replace_storyline(&baseline).await.unwrap();

    let mut left = StorylineLanceStore::open_uri(&uri).await.unwrap();
    let mut right = StorylineLanceStore::open_uri(&uri).await.unwrap();
    left.write_lock = Arc::new(tokio::sync::Mutex::new(()));
    right.write_lock = Arc::new(tokio::sync::Mutex::new(()));
    let barrier = ReplacementAfterCurrentReadBarrier::install(&uri, 2);
    let left_story = story("replacement-left");
    let right_story = story("replacement-right");

    let (left_result, right_result) = tokio::join!(
        left.replace_storyline(&left_story),
        right.replace_storyline(&right_story)
    );
    drop(barrier);

    let (winner, loser, conflict) = match (left_result, right_result) {
        (Ok(()), Err(error)) => (&left_story, &right_story, error),
        (Err(error), Ok(())) => (&right_story, &left_story, error),
        (left, right) => panic!("expected one success and one conflict: {left:?}, {right:?}"),
    };
    assert!(
        conflict.to_string().contains("commit conflict"),
        "{conflict:#}"
    );

    let reopened = StorylineLanceStore::open_uri(&uri).await.unwrap();
    assert_eq!(
        reopened
            .get_storyline_full(&baseline.session_id)
            .await
            .unwrap(),
        Some(baseline.clone())
    );
    assert_eq!(
        reopened
            .get_storyline_full(&winner.session_id)
            .await
            .unwrap(),
        Some(winner.clone())
    );
    assert!(reopened
        .get_storyline_full(&loser.session_id)
        .await
        .unwrap()
        .is_none());

    reopened.replace_storyline(loser).await.unwrap();
    assert_eq!(
        reopened
            .get_storylines_full(&[
                baseline.session_id.clone(),
                winner.session_id.clone(),
                loser.session_id.clone(),
            ])
            .await
            .unwrap(),
        [Some(baseline), Some(winner.clone()), Some(loser.clone())]
    );
}

async fn assert_independent_object_store_create_case(
    label: &str,
    content_options: StorylineContentOptions,
    left_story: StorylineDocument,
    right_story: StorylineDocument,
    expect_offloaded_content: bool,
) {
    let uri = remote_uri(label);
    let mut left = StorylineLanceStore::open_uri_with_content_options(&uri, content_options)
        .await
        .unwrap();
    let mut right = StorylineLanceStore::open_uri_with_content_options(&uri, content_options)
        .await
        .unwrap();
    left.write_lock = Arc::new(tokio::sync::Mutex::new(()));
    right.write_lock = Arc::new(tokio::sync::Mutex::new(()));
    let _barrier = CreateAfterEmptyReadBarrier::install(&uri, 2);
    let mut left_lineage = projection_lineage();
    left_lineage.source_file = format!("left-{label}.lance");
    let mut right_lineage = projection_lineage();
    right_lineage.source_file = format!("right-{label}.lance");

    let (left_outcome, right_outcome) = tokio::join!(
        left.create_projected_storyline_stream(
            std::iter::once(Ok(left_story.clone())),
            left_lineage.clone(),
        ),
        right.create_projected_storyline_stream(
            std::iter::once(Ok(right_story.clone())),
            right_lineage.clone(),
        )
    );
    let left_outcome = left_outcome.unwrap();
    let right_outcome = right_outcome.unwrap();

    let (report, winner_lineage, winner, loser_session_id) = match (&left_outcome, &right_outcome) {
        (StorylineProjectionPublicationOutcome::Published(report), _) => (
            report,
            &left_lineage,
            &left_story,
            right_story.session_id.as_str(),
        ),
        (_, StorylineProjectionPublicationOutcome::Published(report)) => (
            report,
            &right_lineage,
            &right_story,
            left_story.session_id.as_str(),
        ),
        _ => panic!("exactly one independent create must publish for {label}"),
    };
    assert!(matches!(
        (&left_outcome, &right_outcome),
        (
            StorylineProjectionPublicationOutcome::Published(_),
            StorylineProjectionPublicationOutcome::OutputNotEmpty
        ) | (
            StorylineProjectionPublicationOutcome::OutputNotEmpty,
            StorylineProjectionPublicationOutcome::Published(_)
        )
    ));

    let reopened = StorylineLanceStore::open_uri(&uri).await.unwrap();
    let current = reopened.current_table_paths().await.unwrap().unwrap();
    assert_eq!(current.generation, report.generation);
    assert_eq!(current.projection.as_ref(), Some(winner_lineage));
    if expect_offloaded_content {
        let objects = open_objects(&current.objects, current.objects_version)
            .await
            .unwrap();
        assert!(objects.count_rows(None).await.unwrap() > 0);
    }
    assert_eq!(
        reopened
            .get_storyline_full(&winner.session_id)
            .await
            .unwrap(),
        Some(winner.clone())
    );
    assert!(reopened
        .get_storyline_full(loser_session_id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn independent_object_store_creates_publish_one_inline_or_offloaded_projection() {
    assert_independent_object_store_create_case(
        "independent-concurrent-create-inline",
        StorylineContentOptions::default(),
        story("left-inline-winner"),
        story("right-inline-winner"),
        false,
    )
    .await;

    let mut left_story = story("left-offloaded-winner");
    left_story.notes = Some("left projection content ".repeat(256));
    let mut right_story = story("right-offloaded-winner");
    right_story.notes = Some("right projection content ".repeat(256));
    assert_independent_object_store_create_case(
        "independent-concurrent-create-offloaded",
        StorylineContentOptions {
            offload_threshold: 1,
            ..Default::default()
        },
        left_story,
        right_story,
        true,
    )
    .await;
}

#[tokio::test]
async fn stale_current_commit_is_rejected_without_moving_snapshot() {
    let uri = remote_uri("stale-current");
    let store = StorylineLanceStore::open_uri(&uri).await.unwrap();
    store.replace_storyline(&story("first")).await.unwrap();
    let stale = store.current_table_paths().await.unwrap().unwrap();

    store.replace_storyline(&story("second")).await.unwrap();
    let committed = store.current_table_paths().await.unwrap().unwrap();
    let attempted_generation = next_generation();
    let error = store
        .commit_snapshot(
            &StorylineSnapshotPointer {
                schema_version: STORYLINE_LANCE_SCHEMA_VERSION,
                generation: attempted_generation,
                parent_generation: Some(stale.generation.clone()),
                table_generation: stale.table_generation.clone(),
                runs_version: stale.runs_version,
                steps_version: stale.steps_version,
                tool_calls_version: stale.tool_calls_version,
                objects_version: stale.objects_version,
                projection: stale.projection.clone(),
            },
            Some(&stale.generation),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("commit conflict"), "{error:#}");

    let after = store.current_table_paths().await.unwrap().unwrap();
    assert_eq!(after.generation, committed.generation);
    assert!(store.get_storyline_full("second").await.unwrap().is_some());
}

#[test]
fn joins_object_store_locations_without_losing_uri_scheme() {
    assert_eq!(
        normalize_root_uri("s3://bucket/trajectory-root///").unwrap(),
        "s3://bucket/trajectory-root"
    );
    assert_eq!(
        join_location(
            "s3://bucket/trajectory-root",
            &["generations", "gen-1", "runs.lance"]
        ),
        "s3://bucket/trajectory-root/generations/gen-1/runs.lance"
    );
    assert_eq!(normalize_root_uri("/").unwrap(), "/");
    assert_eq!(normalize_root_uri("s3://bucket///").unwrap(), "s3://bucket");
    assert_eq!(
        join_location("s3://bucket/轨迹", &["generations", "/gen-1/"]),
        "s3://bucket/轨迹/generations/gen-1"
    );
    assert!(normalize_root_uri("  ").is_err());
}

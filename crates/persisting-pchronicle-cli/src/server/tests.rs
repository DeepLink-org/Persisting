use super::*;
use axum::http::header;
use axum::response::Response;

fn assert_problem(error: ApiError, status: StatusCode, code: BoundaryCode) {
    assert_eq!(error.status, status);
    assert_eq!(error.code, code);
}

async fn response_json(response: Response) -> Value {
    use http_body_util::BodyExt as _;

    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn boundary_maps_explicit_results_and_redacts_failures() {
    assert_eq!(
        serde_json::to_value([
            BoundaryCode::InvalidRequest,
            BoundaryCode::NotFound,
            BoundaryCode::Conflict,
            BoundaryCode::Unsupported,
            BoundaryCode::ResourceExhausted,
            BoundaryCode::Unavailable,
            BoundaryCode::Internal,
        ])
        .unwrap(),
        json!([
            "invalid_request",
            "not_found",
            "conflict",
            "unsupported",
            "resource_exhausted",
            "unavailable",
            "internal"
        ])
    );
    assert_problem(
        ApiError::invalid_request("bad input"),
        StatusCode::BAD_REQUEST,
        BoundaryCode::InvalidRequest,
    );
    assert_problem(
        ApiError::not_found("missing"),
        StatusCode::NOT_FOUND,
        BoundaryCode::NotFound,
    );
    assert_problem(
        ApiError::conflict("stale"),
        StatusCode::CONFLICT,
        BoundaryCode::Conflict,
    );
    assert_problem(
        ApiError::unsupported("format"),
        StatusCode::UNPROCESSABLE_ENTITY,
        BoundaryCode::Unsupported,
    );
    assert_problem(
        ApiError::resource_exhausted("limit"),
        StatusCode::TOO_MANY_REQUESTS,
        BoundaryCode::ResourceExhausted,
    );
    assert_problem(
        ApiError::unavailable(),
        StatusCode::SERVICE_UNAVAILABLE,
        BoundaryCode::Unavailable,
    );

    let response = ApiError::internal(anyhow::anyhow!("/secret/backend")).into_response();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = response_json(response).await;
    assert_eq!(body["code"], "internal");
    assert_eq!(body["message"], "internal server error");
    assert!(!body.to_string().contains("/secret/backend"));
}

#[test]
fn boundary_writer_exhaustion_is_an_explicit_outcome() {
    use std::io::Write as _;

    let mut output = BoundedOutput::new(3);
    let write_result = output
        .write_all(b"four")
        .map_err(anyhow::Error::from)
        .context("opaque writer context");
    assert!(output.exhausted());
    assert!(matches!(
        output.finish(write_result).unwrap(),
        QueryEvidenceWriteOutcome::LimitExceeded
    ));

    let output = BoundedOutput::new(3);
    let error = output
        .finish(Err(anyhow::anyhow!("ordinary writer failure")))
        .unwrap_err();
    assert!(error.to_string().contains("ordinary writer failure"));
}

fn router(storage: impl Into<String>) -> Router {
    let config = ChronicleServerConfig::mounted(vec![
        DatasetMount::default(storage.into()).expect("test Dataset mount must be valid")
    ])
    .expect("test server config must be valid");
    warehouse_router(config)
}

fn test_router_with_config(config: ChronicleServerConfig) -> Router {
    warehouse_router(config)
}

fn test_router_with_catalog_refresh_interval(
    config: ChronicleServerConfig,
    interval: std::time::Duration,
) -> Router {
    read_routes().with_state(app_state_with_catalog_refresh_interval(config, interval))
}

fn json_dataset_root() -> std::path::PathBuf {
    static NEXT_DATASET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let ordinal = NEXT_DATASET.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "pchronicle-query-{}-{unique}-{ordinal}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    write_gateway_fixture(&root, "gateway.json", "json-session", "json-job");
    root
}

fn write_gateway_fixture(root: &std::path::Path, file: &str, session_id: &str, job_id: &str) {
    write_gateway_fixture_with_status(root, file, session_id, job_id, false);
}

fn write_gateway_fixture_with_status(
    root: &std::path::Path,
    file: &str,
    session_id: &str,
    job_id: &str,
    completed: bool,
) {
    std::fs::write(
        root.join(file),
        serde_json::to_vec(&json!([{
            "id":format!("event-{session_id}"),
            "session_id":session_id,
            "step_id":1,
            "agent_model":"model-json",
            "job_id":job_id,
            "is_session_completed":completed,
            "is_terminal":completed,
            "messages":[{"role":"user","content":"hello"}],
            "response":{"role":"assistant","content":"world"}
        }]))
        .unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn warehouse_rejects_non_loopback_bind() {
    let config = ChronicleServerConfig::mounted(vec![
        DatasetMount::default("/tmp/none").expect("test Dataset mount must be valid")
    ])
    .expect("test server config must be valid");
    let error = serve_warehouse(
        config,
        SocketAddr::new(std::net::IpAddr::from([0, 0, 0, 0]), 0),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("loopback"));
}

#[test]
fn mounted_config_rejects_duplicate_dataset_names() {
    let error = ChronicleServerConfig::mounted(vec![
        DatasetMount::new("live", "/tmp/one").unwrap(),
        DatasetMount::new("live", "/tmp/two").unwrap(),
    ])
    .unwrap_err();
    assert!(error.to_string().contains("unique"));
}

#[test]
fn projected_turn_sequence_wins_over_call_wide_event_group() {
    let turn = StorylineTurn {
        id: 1,
        kind: Some("llm.response".into()),
        timestamp: None,
        source: "agent".into(),
        message: json!("done"),
        reasoning_content: None,
        reasoning_effort: None,
        tool_calls: None,
        observation: None,
        metrics: None,
        model_name: None,
        llm_call_count: Some(1),
        is_copied_context: None,
        latency_ms: None,
        ttft_ms: None,
        extra: Some(json!({"call_id": "model-call", "seq": 11})),
        env: None,
        prompt: None,
        finished_at: None,
    };
    let by_call = BTreeMap::from([("model-call".into(), vec![10, 11])]);

    assert_eq!(event_seqs_for_turn(&turn, &by_call), vec![11]);
}

#[test]
fn explorer_analysis_counts_usage_and_normalized_tools_once_per_call() {
    use persisting_pchronicle::model::StorylineToolCall;

    let user = StorylineTurn {
        id: 1,
        kind: Some("llm.request".into()),
        timestamp: Some(
            persisting_pchronicle::model::StorylineTimestamp::from_rfc3339("2026-08-20T00:00:00Z")
                .unwrap(),
        ),
        source: "user".into(),
        message: json!("run tool"),
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
        extra: Some(json!({"call_id": "model-call", "seq": 0})),
        env: None,
        prompt: None,
        finished_at: None,
    };
    let agent = StorylineTurn {
        id: 2,
        kind: Some("llm.response".into()),
        timestamp: Some(
            persisting_pchronicle::model::StorylineTimestamp::from_rfc3339("2026-08-20T00:00:01Z")
                .unwrap(),
        ),
        source: "agent".into(),
        message: json!(""),
        reasoning_content: None,
        reasoning_effort: None,
        tool_calls: Some(vec![StorylineToolCall {
            tool_call_id: "tool-call-1".into(),
            function_name: "lookup".into(),
            arguments: json!({"q": "x"}),
            result: None,
            duration_ms: None,
            extra: None,
            kind: None,
            response: None,
        }]),
        observation: None,
        metrics: Some(json!({
            "prompt_tokens": 10,
            "completion_tokens": 4,
            "total_tokens": 14
        })),
        model_name: Some("test-model".into()),
        llm_call_count: Some(1),
        is_copied_context: None,
        latency_ms: Some(1000),
        ttft_ms: Some(100),
        extra: Some(json!({"call_id": "model-call", "seq": 1})),
        env: None,
        prompt: None,
        finished_at: None,
    };
    let event = |seq, kind: &str, payload| EventRecord {
        identity: Default::default(),
        seq,
        source: "gateway".into(),
        kind: kind.into(),
        timestamp: None,
        session_id: Some("session".into()),
        agent_id: Some("agent".into()),
        parent_uuid: None,
        trace_id: None,
        call_id: Some("model-call".into()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload,
    };
    let events = vec![
        event(0, "llm.request", json!({"model": "test-model"})),
        event(
            1,
            "llm.response.stream",
            json!({
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 4,
                    "total_tokens": 14
                }
            }),
        ),
    ];
    let turns = vec![
        TrajectoryTurnView {
            turn: user,
            call_id: Some("model-call".into()),
            event_seqs: vec![0],
            // A later request carries the prior assistant tool call as message
            // history. It must not be counted as a new invocation.
            wire_tool_calls: vec![WireToolCall {
                id: Some("tool-call-1".into()),
                name: "lookup".into(),
                arguments: json!({"q": "x"}),
            }],
        },
        TrajectoryTurnView {
            turn: agent,
            call_id: Some("model-call".into()),
            event_seqs: vec![1],
            wire_tool_calls: vec![WireToolCall {
                id: Some("tool-call-1".into()),
                name: "lookup".into(),
                arguments: json!({"q": "x"}),
            }],
        },
    ];
    let run = RunSummary {
        dataset: "dataset".into(),
        file: "events.lance".into(),
        document_id: "session".into(),
        run_id: None,
        agent_id: "agent".into(),
        model_name: Some("test-model".into()),
        session_id: "session".into(),
        root_session_id: None,
        path: "dataset/events.lance/session".into(),
        row_count: 2,
        duplicate_event_ids: 0,
        status: "completed".into(),
    };

    let analysis = explorer::analyze(run, &turns, &events, CatalogEventProvenance::Canonical);
    assert_eq!(analysis.prompt_tokens, Some(10));
    assert_eq!(analysis.completion_tokens, Some(4));
    assert_eq!(analysis.total_tokens, Some(14));
    assert_eq!(analysis.tool_call_count, 1);
    assert_eq!(analysis.tools.len(), 1);
    assert_eq!(analysis.tools[0].name, "lookup");
    assert_eq!(analysis.tools[0].count, 1);
    assert_eq!(analysis.latency_ms.sample_count, 1);
    assert_eq!(analysis.ttft_ms.sample_count, 1);
}

#[test]
fn evidence_queries_are_wrapped_with_a_server_side_row_bound() {
    assert_eq!(
        bounded_evidence_sql("SELECT * FROM dataset.runs;", 200),
        "SELECT * FROM (SELECT * FROM dataset.runs) AS __pchronicle_evidence LIMIT 201"
    );
    assert_eq!(
            bounded_evidence_sql("WITH rows AS (SELECT 1) SELECT * FROM rows", 1),
            "SELECT * FROM (WITH rows AS (SELECT 1) SELECT * FROM rows) AS __pchronicle_evidence LIMIT 2"
        );
    assert_eq!(
        bounded_evidence_sql("EXPLAIN SELECT * FROM dataset.runs", 10),
        "EXPLAIN SELECT * FROM dataset.runs"
    );
}

#[test]
fn canonical_event_uri_resolves_write_coordinates_independent_of_mount_root() {
    let run = RunSummary {
        dataset: "live".into(),
        file: "agent/run-1/events.lance".into(),
        document_id: "child".into(),
        run_id: Some("child".into()),
        agent_id: "agent".into(),
        model_name: None,
        session_id: "child".into(),
        root_session_id: Some("run-1".into()),
        path: "live/agent/run-1/events.lance/child".into(),
        row_count: 1,
        duplicate_event_ids: 0,
        status: "active".into(),
    };
    let local = event_uri_coords("/tmp/capture/agent/run-1/events.lance", &run).unwrap();
    assert_eq!(local.storage, "/tmp/capture");
    assert_eq!(local.agent_id, "agent");
    assert_eq!(local.root_session_id.as_deref(), Some("run-1"));

    let remote = event_uri_coords("s3://bucket/prefix/agent/run-1/events.lance", &run).unwrap();
    assert_eq!(remote.storage, "s3://bucket/prefix");
    assert_eq!(remote.agent_id, "agent");
    assert_eq!(remote.session_id, "child");
}

#[tokio::test]
async fn json_datasets_expose_tables_and_support_read_only_sql() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let root = json_dataset_root();
    let app = router(root.to_string_lossy().to_string());
    let tables = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/query/tables")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(tables.status(), StatusCode::OK);
    let tables: Value =
        serde_json::from_slice(&tables.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let database = tables["database"].as_str().unwrap();
    assert_eq!(database, "dataset");
    assert_eq!(tables["tables"][0]["name"], "sources");
    assert_eq!(tables["tables"][1]["name"], "runs");
    assert_eq!(tables["tables"][2]["name"], "steps");
    assert_eq!(tables["tables"][3]["name"], "tool_calls");
    assert_eq!(tables["tables"][4]["name"], "trajectories");
    assert_eq!(tables["tables"][5]["name"], "events");

    let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/query/evidence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(
                        json!({"sql":format!(
                            "SELECT session_id, step_count FROM {database}.trajectories WHERE session_id = 'json-session'"
                        )})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    let response_status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        response_status,
        StatusCode::OK,
        "query failed: {}",
        String::from_utf8_lossy(&body)
    );
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["rows"][0]["session_id"], "json-session");
    assert_eq!(result["rows"][0]["step_count"], 2);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn explorer_automatically_refreshes_new_dataset_sources() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let root = json_dataset_root();
    let config = ChronicleServerConfig::mounted(vec![DatasetMount::default(
        root.to_string_lossy().to_string(),
    )
    .unwrap()])
    .unwrap();
    let app = test_router_with_catalog_refresh_interval(config, std::time::Duration::ZERO);

    let initial = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/explorer/runs?limit=10")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let initial: Value =
        serde_json::from_slice(&initial.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(initial["snapshot"]["total"], 1);

    write_gateway_fixture(&root, "second.json", "second-session", "second-job");
    let refreshed = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/explorer/runs?limit=10")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let refreshed: Value =
        serde_json::from_slice(&refreshed.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(refreshed["snapshot"]["total"], 2);
    assert!(refreshed["records"]
        .as_array()
        .unwrap()
        .iter()
        .any(|run| run["session_id"] == "second-session"));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn server_routing_index_prunes_point_queries_and_resets_on_refresh() -> anyhow::Result<()> {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let root = json_dataset_root();
    write_gateway_fixture(&root, "second.json", "second-session", "second-job");
    let app = router(root.to_string_lossy().to_string());

    let routed = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/query/evidence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(
                        json!({"sql":"SELECT _file_, session_id FROM runs WHERE session_id = 'json-session'"})
                            .to_string(),
                    ))?,
            )
            .await?;
    assert_eq!(routed.status(), StatusCode::OK);
    let body = routed.into_body().collect().await?.to_bytes();
    let result: Value = serde_json::from_slice(&body)?;
    assert_eq!(result["source_routing"], "applied");
    assert_eq!(result["rows"][0]["_file_"], "gateway.json");
    assert_eq!(result["rows"][0]["session_id"], "json-session");

    let quoted_alias = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/query/evidence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(
                        json!({"sql":"SELECT \"R\".session_id FROM runs AS \"R\" WHERE \"R\".session_id = 'json-session'"})
                            .to_string(),
                    ))?,
            )
            .await?;
    assert_eq!(quoted_alias.status(), StatusCode::OK);
    let quoted_alias: Value =
        serde_json::from_slice(&quoted_alias.into_body().collect().await?.to_bytes())?;
    assert_eq!(quoted_alias["source_routing"], "applied");

    let catalog = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/catalog")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    let catalog: Value = serde_json::from_slice(&catalog.into_body().collect().await?.to_bytes())?;
    assert_eq!(catalog["acceleration"]["run_index"]["rows"], 2);
    assert_eq!(catalog["acceleration"]["run_index"]["sources"], 2);
    assert_eq!(catalog["acceleration"]["run_summaries_ready"], false);
    assert_eq!(catalog["acceleration"]["event_identity_index"], Value::Null);
    assert_eq!(
        catalog["acceleration"]["event_partition_index"],
        Value::Null
    );

    let already_pruned = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/query/evidence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(
                        json!({"sql":"SELECT session_id FROM runs WHERE _file_ = 'gateway.json' AND session_id = 'json-session'"})
                            .to_string(),
                    ))?,
            )
            .await?;
    let already_pruned: Value =
        serde_json::from_slice(&already_pruned.into_body().collect().await?.to_bytes())?;
    assert_eq!(already_pruned["source_routing"], "already_pruned");

    let refreshed = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/catalog")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    assert_eq!(refreshed.status(), StatusCode::OK);
    let refreshed: Value =
        serde_json::from_slice(&refreshed.into_body().collect().await?.to_bytes())?;
    assert_eq!(refreshed["acceleration"]["run_index"], Value::Null);
    assert_eq!(
        refreshed["acceleration"]["event_identity_index"],
        Value::Null
    );
    assert_eq!(
        refreshed["acceleration"]["event_partition_index"],
        Value::Null
    );

    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn catalog_refresh_is_atomic_and_dataset_filtering_is_explicit() -> anyhow::Result<()> {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let live = json_dataset_root();
    let archive = json_dataset_root();
    let config = ChronicleServerConfig::mounted(vec![
        DatasetMount::new("live", live.to_string_lossy())?,
        DatasetMount::new("archive", archive.to_string_lossy())?,
    ])?;
    let app = test_router_with_config(config);

    let initial = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/catalog")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    assert_eq!(initial.status(), StatusCode::OK);
    let initial: Value = serde_json::from_slice(&initial.into_body().collect().await?.to_bytes())?;
    assert_eq!(initial["default_dataset"], Value::Null);
    assert_eq!(initial["datasets"].as_array().unwrap().len(), 2);
    let initial_snapshot = initial["snapshot_id"].as_str().unwrap().to_string();

    let filtered = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/explorer/runs?dataset=archive&limit=10")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    let filtered: Value =
        serde_json::from_slice(&filtered.into_body().collect().await?.to_bytes())?;
    assert_eq!(filtered["snapshot"]["total"], 1);
    assert_eq!(filtered["records"][0]["dataset"], "archive");

    // A malformed peripheral JSON file is intentionally validated only
    // after `_file_` pruning. Use an invalid Storyline commit descriptor
    // here so refresh fails while freezing the candidate snapshot.
    std::fs::create_dir(live.join("broken-store"))?;
    std::fs::write(live.join("broken-store/CURRENT"), "{")?;
    let failed_refresh = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/catalog")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    assert_eq!(failed_refresh.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let failed_refresh = response_json(failed_refresh).await;
    assert_eq!(failed_refresh["code"], "internal");
    assert_eq!(failed_refresh["message"], "internal server error");
    let failed_refresh = failed_refresh.to_string();
    assert!(!failed_refresh.contains(live.to_string_lossy().as_ref()));
    assert!(!failed_refresh.contains("broken-store"));

    let preserved = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/catalog")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    let preserved: Value =
        serde_json::from_slice(&preserved.into_body().collect().await?.to_bytes())?;
    assert_eq!(preserved["snapshot_id"], initial_snapshot);

    std::fs::remove_dir_all(live.join("broken-store"))?;
    std::fs::copy(live.join("gateway.json"), live.join("second.json"))?;
    let refreshed = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/catalog")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    assert_eq!(refreshed.status(), StatusCode::OK);
    let refreshed: Value =
        serde_json::from_slice(&refreshed.into_body().collect().await?.to_bytes())?;
    assert_ne!(refreshed["snapshot_id"], initial_snapshot);

    std::fs::remove_dir_all(live)?;
    std::fs::remove_dir_all(archive)?;
    Ok(())
}

#[tokio::test]
async fn prepared_catalog_installs_refreshes_and_retains_the_last_good_runtime(
) -> anyhow::Result<()> {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let root = json_dataset_root();
    let config =
        ChronicleServerConfig::mounted(vec![DatasetMount::default(root.to_string_lossy())?])?;
    let prepared = PreparedWarehouse::prepare(config).await?;
    let first = prepared
        .current_snapshot_id()
        .await
        .expect("prepare installs a Catalog before requests");

    std::fs::copy(root.join("gateway.json"), root.join("second.json"))?;
    let second = prepared.refresh_catalog().await?;
    assert_ne!(second, first);
    assert_eq!(
        prepared.current_snapshot_id().await.as_deref(),
        Some(second.as_str())
    );

    std::fs::create_dir(root.join("broken"))?;
    std::fs::write(root.join("broken/CURRENT"), "{")?;
    assert!(prepared.refresh_catalog().await.is_err());
    assert_eq!(
        prepared.current_snapshot_id().await.as_deref(),
        Some(second.as_str())
    );

    let response = prepared
        .router()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/catalog")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let catalog: Value = serde_json::from_slice(&response.into_body().collect().await?.to_bytes())?;
    assert_eq!(catalog["snapshot_id"], second);

    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn live_warehouse_reads_new_events_without_catalog_refresh() -> anyhow::Result<()> {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let temp = tempfile::tempdir()?;
    let coords = StoryCoords::new(temp.path().to_string_lossy(), "agent", "session", None);
    let event = |seq| EventRecord {
        identity: persisting_pchronicle::model::EventIdentity::default(),
        seq,
        source: "test".into(),
        kind: "note".into(),
        timestamp: None,
        session_id: Some("session".into()),
        agent_id: Some("agent".into()),
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: json!({"seq": seq}),
    };
    persisting_pchronicle::storage::RawEventLanceStore
        .append_events(&coords, &[event(0)])
        .await?;
    let config = ChronicleServerConfig::mounted(vec![DatasetMount::default(
        temp.path().to_string_lossy().to_string(),
    )?])?;
    let prepared = PreparedWarehouse::prepare_live(config).await?;

    // Append after the Warehouse has pinned its initial Catalog snapshot.
    persisting_pchronicle::storage::RawEventLanceStore
        .append_events(&coords, &[event(1)])
        .await?;
    let response = prepared
        .router()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/events?agent_id=agent&session_id=session")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&response.into_body().collect().await?.to_bytes())?;
    assert_eq!(body["snapshot"]["total"], 2);
    assert_eq!(body["records"].as_array().map(Vec::len), Some(2));
    Ok(())
}

#[tokio::test]
async fn warehouse_keeps_api_v1_aliases_for_embedded_web_ui() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let root = json_dataset_root();
    let app = router(root.to_string_lossy().to_string());
    for uri in [
        "/api/explorer/runs?limit=10",
        "/api/query/tables",
        "/api/physical/sources",
        "/api/v1/explorer/runs?limit=10",
        "/api/v1/query/tables",
        "/api/v1/physical/sources",
    ] {
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{uri} failed: {}",
            String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
        );
    }
}

#[tokio::test]
async fn warehouse_does_not_expose_unused_har_or_revisions_routes() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let root = json_dataset_root();
    let app = router(root.to_string_lossy().to_string());
    for uri in [
        "/api/export/har",
        "/api/revisions",
        "/api/v1/export/har",
        "/api/v1/revisions",
    ] {
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{uri} should be gone: {}",
            String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
        );
    }
}

#[tokio::test]
async fn explorer_routes_page_runs_and_lazy_load_turn_evidence() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let root = json_dataset_root();
    let app = router(root.to_string_lossy().to_string());
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/explorer/runs?status=active&limit=1")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let page: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(page["snapshot"]["total"], 1);
    assert_eq!(page["records"][0]["model"], "model-json");
    assert_eq!(
        page["records"][0]["path"],
        "dataset/gateway.json/json-job/json-session"
    );
    assert_eq!(page["path_index"].as_array().unwrap().len(), 1);

    let path_filtered = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/explorer/runs?path=dataset%2Fgateway.json%2Fjson-job&limit=10")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let path_filtered: Value = serde_json::from_slice(
        &path_filtered
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    assert_eq!(path_filtered["snapshot"]["total"], 1);
    assert_eq!(path_filtered["path_index"].as_array().unwrap().len(), 1);

    // The browser emits empty Catalog coordinates when opening a legacy
    // deep link that only carried the old agent/session identity.
    let coordinates = "dataset=&file=&run_id=&agent_id=model-json&session_id=json-session&root_session_id=json-job";
    let analysis = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/explorer/run?{coordinates}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let analysis_status = analysis.status();
    let analysis_body = analysis.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        analysis_status,
        StatusCode::OK,
        "analysis failed: {}",
        String::from_utf8_lossy(&analysis_body)
    );
    let analysis: Value = serde_json::from_slice(&analysis_body).unwrap();
    assert_eq!(analysis["event_provenance"], "synthetic_from_storyline");
    assert_eq!(analysis["turn_count"], 2);
    assert_eq!(analysis["latency_histogram"].as_array().unwrap().len(), 6);
    assert_eq!(analysis["source_breakdown"].as_array().unwrap().len(), 2);
    assert!(analysis["kind_breakdown"].is_array());
    assert!(analysis["model_breakdown"].is_array());

    let turns = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/explorer/turns?{coordinates}&limit=10"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let turns: Value =
        serde_json::from_slice(&turns.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(turns["records"].as_array().unwrap().len(), 2);
    assert!(turns["records"][0].get("message").is_none());
    assert!(turns["records"][0].get("total_tokens").is_some());

    let detail = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/explorer/turn?{coordinates}&turn_id=1"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let detail_status = detail.status();
    let detail_body = detail.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        detail_status,
        StatusCode::OK,
        "turn detail failed: {}",
        String::from_utf8_lossy(&detail_body)
    );
    let detail: Value = serde_json::from_slice(&detail_body).unwrap();
    assert_eq!(detail["summary"]["id"], 1);
    assert_eq!(detail["turn"]["src"], "user");
    assert_eq!(detail["event_provenance"], "synthetic_from_storyline");

    let events = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/events?{coordinates}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    let events = response_json(events).await;
    assert_eq!(
        events["provenance"],
        json!({
            "kind": "synthetic_from_storyline",
            "transform": "storyline_to_events_v1"
        })
    );
    assert!(!events["records"].as_array().unwrap().is_empty());

    let otlp = app
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/export/otlp?{coordinates}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(otlp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let otlp = response_json(otlp).await;
    assert_eq!(otlp["code"], "unsupported");
    assert!(otlp["message"]
        .as_str()
        .unwrap()
        .contains("reconstructed events"));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn explorer_lists_nested_actf_event_log_json_files() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let root = json_dataset_root();
    let nested = root.join("owner/details");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("_error_lean4-proof_formal method.json"),
        serde_json::to_vec(&json!({
            "task_id": "lean4-proof",
            "category": "formal method",
            "k": 1,
            "correct": false,
            "solved_at": null,
            "attempts_tried": 1,
            "attempts": {
                "1": {
                    "correct": false,
                    "status": "run_error",
                    "trajectory": [
                        {"type":"session","id":"s1","timestamp":"2026-06-17T07:26:27.170Z","cwd":"/root"},
                        {"type":"message","id":"m1","timestamp":"2026-06-17T07:26:28Z",
                         "message":{"role":"user","content":[{"type":"text","text":"hello"}]}}
                    ]
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let response = router(root.to_string_lossy().to_string())
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/explorer/runs?limit=20")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "explorer failed: {}",
        String::from_utf8_lossy(&body)
    );
    let page: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        page["snapshot"]["total"].as_u64().unwrap() >= 2,
        "expected gateway.json plus nested ACTF, got {page}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn explorer_uses_terminal_metadata_for_run_status() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let root = json_dataset_root();
    write_gateway_fixture_with_status(
        &root,
        "completed.json",
        "completed-session",
        "completed-job",
        true,
    );
    let response = router(root.to_string_lossy().to_string())
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/explorer/runs?status=completed&limit=10")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let page: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(page["snapshot"]["total"], 1);
    assert_eq!(page["records"][0]["session_id"], "completed-session");
    assert_eq!(page["records"][0]["status"], "completed");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn limited_query_enforces_copilot_boundaries() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let root = json_dataset_root();
    let app = router(root.to_string_lossy().to_string());
    let tables = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/query/tables")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let tables: Value =
        serde_json::from_slice(&tables.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let database = tables["database"].as_str().unwrap();
    let evidence = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/query/evidence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(
                        json!({"sql":format!("SELECT * FROM {database}.steps"),"max_rows":1,"max_bytes":1048576})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(evidence.status(), StatusCode::OK);
    let evidence: Value =
        serde_json::from_slice(&evidence.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(evidence["returned_rows"], 1);
    assert_eq!(evidence["max_rows"], 1);
    assert_eq!(evidence["max_bytes"], 1_048_576);

    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn boundary_missing_lookup_returns_not_found() {
    use tower::ServiceExt as _;

    let root = json_dataset_root();
    let response = router(root.to_string_lossy().to_string())
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/explorer/turn?dataset=dataset&file=gateway.json&run_id=json-job&agent_id=model-json&session_id=json-session&root_session_id=json-job&turn_id=999")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response_json(response).await;
    assert_eq!(body["code"], "not_found");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn boundary_malformed_query_parameters_return_json() {
    use tower::ServiceExt as _;

    let root = json_dataset_root();
    let response = router(root.to_string_lossy().to_string())
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/explorer/runs?limit=not-a-number")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["message"], "query parameters must be valid");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn boundary_unsupported_query_input_returns_unprocessable_entity() {
    use tower::ServiceExt as _;

    let root = json_dataset_root();
    let response = router(root.to_string_lossy().to_string())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/query/evidence")
                .header(header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    json!({"sql":"DELETE FROM runs"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = response_json(response).await;
    assert_eq!(body["code"], "unsupported");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn query_evidence_sql_failure_returns_visible_invalid_request() {
    use tower::ServiceExt as _;

    let root = json_dataset_root();
    let response = router(root.to_string_lossy().to_string())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/query/evidence")
                .header(header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    json!({"sql":"SELECT no_such_column_xyz FROM runs"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["code"], "invalid_request");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains("no_such_column_xyz"),
        "error message should expose the failing column: {message}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn query_evidence_byte_budget_uses_writer_exhaustion_outcome() {
    use tower::ServiceExt as _;

    let root = json_dataset_root();
    let response = router(root.to_string_lossy().to_string())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/query/evidence")
                .header(header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    json!({"sql":"SELECT repeat('x', 2048) AS payload","max_bytes":1024})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = response_json(response).await;
    assert_eq!(body["code"], "resource_exhausted");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn explorer_tree_lists_mounted_datasets_by_run_count() -> anyhow::Result<()> {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let live = json_dataset_root();
    std::fs::create_dir_all(live.join("nested"))?;
    write_gateway_fixture(&live, "nested/run.json", "nested-session", "nested-job");
    let archive = json_dataset_root();
    let config = ChronicleServerConfig::mounted(vec![
        DatasetMount::new("live", live.to_string_lossy())?,
        DatasetMount::new("archive", archive.to_string_lossy())?,
    ])?;
    let app = test_router_with_config(config);

    let warehouse = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/explorer/tree")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    assert_eq!(warehouse.status(), StatusCode::OK);
    let warehouse: Value =
        serde_json::from_slice(&warehouse.into_body().collect().await?.to_bytes())?;
    assert_eq!(warehouse["run_count"], 3);
    assert_eq!(warehouse["children"][0]["name"], "live");
    assert_eq!(warehouse["children"][0]["kind"], "dataset");
    assert_eq!(warehouse["children"][0]["run_count"], 2);
    assert_eq!(warehouse["children"][1]["name"], "archive");
    assert_eq!(warehouse["children"][1]["run_count"], 1);

    let dataset = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/explorer/tree?dataset=live")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    assert_eq!(dataset.status(), StatusCode::OK);
    let dataset: Value = serde_json::from_slice(&dataset.into_body().collect().await?.to_bytes())?;
    assert_eq!(dataset["dataset"], "live");
    assert_eq!(dataset["run_count"], 2);
    assert!(dataset["ready_sources"].as_u64().unwrap() >= 1);
    let names: Vec<_> = dataset["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|child| child["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"gateway.json".into()));
    assert!(names.contains(&"nested".into()));

    let prefixed = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/explorer/runs?dataset=live&file=nested&limit=10")
                .body(axum::body::Body::empty())?,
        )
        .await?;
    let prefixed: Value =
        serde_json::from_slice(&prefixed.into_body().collect().await?.to_bytes())?;
    assert_eq!(prefixed["snapshot"]["total"], 1);
    assert_eq!(prefixed["records"][0]["file"], "nested/run.json");

    std::fs::remove_dir_all(live)?;
    std::fs::remove_dir_all(archive)?;
    Ok(())
}

#[test]
fn sql_validation_rejects_empty_and_mutating_statements() {
    assert!(validate_read_only_sql("SELECT 1").is_ok());
    assert!(validate_read_only_sql("SELECT 1;").is_ok());
    assert!(validate_read_only_sql("WITH x AS (SELECT 1) SELECT * FROM x").is_ok());
    assert!(validate_read_only_sql("EXPLAIN SELECT 1").is_ok());
    assert!(validate_read_only_sql("").is_err());
    assert!(validate_read_only_sql("DELETE FROM runs").is_err());
    assert!(validate_read_only_sql("EXPLAIN DELETE FROM runs").is_err());
    assert!(validate_read_only_sql("SELECT 1; DELETE FROM runs").is_err());
}

#[tokio::test]
async fn analysis_compile_validates_snapshot_and_rejects_uncomputable_specs() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let root = json_dataset_root();
    let app = router(root.to_string_lossy().to_string());
    let tables = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/query/tables")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let tables: Value =
        serde_json::from_slice(&tables.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let snapshot_id = tables["snapshot_id"].as_str().unwrap();

    let stale = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/analysis/compile")
                .header(header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    json!({
                        "spec": {
                            "intent": "distribution",
                            "grain": "step",
                            "measure": "step_latency_ms",
                            "output": "distribution"
                        },
                        "snapshot_id": "stale",
                        "scope": { "database": "dataset", "items": [] }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let stale: Value =
        serde_json::from_slice(&stale.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(stale["code"], "stale_snapshot");

    let rejected = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/analysis/compile")
                .header(header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    json!({
                        "spec": {
                            "intent": "compare",
                            "grain": "run",
                            "measure": "status",
                            "output": "comparison"
                        },
                        "snapshot_id": snapshot_id,
                        "scope": { "database": "dataset", "items": [] }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let rejected: Value =
        serde_json::from_slice(&rejected.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(rejected["code"], "uncomputable");
}

fn encode_query(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn storyline_document(
    session_id: &str,
    run_id: &str,
) -> persisting_pchronicle::model::StorylineDocument {
    use persisting_pchronicle::model::{
        StorylineAgent, StorylineDocument, StorylineTurn, STORYLINE_SCHEMA_VERSION,
    };
    StorylineDocument {
        schema_version: STORYLINE_SCHEMA_VERSION.into(),
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
        meta: None,
        task: None,
        prompt: None,
        started_at: None,
        finished_at: None,
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
            env: None,
            prompt: None,
            finished_at: None,
        }],
    }
}

async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
        .unwrap_or_else(|_| json!({}));
    (status, body)
}

#[tokio::test]
async fn physical_api_lists_empty_sources_for_json_catalog_and_rejects_non_lance() {
    let root = json_dataset_root();
    let app = router(root.to_string_lossy().to_string());
    let (status, body) = get_json(&app, "/api/physical/sources").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!([]));

    let (status, body) = get_json(
        &app,
        &format!(
            "/api/physical/layout?dataset={}&file=gateway.json",
            encode_query(DEFAULT_DATASET_NAME)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");

    let (status, body) = get_json(
        &app,
        &format!(
            "/api/physical/layout?dataset={}&file=missing.lance",
            encode_query(DEFAULT_DATASET_NAME)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn physical_api_inspects_storyline_lance_layout_file_and_page() {
    use persisting_pchronicle::storage::StorylineLanceStore;

    let root = json_dataset_root();
    let store = StorylineLanceStore::open(root.join("story"))
        .await
        .expect("open storyline store");
    store
        .replace_storyline(&storyline_document("session-a", "run-a"))
        .await
        .expect("write storyline");
    let app = router(root.to_string_lossy().to_string());
    let dataset = encode_query(DEFAULT_DATASET_NAME);

    let (status, sources) = get_json(&app, "/api/physical/sources").await;
    assert_eq!(status, StatusCode::OK, "{sources}");
    assert_eq!(sources.as_array().map(Vec::len), Some(1));
    assert_eq!(sources[0]["file"], "story");
    assert_eq!(sources[0]["format"], "storyline-lance");

    let (status, layout) = get_json(
        &app,
        &format!("/api/physical/layout?dataset={dataset}&file=story"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{layout}");
    let runs = layout["tables"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|table| table["name"] == "runs")
        .expect("runs table");
    let fragment = &runs["fragments"][0];
    assert_eq!(runs["fragments"].as_array().map(Vec::len), Some(1));
    assert!(fragment["physical_rows"].as_u64().unwrap_or(0) >= 1);
    assert!(fragment["size_bytes"].as_u64().unwrap_or(0) > 0);
    let fragment_id = fragment["id"].as_u64().expect("fragment id");
    let data_file = fragment["files"][0]["path"].as_str().expect("data file");

    let (status, file) = get_json(
        &app,
        &format!(
            "/api/physical/file?dataset={dataset}&file=story&table=runs&fragment={fragment_id}&data_file={}",
            encode_query(data_file)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{file}");
    assert!(!file["columns"].as_array().unwrap().is_empty());

    let (status, preview) = get_json(
        &app,
        &format!(
            "/api/physical/page?dataset={dataset}&file=story&table=runs&fragment={fragment_id}&data_file={}&column=session_id&limit=8",
            encode_query(data_file)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["columns"], json!(["session_id"]));
    assert!(
        preview["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|row| row
                .as_array()
                .is_some_and(|cells| cells.iter().any(|cell| cell == "session-a"))),
        "{preview}"
    );
}

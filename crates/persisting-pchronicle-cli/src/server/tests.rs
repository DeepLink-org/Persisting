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
fn tool_call_counter_handles_openai_and_anthropic_payloads() {
    let payload = json!({
        "choices": [{"message": {"tool_calls": [{"id":"call-1"}, {"id":"call-2"}]}}],
        "content": [{"type":"tool_use", "id":"toolu-1"}],
    });
    assert_eq!(count_tool_calls(&payload), 3);
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

//! Local, loopback-only pChronicle browser.

mod acceleration;
mod asset;
mod explorer;
pub(crate) mod problem;

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use persisting_pchronicle::document::{events_to_har, events_to_otlp_json, InputIssue};
use persisting_pchronicle::model::{EventRecord, StorylineTurn};
use persisting_pchronicle::query::ChronicleQueryEngine;
use persisting_pchronicle::storage::{
    read_revisions, CatalogErrorPolicy, CatalogSnapshotOptions, CatalogStorylineKey,
    DatasetCatalogSnapshot, DatasetMount, StoryCoords, DEFAULT_DATASET_NAME,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use acceleration::{AccelerationStatus, ServerAcceleration};
use problem::ApiError;

#[cfg(test)]
use problem::BoundaryCode;

#[derive(Clone)]
struct AppState {
    config: Arc<ChronicleServerConfig>,
    catalog: Arc<tokio::sync::RwLock<Option<Arc<CatalogRuntime>>>>,
    trajectory_cache: Arc<tokio::sync::RwLock<Option<(String, LoadedTrajectory)>>>,
}

#[derive(Debug, Clone)]
pub struct ChronicleServerConfig {
    pub datasets: Vec<DatasetMount>,
    pub default_dataset: Option<String>,
    pub catalog_options: CatalogSnapshotOptions,
}

impl ChronicleServerConfig {
    pub fn mounted(datasets: Vec<DatasetMount>) -> anyhow::Result<Self> {
        anyhow::ensure!(!datasets.is_empty(), "mount at least one Dataset");
        let unique = datasets
            .iter()
            .map(|dataset| dataset.name.as_str())
            .collect::<std::collections::HashSet<_>>();
        anyhow::ensure!(
            unique.len() == datasets.len(),
            "Dataset names must be unique"
        );
        let default_dataset = (datasets.len() == 1).then(|| datasets[0].name.clone());
        Ok(Self {
            datasets,
            default_dataset,
            catalog_options: CatalogSnapshotOptions::default(),
        })
    }
}

#[derive(Debug)]
struct CatalogRuntime {
    snapshot: Arc<DatasetCatalogSnapshot>,
    engine: Arc<ChronicleQueryEngine>,
    acceleration: ServerAcceleration,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunSummary {
    pub(crate) dataset: String,
    pub(crate) file: String,
    pub(crate) document_id: String,
    pub(crate) run_id: Option<String>,
    pub(crate) agent_id: String,
    pub(crate) model_name: Option<String>,
    pub(crate) session_id: String,
    pub(crate) root_session_id: Option<String>,
    pub(crate) path: String,
    pub(crate) row_count: usize,
    pub(crate) duplicate_event_ids: usize,
    pub(crate) status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SessionQuery {
    pub(crate) dataset: Option<String>,
    pub(crate) file: Option<String>,
    pub(crate) run_id: Option<String>,
    pub(crate) agent_id: String,
    pub(crate) session_id: String,
    pub(crate) root_session_id: Option<String>,
    pub(crate) offset: Option<usize>,
    pub(crate) limit: Option<usize>,
}

/// Build the read-only Warehouse API and Web UI.
pub fn warehouse_router(config: ChronicleServerConfig) -> Router {
    let state = app_state(config);
    read_routes().with_state(state)
}

fn app_state(config: ChronicleServerConfig) -> AppState {
    AppState {
        config: Arc::new(config),
        catalog: Arc::new(tokio::sync::RwLock::new(None)),
        trajectory_cache: Arc::new(tokio::sync::RwLock::new(None)),
    }
}

fn read_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/api/health", get(warehouse_health))
        .route("/api/runs", get(runs))
        .route("/api/explorer/runs", get(explorer_runs))
        .route("/api/explorer/run", get(explorer_run))
        .route("/api/explorer/turns", get(explorer_turns))
        .route("/api/explorer/turn", get(explorer_turn))
        .route("/api/events", get(events))
        .route("/api/storyline", get(storyline))
        .route("/api/trajectory-view", get(trajectory_view))
        .route("/api/export/har", get(export_har))
        .route("/api/export/otlp", get(export_otlp))
        .route("/api/revisions", get(revisions))
        .route("/api/catalog", get(catalog).post(refresh_catalog))
        .route("/api/query/tables", get(query_tables))
        .route("/api/query/evidence", post(query_evidence))
        .fallback(asset_fallback)
}

/// Serve statically mounted Datasets through the read-only Warehouse surface.
pub async fn serve_warehouse(
    config: ChronicleServerConfig,
    addr: SocketAddr,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        addr.ip().is_loopback(),
        "pChronicle Warehouse may only bind to a loopback address"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_warehouse_with_listener(config, listener).await
}

/// Serve the Warehouse with an already-bound listener. This lets callers
/// report the actual address (including an ephemeral port) before serving.
pub async fn serve_warehouse_with_listener(
    config: ChronicleServerConfig,
    listener: tokio::net::TcpListener,
) -> anyhow::Result<()> {
    serve_warehouse_with_listener_and_shutdown(config, listener, std::future::pending()).await
}

/// Serve the Warehouse until the supplied shutdown signal completes.
pub async fn serve_warehouse_with_listener_and_shutdown(
    config: ChronicleServerConfig,
    listener: tokio::net::TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let addr = listener
        .local_addr()
        .context("read Warehouse listen address")?;
    anyhow::ensure!(
        addr.ip().is_loopback(),
        "pChronicle Warehouse may only bind to a loopback address"
    );
    axum::serve(listener, warehouse_router(config))
        .with_graceful_shutdown(shutdown)
        .await
        .context("serve pChronicle Warehouse")
}

async fn index(headers: axum::http::HeaderMap) -> Response {
    asset::index(headers).await
}

async fn asset_fallback(
    State(_state): State<AppState>,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
) -> Response {
    let path = uri.path().to_ascii_lowercase();
    if path.split('/').any(|segment| segment == "..")
        || path.contains("%2e")
        || path.contains('\\')
        || path.contains("%5c")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    asset::fallback(uri, headers).await
}

async fn warehouse_health() -> Json<Value> {
    Json(json!({"status":"ok","mode":"read_only"}))
}

async fn build_catalog_runtime(
    config: &ChronicleServerConfig,
) -> anyhow::Result<Arc<CatalogRuntime>> {
    let snapshot = Arc::new(
        DatasetCatalogSnapshot::discover(
            config.datasets.clone(),
            config.default_dataset.clone(),
            config.catalog_options,
        )
        .await?,
    );
    let engine = Arc::new(snapshot.clone().query_engine(Default::default()).await?);
    Ok(Arc::new(CatalogRuntime {
        snapshot,
        engine,
        acceleration: ServerAcceleration::default(),
    }))
}

async fn current_catalog(state: &AppState) -> Result<Arc<CatalogRuntime>, ApiError> {
    if let Some(runtime) = state.catalog.read().await.as_ref() {
        return Ok(runtime.clone());
    }
    let runtime = build_catalog_runtime(&state.config)
        .await
        .map_err(ApiError::internal)?;
    let mut catalog = state.catalog.write().await;
    Ok(catalog.get_or_insert_with(|| runtime.clone()).clone())
}

#[derive(Debug, Serialize)]
struct CatalogResponse {
    snapshot_id: String,
    created_at: String,
    default_dataset: Option<String>,
    error_policy: CatalogErrorPolicy,
    datasets: Vec<persisting_pchronicle::storage::CatalogDataset>,
    acceleration: AccelerationStatus,
}

fn catalog_response(state: &AppState, runtime: &CatalogRuntime) -> CatalogResponse {
    CatalogResponse {
        snapshot_id: runtime.snapshot.snapshot_id().to_string(),
        created_at: runtime.snapshot.created_at().to_string(),
        default_dataset: runtime.snapshot.default_dataset().map(str::to_owned),
        error_policy: state.config.catalog_options.error_policy,
        datasets: runtime.snapshot.datasets().to_vec(),
        acceleration: runtime.acceleration.status(),
    }
}

async fn catalog(State(state): State<AppState>) -> Result<Json<CatalogResponse>, ApiError> {
    let runtime = current_catalog(&state).await?;
    Ok(Json(catalog_response(&state, &runtime)))
}

async fn refresh_catalog(State(state): State<AppState>) -> Result<Json<CatalogResponse>, ApiError> {
    // Build fully outside the write lock. Failed strict refreshes leave the
    // previously published snapshot untouched.
    let runtime = build_catalog_runtime(&state.config)
        .await
        .map_err(ApiError::internal)?;
    *state.catalog.write().await = Some(runtime.clone());
    *state.trajectory_cache.write().await = None;
    Ok(Json(catalog_response(&state, &runtime)))
}

async fn runs(State(state): State<AppState>) -> Result<Json<Vec<RunSummary>>, ApiError> {
    Ok(Json(load_run_summaries(&state).await?))
}

async fn load_run_summaries(state: &AppState) -> Result<Vec<RunSummary>, ApiError> {
    let runtime = current_catalog(state).await?;
    runtime
        .acceleration
        .run_summaries(&runtime.snapshot, &runtime.engine)
        .await
        .map(|summaries| summaries.as_ref().clone())
        .map_err(ApiError::internal)
}

fn api_query<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, ApiError> {
    query
        .map(|Query(query)| query)
        .map_err(|_| ApiError::invalid_request("query parameters must be valid"))
}

async fn explorer_runs(
    State(state): State<AppState>,
    query: Result<Query<explorer::ExplorerRunsQuery>, QueryRejection>,
) -> Result<Json<explorer::RunExplorerPage>, ApiError> {
    let query = api_query(query)?;
    let summaries = load_run_summaries(&state).await?;
    Ok(Json(explorer::run_page(summaries, &query)))
}

async fn resolve_run_summary(
    state: &AppState,
    query: &SessionQuery,
) -> Result<RunSummary, ApiError> {
    let mut matches = load_run_summaries(state)
        .await?
        .into_iter()
        .filter(|run| {
            query
                .dataset
                .as_ref()
                .filter(|value| !value.is_empty())
                .is_none_or(|value| value == &run.dataset)
                && query
                    .file
                    .as_ref()
                    .filter(|value| !value.is_empty())
                    .is_none_or(|value| value == &run.file)
                && query
                    .run_id
                    .as_ref()
                    .filter(|value| !value.is_empty())
                    .is_none_or(|value| run.run_id.as_ref() == Some(value))
                && (run.agent_id == query.agent_id
                    || run.model_name.as_deref() == Some(query.agent_id.as_str()))
                && run.session_id == query.session_id
        })
        .collect::<Vec<_>>();
    // Legacy browser URLs did not carry the Catalog key. Treat the old
    // root_session_id coordinate as an ambiguity breaker, not as a required
    // identity field: direct JSON sources may not preserve that field in the
    // normalized run row.
    if matches.len() > 1 {
        if let Some(root) = &query.root_session_id {
            matches.retain(|run| run.root_session_id.as_ref() == Some(root));
        }
    }
    if matches.is_empty() {
        return Err(ApiError::not_found("trajectory was not found"));
    }
    if matches.len() > 1 {
        return Err(ApiError::conflict(
            "trajectory selector is ambiguous; include dataset, _file_, and session_id",
        ));
    }
    Ok(matches.into_iter().next().expect("one matching run"))
}

fn catalog_storyline_key(run: &RunSummary) -> CatalogStorylineKey {
    CatalogStorylineKey {
        dataset: run.dataset.clone(),
        file: run.file.clone(),
        document_id: run.document_id.clone(),
        session_id: run.session_id.clone(),
    }
}

async fn canonical_run_coords(
    state: &AppState,
    query: &SessionQuery,
) -> Result<Option<StoryCoords>, ApiError> {
    let run = resolve_run_summary(state, query).await?;
    canonical_run_coords_for_summary(state, &run).await
}

async fn canonical_run_coords_for_summary(
    state: &AppState,
    run: &RunSummary,
) -> Result<Option<StoryCoords>, ApiError> {
    let runtime = current_catalog(state).await?;
    let event_uri = runtime
        .snapshot
        .canonical_event_uri(&catalog_storyline_key(run))
        .map_err(ApiError::internal)?;
    event_uri
        .map(|event_uri| event_uri_coords(event_uri, run).map_err(ApiError::internal))
        .transpose()
}

fn event_uri_coords(uri: &str, run: &RunSummary) -> anyhow::Result<StoryCoords> {
    let uri = uri.trim_end_matches('/');
    let run_uri = uri
        .strip_suffix("/events.lance")
        .or_else(|| (uri == "events.lance").then_some(""))
        .with_context(|| format!("canonical event URI does not end in events.lance: {uri}"))?;
    let (agent_uri, physical_run_id) = run_uri
        .rsplit_once('/')
        .context("canonical event URI has no Run directory")?;
    let (storage, physical_agent_id) = agent_uri
        .rsplit_once('/')
        .map_or((".", agent_uri), |(storage, agent)| (storage, agent));
    anyhow::ensure!(
        !physical_agent_id.is_empty() && !physical_run_id.is_empty(),
        "canonical event URI has an invalid agent/Run hierarchy: {uri}"
    );
    Ok(StoryCoords::new(
        storage,
        physical_agent_id,
        run.session_id.clone(),
        Some(physical_run_id.to_string()),
    ))
}

async fn load_events(state: &AppState, query: &SessionQuery) -> Result<Vec<EventRecord>, ApiError> {
    let run = resolve_run_summary(state, query).await?;
    let runtime = current_catalog(state).await?;
    let document = runtime
        .snapshot
        .load_events(&catalog_storyline_key(&run))
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("trajectory was not found"))?;
    let offset = query.offset.unwrap_or(0).min(document.events.len());
    let end = query
        .limit
        .map(|limit| offset.saturating_add(limit).min(document.events.len()))
        .unwrap_or(document.events.len());
    Ok(document.events[offset..end].to_vec())
}

async fn events(
    State(state): State<AppState>,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let query = api_query(query)?;
    let offset = query.offset.unwrap_or(0);
    let requested_limit = query.limit.unwrap_or(1000);
    let full_query = SessionQuery {
        offset: None,
        limit: None,
        ..query.clone()
    };
    let all_records = load_events(&state, &full_query).await?;
    let total = all_records.len();
    let start = offset.min(total);
    let end = start.saturating_add(requested_limit).min(total);
    let records = all_records[start..end].to_vec();
    let next_offset = offset + records.len();
    Ok(Json(json!({
        "snapshot": {
            "offset": offset,
            "next_offset": next_offset,
            "total": total,
            "has_more": next_offset < total && !records.is_empty(),
            "limit": requested_limit
        },
        "records": records
    })))
}

async fn storyline(
    State(state): State<AppState>,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let query = api_query(query)?;
    let run = resolve_run_summary(&state, &query).await?;
    let runtime = current_catalog(&state).await?;
    let document = runtime
        .snapshot
        .load_storyline(&catalog_storyline_key(&run))
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("trajectory was not found"))?;
    Ok(Json(
        serde_json::to_value(document)
            .map_err(anyhow::Error::from)
            .map_err(ApiError::internal)?,
    ))
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TrajectoryTurnView {
    pub(crate) turn: StorylineTurn,
    pub(crate) call_id: Option<String>,
    pub(crate) event_seqs: Vec<u64>,
    pub(crate) wire_tool_calls: Vec<WireToolCall>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WireToolCall {
    pub(crate) id: Option<String>,
    pub(crate) name: String,
    pub(crate) arguments: Value,
}

#[derive(Debug, Serialize)]
struct TrajectoryView {
    run: RunSummary,
    event_kind_counts: BTreeMap<String, usize>,
    tool_call_count: usize,
    turns: Vec<TrajectoryTurnView>,
}

fn normalize_arguments(value: Value) -> Value {
    if let Value::String(text) = &value {
        serde_json::from_str(text).unwrap_or(value)
    } else {
        value
    }
}

fn parse_wire_tool_call(value: &Value) -> Option<WireToolCall> {
    let map = value.as_object()?;
    let function = map.get("function").and_then(Value::as_object);
    let name = function
        .and_then(|value| value.get("name"))
        .or_else(|| map.get("name"))
        .and_then(Value::as_str)?
        .to_string();
    let id = map
        .get("id")
        .or_else(|| map.get("call_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let arguments = function
        .and_then(|value| value.get("arguments"))
        .or_else(|| map.get("arguments"))
        .or_else(|| map.get("input"))
        .cloned()
        .map(normalize_arguments)
        .unwrap_or_else(|| json!({}));
    Some(WireToolCall {
        id,
        name,
        arguments,
    })
}

fn collect_wire_tool_calls(value: &Value, out: &mut Vec<WireToolCall>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_wire_tool_calls(item, out);
            }
        }
        Value::Object(map) => {
            if let Some(calls) = map.get("tool_calls").and_then(Value::as_array) {
                out.extend(calls.iter().filter_map(parse_wire_tool_call));
            }
            if matches!(
                map.get("type").and_then(Value::as_str),
                Some("tool_use" | "function_call" | "custom_tool_call" | "local_shell_call")
            ) {
                if let Some(call) = parse_wire_tool_call(value) {
                    out.push(call);
                }
            }
            for (key, child) in map {
                if key != "tool_calls" {
                    collect_wire_tool_calls(child, out);
                }
            }
        }
        _ => {}
    }
}

fn turn_call_id(turn: &StorylineTurn) -> Option<String> {
    turn.extra
        .as_ref()
        .and_then(|extra| extra.get("call_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn turn_seq(turn: &StorylineTurn) -> Option<u64> {
    turn.extra
        .as_ref()
        .and_then(|extra| extra.get("seq").or_else(|| extra.get("event_seq")))
        .and_then(Value::as_u64)
}

fn event_seqs_for_turn(turn: &StorylineTurn, by_call: &BTreeMap<String, Vec<u64>>) -> Vec<u64> {
    // Canonical Event -> Storyline projection records the authoritative source
    // sequence on each turn. Prefer it over the broader call correlation: a
    // call contains both request and response, and attaching both to both turns
    // duplicates usage, tool calls, latency and TTFT in Explorer aggregates.
    turn_seq(turn)
        .map(|seq| vec![seq])
        .or_else(|| turn_call_id(turn).and_then(|id| by_call.get(&id).cloned()))
        .unwrap_or_default()
}

#[derive(Clone)]
struct LoadedTrajectory {
    run: RunSummary,
    records: Vec<EventRecord>,
    turns: Vec<TrajectoryTurnView>,
}

async fn load_trajectory(
    state: &AppState,
    query: &SessionQuery,
) -> Result<LoadedTrajectory, ApiError> {
    let run = resolve_run_summary(state, query).await?;
    let runtime = current_catalog(state).await?;
    let cache_key = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}",
        runtime.snapshot.snapshot_id(),
        run.dataset,
        run.file,
        run.session_id
    );
    if let Some((_, loaded)) = state
        .trajectory_cache
        .read()
        .await
        .as_ref()
        .filter(|(key, _)| key == &cache_key)
    {
        return Ok(loaded.clone());
    }
    let key = catalog_storyline_key(&run);
    let bundle = runtime
        .snapshot
        .load_trajectory_bundle(&key)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("trajectory was not found"))?;
    let records = bundle.events.events;
    let document = bundle.storyline;
    let mut by_call = BTreeMap::<String, Vec<u64>>::new();
    for event in &records {
        if let Some(call_id) = event.call_id.as_ref().filter(|id| !id.is_empty()) {
            by_call.entry(call_id.clone()).or_default().push(event.seq);
        }
    }
    let turns = document
        .turns
        .into_iter()
        .map(|turn| {
            let call_id = turn_call_id(&turn);
            let event_seqs = event_seqs_for_turn(&turn, &by_call);
            let mut wire_tool_calls = Vec::new();
            for event in records
                .iter()
                .filter(|event| event_seqs.contains(&event.seq))
            {
                collect_wire_tool_calls(&event.payload, &mut wire_tool_calls);
            }
            let mut seen = BTreeSet::new();
            wire_tool_calls.retain(|call| {
                seen.insert((
                    call.id.clone(),
                    call.name.clone(),
                    serde_json::to_string(&call.arguments).unwrap_or_default(),
                ))
            });
            TrajectoryTurnView {
                turn,
                call_id,
                event_seqs,
                wire_tool_calls,
            }
        })
        .collect();
    let loaded = LoadedTrajectory {
        run,
        records,
        turns,
    };
    *state.trajectory_cache.write().await = Some((cache_key, loaded.clone()));
    Ok(loaded)
}

async fn trajectory_view(
    State(state): State<AppState>,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<TrajectoryView>, ApiError> {
    let query = api_query(query)?;
    let loaded = load_trajectory(&state, &query).await?;
    let mut event_kind_counts = BTreeMap::new();
    for event in &loaded.records {
        *event_kind_counts.entry(event.kind.clone()).or_insert(0) += 1;
    }
    let tool_call_count = loaded
        .turns
        .iter()
        .map(explorer::display_tool_calls)
        .map(|calls| calls.len())
        .sum();
    Ok(Json(TrajectoryView {
        run: loaded.run,
        event_kind_counts,
        tool_call_count,
        turns: loaded.turns,
    }))
}

async fn explorer_run(
    State(state): State<AppState>,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<explorer::RunAnalysis>, ApiError> {
    let query = api_query(query)?;
    let loaded = load_trajectory(&state, &query).await?;
    Ok(Json(explorer::analyze(
        loaded.run,
        &loaded.turns,
        &loaded.records,
    )))
}

#[derive(Debug, Deserialize)]
struct TurnsQuery {
    dataset: Option<String>,
    file: Option<String>,
    run_id: Option<String>,
    agent_id: String,
    session_id: String,
    root_session_id: Option<String>,
    q: Option<String>,
    source: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}

impl TurnsQuery {
    fn session(&self) -> SessionQuery {
        SessionQuery {
            dataset: self.dataset.clone(),
            file: self.file.clone(),
            run_id: self.run_id.clone(),
            agent_id: self.agent_id.clone(),
            session_id: self.session_id.clone(),
            root_session_id: self.root_session_id.clone(),
            offset: None,
            limit: None,
        }
    }
}

async fn explorer_turns(
    State(state): State<AppState>,
    query: Result<Query<TurnsQuery>, QueryRejection>,
) -> Result<Json<explorer::ExplorerPage<explorer::TurnSummary>>, ApiError> {
    let query = api_query(query)?;
    let session = query.session();
    let loaded = load_trajectory(&state, &session).await?;
    Ok(Json(explorer::turn_page(
        &loaded.turns,
        &loaded.records,
        query.q.as_deref(),
        query.source.as_deref(),
        query.offset.unwrap_or(0),
        query.limit.unwrap_or(100),
    )))
}

#[derive(Debug, Deserialize)]
struct TurnDetailQuery {
    dataset: Option<String>,
    file: Option<String>,
    run_id: Option<String>,
    agent_id: String,
    session_id: String,
    root_session_id: Option<String>,
    turn_id: i64,
}

async fn explorer_turn(
    State(state): State<AppState>,
    query: Result<Query<TurnDetailQuery>, QueryRejection>,
) -> Result<Json<explorer::TurnDetail>, ApiError> {
    let query = api_query(query)?;
    let session = SessionQuery {
        dataset: query.dataset,
        file: query.file,
        run_id: query.run_id,
        agent_id: query.agent_id,
        session_id: query.session_id,
        root_session_id: query.root_session_id,
        offset: None,
        limit: None,
    };
    let loaded = load_trajectory(&state, &session).await?;
    let item = loaded
        .turns
        .iter()
        .find(|item| item.turn.id == query.turn_id)
        .ok_or_else(|| ApiError::not_found(format!("turn {} was not found", query.turn_id)))?;
    Ok(Json(explorer::turn_detail(item, &loaded.records)))
}

async fn export_har(
    State(state): State<AppState>,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let query = api_query(query)?;
    Ok(Json(events_to_har(&load_events(&state, &query).await?)))
}

async fn export_otlp(
    State(state): State<AppState>,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let query = api_query(query)?;
    Ok(Json(events_to_otlp_json(
        &load_events(&state, &query).await?,
    )))
}

async fn revisions(
    State(state): State<AppState>,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let query = api_query(query)?;
    let Some(coords) = canonical_run_coords(&state, &query).await? else {
        return Err(ApiError::not_found("canonical event source was not found"));
    };
    Ok(Json(
        serde_json::to_value(read_revisions(&coords).await.map_err(ApiError::internal)?)
            .map_err(anyhow::Error::from)
            .map_err(ApiError::internal)?,
    ))
}

#[derive(Debug, Serialize)]
struct QueryCatalog {
    snapshot_id: String,
    read_only: bool,
    database: String,
    storage_path: String,
    path_column: &'static str,
    datasets: Vec<QueryDatasetSummary>,
    tables: Vec<QueryTableSummary>,
}

#[derive(Debug, Serialize)]
struct QueryDatasetSummary {
    name: String,
    uri: String,
    ready_sources: usize,
    error_sources: usize,
}

#[derive(Debug, Serialize)]
struct QueryTableSummary {
    name: &'static str,
    description: &'static str,
    grain: &'static str,
    fields: Vec<QueryFieldSummary>,
}

#[derive(Debug, Clone, Serialize)]
struct QueryFieldSummary {
    name: &'static str,
    data_type: &'static str,
    description: &'static str,
}

fn field(
    name: &'static str,
    data_type: &'static str,
    description: &'static str,
) -> QueryFieldSummary {
    QueryFieldSummary {
        name,
        data_type,
        description,
    }
}

fn run_query_fields() -> Vec<QueryFieldSummary> {
    vec![
        field(
            "_file_",
            "TEXT",
            "Relative source path; supports = and LIKE",
        ),
        field(
            "run_id",
            "TEXT",
            "Run grouping identifier; not the Storyline primary key",
        ),
        field(
            "run_id_explicit",
            "BOOLEAN",
            "Whether run_id came from source data",
        ),
        field(
            "session_id",
            "TEXT",
            "Storyline identifier within one Catalog source",
        ),
        field("agent_id", "TEXT", "Agent identifier"),
        field("agent_name", "TEXT?", "Agent display name"),
        field("agent_version", "TEXT?", "Agent version"),
        field("agent_model_name", "TEXT?", "Model used by the agent"),
        field(
            "agent_tool_definitions_json",
            "JSON?",
            "Declared tool definitions",
        ),
        field(
            "agent_extra_json",
            "JSON?",
            "Source-specific agent metadata",
        ),
        field("parent_json", "JSON?", "Parent trajectory reference"),
        field(
            "child_session_ids_json",
            "JSON?",
            "Child session identifiers",
        ),
        field("notes", "TEXT?", "Trajectory notes"),
        field("final_metrics_json", "JSON?", "Final evaluation metrics"),
        field(
            "continued_trajectory_ref",
            "TEXT?",
            "Continuation reference",
        ),
        field("extra_json", "JSON?", "Source-specific metadata"),
    ]
}

fn step_query_fields() -> Vec<QueryFieldSummary> {
    vec![
        field(
            "_file_",
            "TEXT",
            "Relative source path; supports = and LIKE",
        ),
        field("run_id", "TEXT", "Owning Run grouping identifier"),
        field("session_id", "TEXT", "Owning Storyline identifier"),
        field("step_id", "BIGINT", "Ordered step number"),
        field("kind", "TEXT?", "Captured step kind"),
        field("effective_kind", "TEXT", "Normalized step kind"),
        field(
            "timestamp",
            "TIMESTAMP(MILLISECOND, UTC)?",
            "UTC millisecond timestamp for ordering and range queries",
        ),
        field("source", "TEXT", "user, agent, or system"),
        field("message_json", "JSON", "Complete normalized message"),
        field(
            "reasoning_content",
            "TEXT?",
            "Reasoning content when present",
        ),
        field("reasoning_effort_json", "JSON?", "Reasoning configuration"),
        field("metrics_json", "JSON?", "Per-step metrics"),
        field("model_name", "TEXT?", "Model for this step"),
        field("llm_call_count", "BIGINT?", "Number of model calls"),
        field(
            "is_copied_context",
            "BOOLEAN?",
            "Whether context was copied",
        ),
        field("latency_ms", "BIGINT?", "End-to-end latency"),
        field("ttft_ms", "BIGINT?", "Time to first token"),
        field(
            "had_observation",
            "BOOLEAN",
            "Whether an observation exists",
        ),
        field("extra_json", "JSON?", "Source-specific metadata"),
    ]
}

fn tool_call_query_fields() -> Vec<QueryFieldSummary> {
    vec![
        field(
            "_file_",
            "TEXT",
            "Relative source path; supports = and LIKE",
        ),
        field("run_id", "TEXT", "Owning Run grouping identifier"),
        field("session_id", "TEXT", "Owning Storyline identifier"),
        field("step_id", "BIGINT", "Owning step number"),
        field("call_index", "BIGINT", "Tool-call order within the step"),
        field("tool_call_id", "TEXT", "Tool-call identifier"),
        field("function_name", "TEXT", "Normalized tool name"),
        field("arguments_json", "JSON", "Complete call arguments"),
        field("results_json", "JSON", "Complete tool results"),
        field("duration_ms", "BIGINT?", "Tool execution duration"),
        field("extra_json", "JSON?", "Source-specific metadata"),
    ]
}

fn trajectory_query_fields() -> Vec<QueryFieldSummary> {
    let mut fields = run_query_fields();
    fields.extend([
        field("step_count", "BIGINT", "Number of steps in the trajectory"),
        field("step_ids", "BIGINT[]", "Ordered step identifiers"),
        field(
            "step_sources",
            "TEXT[]",
            "Ordered user, agent, and system roles",
        ),
        field("messages_json", "JSON[]", "Ordered complete step messages"),
        field("tool_call_count", "BIGINT", "Number of tool calls"),
        field("tool_names", "TEXT[]", "Ordered tool names"),
        field("tool_arguments_json", "JSON[]", "Ordered tool arguments"),
        field("tool_results_json", "JSON[]", "Ordered tool results"),
    ]);
    fields
}

fn source_query_fields() -> Vec<QueryFieldSummary> {
    vec![
        field("_file_", "TEXT", "Dataset-relative logical source path"),
        field("format", "TEXT?", "Detected trajectory format"),
        field("kind", "TEXT", "file or composite store"),
        field(
            "snapshot_ref",
            "TEXT?",
            "Pinned generation, manifest, version, or ETag",
        ),
        field("size_bytes", "BIGINT?", "Discovered source size"),
        field("last_modified", "TEXT?", "Source modification timestamp"),
        field("status", "TEXT", "ready or error"),
        field("error", "TEXT?", "Discovery error in report mode"),
    ]
}

fn event_query_fields() -> Vec<QueryFieldSummary> {
    vec![
        field("_file_", "TEXT", "Dataset-relative canonical events source"),
        field("seq", "BIGINT", "Canonical append sequence"),
        field("event_id", "TEXT?", "Producer event identifier"),
        field("timestamp", "TEXT?", "Captured timestamp"),
        field("kind", "TEXT", "Canonical event kind"),
        field("source", "TEXT", "Event producer"),
        field("agent_id", "TEXT?", "Agent identifier"),
        field("session_id", "TEXT?", "Session identifier"),
        field("call_id", "TEXT?", "Call correlation identifier"),
        field("trace_id", "TEXT?", "Trace correlation identifier"),
        field("parent_call_id", "TEXT?", "Parent call identifier"),
        field("model", "TEXT?", "Captured model"),
        field("payload_json", "JSON", "Canonical event payload"),
    ]
}

async fn query_tables(State(state): State<AppState>) -> Result<Json<QueryCatalog>, ApiError> {
    let runtime = current_catalog(&state).await?;
    let database = runtime
        .snapshot
        .default_dataset()
        .map(str::to_owned)
        .or_else(|| {
            runtime
                .snapshot
                .datasets()
                .first()
                .map(|dataset| dataset.mount.name.clone())
        })
        .unwrap_or_else(|| DEFAULT_DATASET_NAME.into());
    let storage_path = runtime
        .snapshot
        .dataset(&database)
        .map(|dataset| dataset.mount.uri.clone())
        .unwrap_or_default();
    Ok(Json(QueryCatalog {
        snapshot_id: runtime.snapshot.snapshot_id().to_string(),
        read_only: true,
        database,
        storage_path,
        path_column: "_file_",
        datasets: runtime
            .snapshot
            .datasets()
            .iter()
            .map(|dataset| QueryDatasetSummary {
                name: dataset.mount.name.clone(),
                uri: dataset.mount.uri.clone(),
                ready_sources: dataset.ready_source_count(),
                error_sources: dataset.error_source_count(),
            })
            .collect(),
        tables: vec![
            QueryTableSummary {
                name: "sources",
                description: "One row per discovered logical trajectory source",
                grain: "source",
                fields: source_query_fields(),
            },
            QueryTableSummary {
                name: "runs",
                description: "One row per trajectory across the complete data path",
                grain: "trajectory",
                fields: run_query_fields(),
            },
            QueryTableSummary {
                name: "steps",
                description: "Ordered user, agent, and system steps for every trajectory",
                grain: "step",
                fields: step_query_fields(),
            },
            QueryTableSummary {
                name: "tool_calls",
                description: "Structured tool calls joined to their trajectory and step",
                grain: "tool call",
                fields: tool_call_query_fields(),
            },
            QueryTableSummary {
                name: "trajectories",
                description: "One complete trajectory with ordered step and tool-call arrays",
                grain: "complete trajectory",
                fields: trajectory_query_fields(),
            },
            QueryTableSummary {
                name: "events",
                description: "Raw canonical events; empty for non-events sources",
                grain: "event",
                fields: event_query_fields(),
            },
        ],
    }))
}

#[derive(Debug, Deserialize)]
struct QueryEvidenceRequest {
    sql: String,
    max_rows: Option<usize>,
    max_bytes: Option<usize>,
}

#[derive(Debug, Serialize)]
struct QueryEvidence {
    rows: Vec<Value>,
    returned_rows: usize,
    truncated: bool,
    max_rows: usize,
    max_bytes: usize,
    source_routing: &'static str,
    candidate_sources: Option<usize>,
}

async fn query_evidence(
    State(state): State<AppState>,
    request: Result<Json<QueryEvidenceRequest>, JsonRejection>,
) -> Result<Json<QueryEvidence>, ApiError> {
    let Json(request) =
        request.map_err(|_| ApiError::invalid_request("request body must be valid JSON"))?;
    validate_read_only_sql(&request.sql).map_err(ApiError::input)?;
    let max_rows = request.max_rows.unwrap_or(50).clamp(1, 200);
    let max_bytes = request
        .max_bytes
        .unwrap_or(64 * 1024)
        .clamp(1024, 8 * 1024 * 1024);
    let runtime = current_catalog(&state).await?;
    let routed = runtime
        .acceleration
        .route_sql(&runtime.snapshot, &runtime.engine, &request.sql)
        .await;
    let bounded_sql = bounded_evidence_sql(&routed.sql, max_rows);
    let mut output = BoundedOutput::new(max_bytes);
    let write_result = runtime
        .engine
        .write_query_jsonl_with_max_rows(
            &bounded_sql,
            &mut output,
            Some(max_rows.saturating_add(1) as u64),
        )
        .await;
    let bytes = match output.finish(write_result).map_err(ApiError::internal)? {
        QueryEvidenceWriteOutcome::Complete(bytes) => bytes,
        QueryEvidenceWriteOutcome::LimitExceeded => {
            return Err(ApiError::resource_exhausted(
                "query evidence exceeds max_bytes limit",
            ));
        }
    };
    let body = String::from_utf8(bytes)
        .map_err(anyhow::Error::from)
        .map_err(ApiError::internal)?;
    let mut rows = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    for line in body.lines().filter(|line| !line.trim().is_empty()) {
        if rows.len() >= max_rows || bytes.saturating_add(line.len()) > max_bytes {
            truncated = true;
            break;
        }
        rows.push(
            serde_json::from_str(line)
                .map_err(anyhow::Error::from)
                .map_err(ApiError::internal)?,
        );
        bytes += line.len();
    }
    Ok(Json(QueryEvidence {
        returned_rows: rows.len(),
        rows,
        truncated,
        max_rows,
        max_bytes,
        source_routing: routed.outcome.as_str(),
        candidate_sources: routed.candidate_sources,
    }))
}

struct BoundedOutput {
    bytes: Vec<u8>,
    max_bytes: usize,
    exhausted: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum QueryEvidenceWriteOutcome {
    Complete(Vec<u8>),
    LimitExceeded,
}

impl BoundedOutput {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes,
            exhausted: false,
        }
    }

    fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn finish(self, write_result: anyhow::Result<()>) -> anyhow::Result<QueryEvidenceWriteOutcome> {
        match (write_result, self.exhausted()) {
            (_, true) => Ok(QueryEvidenceWriteOutcome::LimitExceeded),
            (Ok(()), false) => Ok(QueryEvidenceWriteOutcome::Complete(self.bytes)),
            (Err(error), false) => Err(error),
        }
    }
}

impl std::io::Write for BoundedOutput {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(buffer.len()) > self.max_bytes {
            self.exhausted = true;
            return Err(std::io::Error::other("bounded query output exhausted"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn bounded_evidence_sql(sql: &str, max_rows: usize) -> String {
    let statement = sql.trim().strip_suffix(';').unwrap_or(sql.trim());
    if statement
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("explain ")
    {
        statement.to_owned()
    } else {
        format!(
            "SELECT * FROM ({statement}) AS __pchronicle_evidence LIMIT {}",
            max_rows.saturating_add(1)
        )
    }
}

fn validate_read_only_sql(sql: &str) -> std::result::Result<(), InputIssue> {
    let statement = sql.trim();
    let statement = statement.strip_suffix(';').unwrap_or(statement).trim_end();
    if statement.is_empty() || statement.contains(';') {
        return Err(InputIssue::invalid(
            "exactly one read-only SQL statement is required",
        ));
    }
    let normalized = statement.trim_start().to_ascii_lowercase();
    let has_keyword = |value: &str, keyword: &str| {
        value
            .strip_prefix(keyword)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    };
    let read_only = has_keyword(&normalized, "select")
        || has_keyword(&normalized, "with")
        || normalized
            .strip_prefix("explain")
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .map(str::trim_start)
            .is_some_and(|rest| has_keyword(rest, "select") || has_keyword(rest, "with"));
    if !read_only {
        return Err(InputIssue::unsupported(
            "only SELECT, WITH, EXPLAIN SELECT, and EXPLAIN WITH are allowed",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

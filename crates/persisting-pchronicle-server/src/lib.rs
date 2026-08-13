//! Local, loopback-only pChronicle browser.

mod acceleration;
mod asset;
mod explorer;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use persisting_pchronicle::{
    events_to_har, events_to_otlp_json, maintain_raw_events, read_judge_rows, read_revisions,
    write_judge_rows, CatalogErrorPolicy, CatalogSnapshotOptions, CatalogStorylineKey,
    ChronicleQueryEngine, DatasetCatalogSnapshot, DatasetMount, EventRecord, JudgeRow,
    LanceMaintenanceOptions, StoryCoords, StorylineTurn, DEFAULT_DATASET_NAME,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use acceleration::{AccelerationStatus, ServerAcceleration};

#[derive(Clone)]
struct AppState {
    storage: Arc<String>,
    config: Arc<ChronicleServerConfig>,
    catalog: Arc<tokio::sync::RwLock<Option<Arc<CatalogRuntime>>>>,
    trajectory_cache: Arc<tokio::sync::RwLock<Option<(String, LoadedTrajectory)>>>,
}

#[derive(Debug, Clone)]
pub struct ChronicleServerConfig {
    pub datasets: Vec<DatasetMount>,
    pub default_dataset: Option<String>,
    pub writable_dataset: Option<String>,
    pub catalog_options: CatalogSnapshotOptions,
}

impl ChronicleServerConfig {
    pub fn legacy(storage: impl Into<String>) -> anyhow::Result<Self> {
        let mount = DatasetMount::default(storage.into())?;
        Ok(Self {
            datasets: vec![mount],
            default_dataset: Some(DEFAULT_DATASET_NAME.into()),
            writable_dataset: Some(DEFAULT_DATASET_NAME.into()),
            catalog_options: CatalogSnapshotOptions::default(),
        })
    }

    pub fn mounted(datasets: Vec<DatasetMount>) -> anyhow::Result<Self> {
        anyhow::ensure!(!datasets.is_empty(), "mount at least one Dataset");
        let default_dataset = (datasets.len() == 1).then(|| datasets[0].name.clone());
        Ok(Self {
            datasets,
            default_dataset,
            writable_dataset: None,
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

#[derive(Debug, Serialize)]
struct ApiError {
    code: &'static str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, Json(self)).into_response()
    }
}

fn api_error(error: impl std::fmt::Display) -> ApiError {
    let code = persisting_pchronicle::classify_error(&error).as_str();
    ApiError {
        code,
        message: error.to_string(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunSummary {
    pub(crate) dataset: String,
    pub(crate) file: String,
    pub(crate) run_id: String,
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

pub fn router(storage: impl Into<String>) -> Router {
    let storage = storage.into();
    let config = ChronicleServerConfig::legacy(storage.clone())
        .expect("legacy pChronicle storage must create the default Dataset");
    router_with_config(config)
}

pub fn router_with_config(config: ChronicleServerConfig) -> Router {
    let storage = config
        .writable_dataset
        .as_deref()
        .or(config.default_dataset.as_deref())
        .and_then(|name| config.datasets.iter().find(|mount| mount.name == name))
        .or_else(|| config.datasets.first())
        .map(|mount| mount.uri.clone())
        .unwrap_or_default();
    let state = AppState {
        storage: Arc::new(storage),
        config: Arc::new(config),
        catalog: Arc::new(tokio::sync::RwLock::new(None)),
        trajectory_cache: Arc::new(tokio::sync::RwLock::new(None)),
    };
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/api/v1/health", get(health))
        .route("/api/v1/runs", get(runs))
        .route("/api/v1/explorer/runs", get(explorer_runs))
        .route("/api/v1/explorer/run", get(explorer_run))
        .route("/api/v1/explorer/turns", get(explorer_turns))
        .route("/api/v1/explorer/turn", get(explorer_turn))
        .route("/api/v1/events", get(events))
        .route("/api/v1/storyline", get(storyline))
        .route("/api/v1/trajectory-view", get(trajectory_view))
        .route("/api/v1/export/har", get(export_har))
        .route("/api/v1/export/otlp", get(export_otlp))
        .route("/api/v1/judgments", get(judgments).post(write_judgments))
        .route("/api/v1/revisions", get(revisions))
        .route("/api/v1/maintain", post(maintain))
        .route("/api/v1/catalog", get(catalog).post(refresh_catalog))
        .route("/api/v1/query/tables", get(query_tables))
        .route("/api/v1/query", post(query_sql))
        .route("/api/v1/query/evidence", post(query_evidence))
        .fallback(asset::fallback)
        .with_state(state)
}

/// Serve the local UI. Non-loopback addresses are deliberately rejected
/// because this single-user surface has no authentication layer.
pub async fn serve(storage: impl Into<String>, addr: SocketAddr) -> anyhow::Result<()> {
    serve_with_config(ChronicleServerConfig::legacy(storage)?, addr).await
}

pub async fn serve_with_config(
    config: ChronicleServerConfig,
    addr: SocketAddr,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        addr.ip().is_loopback(),
        "pChronicle Web UI may only bind to a loopback address"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router_with_config(config))
        .await
        .context("serve pChronicle Web UI")
}

async fn index(headers: axum::http::HeaderMap) -> Response {
    asset::index(headers).await
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"status":"ok","storage":state.storage.as_str()}))
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
    let engine = Arc::new(ChronicleQueryEngine::from_catalog_snapshot(snapshot.clone()).await?);
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
        .map_err(api_error)?;
    let mut catalog = state.catalog.write().await;
    Ok(catalog.get_or_insert_with(|| runtime.clone()).clone())
}

#[derive(Debug, Serialize)]
struct CatalogResponse {
    snapshot_id: String,
    created_at: String,
    default_dataset: Option<String>,
    writable_dataset: Option<String>,
    error_policy: CatalogErrorPolicy,
    datasets: Vec<persisting_pchronicle::CatalogDataset>,
    acceleration: AccelerationStatus,
}

fn catalog_response(state: &AppState, runtime: &CatalogRuntime) -> CatalogResponse {
    CatalogResponse {
        snapshot_id: runtime.snapshot.snapshot_id().to_string(),
        created_at: runtime.snapshot.created_at().to_string(),
        default_dataset: runtime.snapshot.default_dataset().map(str::to_owned),
        writable_dataset: state.config.writable_dataset.clone(),
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
        .map_err(api_error)?;
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
        .map_err(api_error)
}

async fn explorer_runs(
    State(state): State<AppState>,
    Query(query): Query<explorer::ExplorerRunsQuery>,
) -> Result<Json<explorer::RunExplorerPage>, ApiError> {
    let summaries = load_run_summaries(&state).await?;
    let mut judgments_by_run = BTreeMap::new();
    for run in &summaries {
        let session_query = SessionQuery {
            dataset: Some(run.dataset.clone()),
            file: Some(run.file.clone()),
            run_id: Some(run.run_id.clone()),
            agent_id: run.agent_id.clone(),
            session_id: run.session_id.clone(),
            root_session_id: run.root_session_id.clone(),
            offset: None,
            limit: None,
        };
        let rows = session_judgments(&state, &session_query)
            .await
            .unwrap_or_default();
        judgments_by_run.insert(explorer::run_key(run), rows);
    }
    Ok(Json(explorer::run_page(
        summaries,
        &judgments_by_run,
        &query,
    )))
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
                    .is_none_or(|value| value == &run.run_id)
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
    if matches.len() != 1 {
        return Err(api_error(format!(
            "trajectory selector resolved {} Storylines; include dataset, _file_, and session_id",
            matches.len()
        )));
    }
    Ok(matches.into_iter().next().expect("one matching run"))
}

fn catalog_storyline_key(run: &RunSummary) -> CatalogStorylineKey {
    CatalogStorylineKey {
        dataset: run.dataset.clone(),
        file: run.file.clone(),
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
        .map_err(api_error)?;
    event_uri
        .map(|event_uri| event_uri_coords(event_uri, run).map_err(api_error))
        .transpose()
}

async fn writable_run_coords(
    state: &AppState,
    query: &SessionQuery,
    required: bool,
) -> Result<Option<StoryCoords>, ApiError> {
    let run = resolve_run_summary(state, query).await?;
    let Some(writable_dataset) = state.config.writable_dataset.as_deref() else {
        if required {
            return Err(api_error(
                "pChronicle Web was started without --writable-dataset",
            ));
        }
        return Ok(None);
    };
    if run.dataset != writable_dataset {
        if required {
            return Err(api_error(format!(
                "Dataset '{}' is read-only; writable Dataset is '{}'",
                run.dataset, writable_dataset
            )));
        }
        return Ok(None);
    }
    let Some(coords) = canonical_run_coords_for_summary(state, &run).await? else {
        if required {
            return Err(api_error(format!(
                "Dataset source '{}/{}' is not a writable canonical events source",
                run.dataset, run.file
            )));
        }
        return Ok(None);
    };
    Ok(Some(coords))
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
        .map_err(api_error)?
        .ok_or_else(|| api_error("trajectory was not found in the active Catalog snapshot"))?;
    let offset = query.offset.unwrap_or(0).min(document.events.len());
    let end = query
        .limit
        .map(|limit| offset.saturating_add(limit).min(document.events.len()))
        .unwrap_or(document.events.len());
    Ok(document.events[offset..end].to_vec())
}

async fn events(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
) -> Result<Json<Value>, ApiError> {
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
    Query(query): Query<SessionQuery>,
) -> Result<Json<Value>, ApiError> {
    let run = resolve_run_summary(&state, &query).await?;
    let runtime = current_catalog(&state).await?;
    let document = runtime
        .snapshot
        .load_storyline(&catalog_storyline_key(&run))
        .await
        .map_err(api_error)?
        .ok_or_else(|| api_error("trajectory was not found in the active Catalog snapshot"))?;
    Ok(Json(serde_json::to_value(document).map_err(api_error)?))
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

fn count_tool_calls(value: &Value) -> usize {
    match value {
        Value::Array(items) => items.iter().map(count_tool_calls).sum(),
        Value::Object(map) => {
            let here = map
                .get("tool_calls")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
                + usize::from(matches!(
                    map.get("type").and_then(Value::as_str),
                    Some("tool_use" | "function_call" | "custom_tool_call" | "local_shell_call")
                ));
            here + map
                .iter()
                .filter(|(key, _)| key.as_str() != "tool_calls")
                .map(|(_, value)| count_tool_calls(value))
                .sum::<usize>()
        }
        _ => 0,
    }
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
    let records = runtime
        .snapshot
        .load_events(&key)
        .await
        .map_err(api_error)?
        .ok_or_else(|| api_error("trajectory was not found in the active Catalog snapshot"))?
        .events;
    let document = runtime
        .snapshot
        .load_storyline(&key)
        .await
        .map_err(api_error)?
        .ok_or_else(|| api_error("trajectory was not found in the active Catalog snapshot"))?;
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
            let event_seqs = call_id
                .as_ref()
                .and_then(|id| by_call.get(id).cloned())
                .or_else(|| turn_seq(&turn).map(|seq| vec![seq]))
                .unwrap_or_default();
            let mut wire_tool_calls = Vec::new();
            for event in records
                .iter()
                .filter(|event| event_seqs.contains(&event.seq))
            {
                collect_wire_tool_calls(&event.payload, &mut wire_tool_calls);
            }
            wire_tool_calls.dedup_by(|left, right| {
                left.id == right.id && left.name == right.name && left.arguments == right.arguments
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
    Query(query): Query<SessionQuery>,
) -> Result<Json<TrajectoryView>, ApiError> {
    let loaded = load_trajectory(&state, &query).await?;
    let mut event_kind_counts = BTreeMap::new();
    let mut tool_call_count = 0;
    for event in &loaded.records {
        *event_kind_counts.entry(event.kind.clone()).or_insert(0) += 1;
        tool_call_count += count_tool_calls(&event.payload);
    }
    Ok(Json(TrajectoryView {
        run: loaded.run,
        event_kind_counts,
        tool_call_count,
        turns: loaded.turns,
    }))
}

async fn session_judgments(
    state: &AppState,
    query: &SessionQuery,
) -> Result<Vec<JudgeRow>, ApiError> {
    let Some(coords) = canonical_run_coords(state, query).await? else {
        return Ok(Vec::new());
    };
    Ok(read_judge_rows(&coords)
        .await
        .map_err(api_error)?
        .into_iter()
        .filter(|row| row.session_id == query.session_id)
        .collect())
}

async fn explorer_run(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
) -> Result<Json<explorer::RunAnalysis>, ApiError> {
    let loaded = load_trajectory(&state, &query).await?;
    let judgments = session_judgments(&state, &query).await?;
    Ok(Json(explorer::analyze(
        loaded.run,
        &loaded.turns,
        &loaded.records,
        &judgments,
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
    Query(query): Query<TurnsQuery>,
) -> Result<Json<explorer::ExplorerPage<explorer::TurnSummary>>, ApiError> {
    let session = query.session();
    let loaded = load_trajectory(&state, &session).await?;
    let judgments = session_judgments(&state, &session).await?;
    Ok(Json(explorer::turn_page(
        &loaded.turns,
        &loaded.records,
        &judgments,
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
    Query(query): Query<TurnDetailQuery>,
) -> Result<Json<explorer::TurnDetail>, ApiError> {
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
        .ok_or_else(|| ApiError {
            code: "turn_not_found",
            message: format!("turn {} was not found", query.turn_id),
        })?;
    let judgments = session_judgments(&state, &session).await?;
    Ok(Json(explorer::turn_detail(
        item,
        &loaded.records,
        &judgments,
    )))
}

async fn export_har(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(events_to_har(&load_events(&state, &query).await?)))
}

async fn export_otlp(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(events_to_otlp_json(
        &load_events(&state, &query).await?,
    )))
}

async fn judgments(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
) -> Result<Json<Value>, ApiError> {
    let rows = session_judgments(&state, &query).await?;
    Ok(Json(Value::Array(
        rows.into_iter()
            .map(|row| {
                json!({
                    "session_id":row.session_id,"call_id":row.call_id,"rubric_id":row.rubric_id,
                    "score":row.score,"verdict":row.verdict,"rationale":row.rationale
                })
            })
            .collect(),
    )))
}

#[derive(Debug, Deserialize)]
struct JudgmentWrite {
    dataset: Option<String>,
    file: Option<String>,
    run_id: Option<String>,
    agent_id: String,
    session_id: String,
    root_session_id: Option<String>,
    call_id: String,
    rubric_id: String,
    score: i64,
    verdict: String,
    rationale: String,
}

async fn write_judgments(
    State(state): State<AppState>,
    Json(mut request): Json<JudgmentWrite>,
) -> Result<Json<Value>, ApiError> {
    request.call_id = request.call_id.trim().to_string();
    request.rubric_id = request.rubric_id.trim().to_string();
    request.verdict = request.verdict.trim().to_ascii_lowercase();
    request.rationale = request.rationale.trim().to_string();
    if request.call_id.is_empty() {
        return Err(ApiError {
            code: "invalid_judgment",
            message: "call_id is required; use __story__ for a trajectory judgment".into(),
        });
    }
    if request.rubric_id.is_empty() {
        return Err(ApiError {
            code: "invalid_judgment",
            message: "rubric_id is required".into(),
        });
    }
    if request.rationale.is_empty() {
        return Err(ApiError {
            code: "invalid_judgment",
            message: "rationale is required".into(),
        });
    }
    if !(0..=100).contains(&request.score) {
        return Err(ApiError {
            code: "invalid_judgment",
            message: "score must be between 0 and 100".into(),
        });
    }
    if !matches!(request.verdict.as_str(), "pass" | "partial" | "fail") {
        return Err(ApiError {
            code: "invalid_judgment",
            message: "verdict must be pass, partial, or fail".into(),
        });
    }
    let query = SessionQuery {
        dataset: request.dataset,
        file: request.file,
        run_id: request.run_id,
        agent_id: request.agent_id,
        session_id: request.session_id.clone(),
        root_session_id: request.root_session_id,
        offset: None,
        limit: None,
    };
    let row = JudgeRow {
        session_id: request.session_id,
        call_id: request.call_id,
        rubric_id: request.rubric_id,
        score: request.score,
        verdict: request.verdict,
        rationale: request.rationale,
    };
    let response_row = json!({
        "session_id": row.session_id,
        "call_id": row.call_id,
        "rubric_id": row.rubric_id,
        "score": row.score,
        "verdict": row.verdict,
        "rationale": row.rationale,
    });
    let coords = writable_run_coords(&state, &query, true)
        .await?
        .expect("required writable coordinates");
    let dataset = write_judge_rows(&coords, &[row]).await.map_err(api_error)?;
    Ok(Json(
        json!({"status":"ok","dataset":dataset,"judgment":response_row}),
    ))
}

async fn revisions(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
) -> Result<Json<Value>, ApiError> {
    let Some(coords) = canonical_run_coords(&state, &query).await? else {
        return Ok(Json(json!([])));
    };
    Ok(Json(
        serde_json::to_value(read_revisions(&coords).await.map_err(api_error)?)
            .map_err(api_error)?,
    ))
}

async fn maintain(
    State(state): State<AppState>,
    Json(query): Json<SessionQuery>,
) -> Result<Json<Value>, ApiError> {
    let coords = writable_run_coords(&state, &query, true)
        .await?
        .expect("required writable coordinates");
    let report = maintain_raw_events(&coords, &LanceMaintenanceOptions::default())
        .await
        .map_err(api_error)?;
    Ok(Json(json!({
        "status":"ok", "fragments_removed":report.fragments_removed,
        "fragments_added":report.fragments_added, "old_versions_removed":report.old_versions_removed,
        "bytes_removed":report.bytes_removed, "final_version":report.final_version
    })))
}

#[derive(Debug, Deserialize)]
struct SqlRequest {
    sql: String,
}

#[derive(Debug, Serialize)]
struct QueryCatalog {
    snapshot_id: String,
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
        field("schema_version", "TEXT", "Normalized schema version"),
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
        field("timestamp", "TEXT?", "Captured timestamp"),
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

async fn query_sql(
    State(state): State<AppState>,
    Json(request): Json<SqlRequest>,
) -> Result<Response, ApiError> {
    validate_read_only_sql(&request.sql)?;
    let runtime = current_catalog(&state).await?;
    let routed = runtime
        .acceleration
        .route_sql(&runtime.snapshot, &runtime.engine, &request.sql)
        .await;
    let body = runtime
        .engine
        .query_jsonl(&routed.sql)
        .await
        .map_err(api_error)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (
                header::HeaderName::from_static("x-pchronicle-source-routing"),
                routed.outcome.as_str(),
            ),
        ],
        body,
    )
        .into_response())
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
    Json(request): Json<QueryEvidenceRequest>,
) -> Result<Json<QueryEvidence>, ApiError> {
    validate_read_only_sql(&request.sql)?;
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
    let body = runtime
        .engine
        .query_jsonl(&routed.sql)
        .await
        .map_err(api_error)?;
    let mut rows = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    for line in body.lines().filter(|line| !line.trim().is_empty()) {
        if rows.len() >= max_rows || bytes.saturating_add(line.len()) > max_bytes {
            truncated = true;
            break;
        }
        rows.push(serde_json::from_str(line).map_err(api_error)?);
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

fn validate_read_only_sql(sql: &str) -> Result<(), ApiError> {
    let statement = sql.trim();
    let statement = statement.strip_suffix(';').unwrap_or(statement).trim_end();
    if statement.is_empty() || statement.contains(';') {
        return Err(ApiError {
            code: "read_only_sql",
            message: "exactly one read-only SQL statement is required".into(),
        });
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
        return Err(ApiError {
            code: "read_only_sql",
            message: "only SELECT, WITH, EXPLAIN SELECT, and EXPLAIN WITH are allowed".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn health_exposes_selected_storage() {
        use http_body_util::BodyExt;
        use tower::ServiceExt;
        let response = router("/tmp/chronicle-test")
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["storage"], "/tmp/chronicle-test");
    }

    #[tokio::test]
    async fn rejects_non_loopback_bind() {
        let error = serve(
            "/tmp/none",
            SocketAddr::new(std::net::IpAddr::from([0, 0, 0, 0]), 0),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("loopback"));
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
    fn canonical_event_uri_resolves_write_coordinates_independent_of_mount_root() {
        let run = RunSummary {
            dataset: "live".into(),
            file: "agent/run-1/events.lance".into(),
            run_id: "child".into(),
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
    async fn spa_shell_does_not_capture_unknown_api_paths() {
        use tower::ServiceExt;
        let response = router("/tmp/chronicle-test")
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/not-a-route")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
                    .uri("/api/v1/query/tables")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(tables.status(), StatusCode::OK);
        let tables: Value =
            serde_json::from_slice(&tables.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
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
                    .uri("/api/v1/query")
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
        let row: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(row["session_id"], "json-session");
        assert_eq!(row["step_count"], 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn server_routing_index_prunes_point_queries_and_resets_on_refresh() -> anyhow::Result<()>
    {
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
                    .uri("/api/v1/query")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(
                        json!({"sql":"SELECT _file_, session_id FROM runs WHERE session_id = 'json-session'"})
                            .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(routed.status(), StatusCode::OK);
        assert_eq!(
            routed
                .headers()
                .get("x-pchronicle-source-routing")
                .and_then(|value| value.to_str().ok()),
            Some("applied")
        );
        let body = routed.into_body().collect().await?.to_bytes();
        let row: Value = serde_json::from_slice(&body)?;
        assert_eq!(row["_file_"], "gateway.json");
        assert_eq!(row["session_id"], "json-session");

        let quoted_alias = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/query")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(
                        json!({"sql":"SELECT \"R\".session_id FROM runs AS \"R\" WHERE \"R\".session_id = 'json-session'"})
                            .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(quoted_alias.status(), StatusCode::OK);
        assert_eq!(
            quoted_alias
                .headers()
                .get("x-pchronicle-source-routing")
                .and_then(|value| value.to_str().ok()),
            Some("applied")
        );

        let catalog = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/catalog")
                    .body(axum::body::Body::empty())?,
            )
            .await?;
        let catalog: Value =
            serde_json::from_slice(&catalog.into_body().collect().await?.to_bytes())?;
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
                    .uri("/api/v1/query")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(
                        json!({"sql":"SELECT session_id FROM runs WHERE _file_ = 'gateway.json' AND session_id = 'json-session'"})
                            .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(
            already_pruned
                .headers()
                .get("x-pchronicle-source-routing")
                .and_then(|value| value.to_str().ok()),
            Some("already_pruned")
        );

        let refreshed = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/catalog")
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
        let app = router_with_config(config);

        let initial = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/catalog")
                    .body(axum::body::Body::empty())?,
            )
            .await?;
        assert_eq!(initial.status(), StatusCode::OK);
        let initial: Value =
            serde_json::from_slice(&initial.into_body().collect().await?.to_bytes())?;
        assert_eq!(initial["default_dataset"], Value::Null);
        assert_eq!(initial["datasets"].as_array().unwrap().len(), 2);
        let initial_snapshot = initial["snapshot_id"].as_str().unwrap().to_string();

        let filtered = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/explorer/runs?dataset=archive&limit=10")
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
                    .uri("/api/v1/catalog")
                    .body(axum::body::Body::empty())?,
            )
            .await?;
        assert_eq!(failed_refresh.status(), StatusCode::BAD_REQUEST);

        let preserved = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/catalog")
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
                    .uri("/api/v1/catalog")
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
    async fn explorer_routes_page_runs_and_lazy_load_turn_evidence() {
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let root = json_dataset_root();
        let app = router(root.to_string_lossy().to_string());
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/explorer/runs?status=active&limit=1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
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
                    .uri("/api/v1/explorer/runs?path=dataset%2Fgateway.json%2Fjson-job&limit=10")
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
                    .uri(format!("/api/v1/explorer/run?{coordinates}"))
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
                    .uri(format!("/api/v1/explorer/turns?{coordinates}&limit=10"))
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
                    .uri(format!("/api/v1/explorer/turn?{coordinates}&turn_id=1"))
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
                    .uri("/api/v1/explorer/runs?status=completed&limit=10")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(page["snapshot"]["total"], 1);
        assert_eq!(page["records"][0]["session_id"], "completed-session");
        assert_eq!(page["records"][0]["status"], "completed");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn read_only_mounts_expose_existing_judgments() -> anyhow::Result<()> {
        use http_body_util::BodyExt;
        use persisting_pchronicle::{RawEventLanceStore, StructuredStore};
        use tower::ServiceExt;

        let root = tempfile::tempdir()?;
        let coords = StoryCoords::new(
            root.path().to_string_lossy(),
            "agent",
            "child-session",
            Some("shared-run".into()),
        );
        RawEventLanceStore
            .append_events(
                &coords,
                &[EventRecord {
                    identity: Default::default(),
                    seq: 0,
                    source: "test".into(),
                    kind: "note".into(),
                    timestamp: None,
                    session_id: Some("child-session".into()),
                    agent_id: Some("agent".into()),
                    parent_uuid: None,
                    trace_id: None,
                    call_id: None,
                    subagent_id: None,
                    parent_agent_id: None,
                    branch: None,
                    parent_call_id: None,
                    payload: json!({"content":"captured"}),
                }],
            )
            .await?;
        write_judge_rows(
            &coords,
            &[JudgeRow {
                session_id: "child-session".into(),
                call_id: "__story__".into(),
                rubric_id: "quality".into(),
                score: 91,
                verdict: "pass".into(),
                rationale: "stored before mounting read-only".into(),
            }],
        )
        .await?;

        let app = router_with_config(ChronicleServerConfig::mounted(vec![DatasetMount::new(
            "archive",
            root.path().to_string_lossy(),
        )?])?);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri(
                        "/api/v1/judgments?dataset=archive&file=agent%2Fshared-run%2Fevents.lance&run_id=shared-run&agent_id=agent&session_id=child-session",
                    )
                    .body(axum::body::Body::empty())?,
            )
            .await?;
        let response_status = response.status();
        let response_body = response.into_body().collect().await?.to_bytes();
        assert_eq!(
            response_status,
            StatusCode::OK,
            "judgment read failed: {}",
            String::from_utf8_lossy(&response_body)
        );
        let rows: Value = serde_json::from_slice(&response_body)?;
        assert_eq!(rows[0]["score"], 91);
        assert_eq!(rows[0]["session_id"], "child-session");
        Ok(())
    }

    #[tokio::test]
    async fn limited_query_and_judgment_validation_enforce_copilot_boundaries() {
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let root = json_dataset_root();
        let app = router(root.to_string_lossy().to_string());
        let tables = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/query/tables")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let tables: Value =
            serde_json::from_slice(&tables.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let database = tables["database"].as_str().unwrap();
        let evidence = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/query/evidence")
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
            serde_json::from_slice(&evidence.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(evidence["returned_rows"], 1);
        assert_eq!(evidence["max_rows"], 1);
        assert_eq!(evidence["max_bytes"], 1_048_576);

        let valid = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/judgments")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(
                        json!({
                            "agent_id":"model-json","session_id":"json-session",
                            "root_session_id":"json-job","call_id":"__story__",
                            "rubric_id":"quality","score":88,"verdict":"pass",
                            "rationale":"Evidence supports the trajectory-level verdict."
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(valid.status(), StatusCode::BAD_REQUEST);

        let saved = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/judgments?agent_id=model-json&session_id=json-session&root_session_id=json-job")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let saved_status = saved.status();
        let saved_body = saved.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            saved_status,
            StatusCode::OK,
            "judgments failed: {}",
            String::from_utf8_lossy(&saved_body)
        );
        let saved: Value = serde_json::from_slice(&saved_body).unwrap();
        assert_eq!(saved, json!([]));

        let invalid = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/judgments")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(
                        json!({
                            "agent_id":"model-json","session_id":"json-session",
                            "root_session_id":"json-job","call_id":"__story__",
                            "rubric_id":"quality","score":101,"verdict":"pass","rationale":""
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
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
}

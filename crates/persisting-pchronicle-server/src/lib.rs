//! Local, loopback-only pChronicle browser.

mod asset;
mod dataset;
mod explorer;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use persisting_pchronicle::{
    events_to_har, events_to_otlp_json, events_to_storyline, expand_story_locations,
    list_story_read_locations, maintain_raw_events, read_judge_rows, read_revisions,
    write_judge_rows, Chronicle, ChronicleQueryEngine, EventRecord, EventsDocument, JudgeRow,
    LanceMaintenanceOptions, StoryCoords, StorylineTurn, TrajectoryReplayRequest,
    TrajectoryStatsRequest, TrajectoryStorageFormat,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::StreamExt;

#[derive(Clone)]
struct AppState {
    storage: Arc<String>,
    chronicle: Chronicle,
    dataset: Option<Arc<dataset::DatasetStore>>,
    trajectory_cache: Arc<tokio::sync::RwLock<Option<(String, LoadedTrajectory)>>>,
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
    pub(crate) agent_id: String,
    pub(crate) session_id: String,
    pub(crate) root_session_id: Option<String>,
    pub(crate) offset: Option<usize>,
    pub(crate) limit: Option<usize>,
}

fn coords(storage: &str, query: &SessionQuery) -> StoryCoords {
    StoryCoords::new(
        storage,
        &query.agent_id,
        &query.session_id,
        query.root_session_id.clone(),
    )
}

fn logical_run_path(agent_id: &str, session_id: &str, root_session_id: Option<&str>) -> String {
    match root_session_id {
        Some(root) if root != session_id => {
            format!("{agent_id}/{root}/subagents/{session_id}")
        }
        Some(root) => format!("{agent_id}/{root}"),
        None => format!("{agent_id}/{session_id}"),
    }
}

pub fn router(storage: impl Into<String>) -> Router {
    let storage = storage.into();
    let dataset = dataset::DatasetStore::discover(&storage)
        .map(|dataset| dataset.map(Arc::new))
        .unwrap_or_else(|error| {
            eprintln!("[persisting-pchronicle-server] dataset discovery failed: {error}");
            None
        });
    let state = AppState {
        storage: Arc::new(storage),
        chronicle: Chronicle,
        dataset,
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
        .route("/api/v1/stream", get(stream))
        .route("/api/v1/storyline", get(storyline))
        .route("/api/v1/trajectory-view", get(trajectory_view))
        .route("/api/v1/export/har", get(export_har))
        .route("/api/v1/export/otlp", get(export_otlp))
        .route("/api/v1/judgments", get(judgments).post(write_judgments))
        .route("/api/v1/revisions", get(revisions))
        .route("/api/v1/maintain", post(maintain))
        .route("/api/v1/query/tables", get(query_tables))
        .route("/api/v1/query", post(query_sql))
        .route("/api/v1/query/evidence", post(query_evidence))
        .fallback(asset::fallback)
        .with_state(state)
}

/// Serve the local UI. Non-loopback addresses are deliberately rejected
/// because this single-user surface has no authentication layer.
pub async fn serve(storage: impl Into<String>, addr: SocketAddr) -> anyhow::Result<()> {
    anyhow::ensure!(
        addr.ip().is_loopback(),
        "pChronicle Web UI may only bind to a loopback address"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(storage))
        .await
        .context("serve pChronicle Web UI")
}

async fn index(headers: axum::http::HeaderMap) -> Response {
    asset::index(headers).await
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"status":"ok","storage":state.storage.as_str()}))
}

async fn runs(State(state): State<AppState>) -> Result<Json<Vec<RunSummary>>, ApiError> {
    Ok(Json(load_run_summaries(&state).await?))
}

async fn load_run_summaries(state: &AppState) -> Result<Vec<RunSummary>, ApiError> {
    if let Some(dataset) = &state.dataset {
        return Ok(dataset.summaries());
    }
    let roots = match list_story_read_locations(state.storage.as_ref().clone(), None, None, None) {
        Ok(roots) => roots,
        Err(error) if is_empty_storage_error(&error.to_string()) => return Ok(Vec::new()),
        Err(error) => return Err(api_error(error)),
    };
    let locations = expand_story_locations(roots).await.map_err(api_error)?;
    let mut out = Vec::with_capacity(locations.len());
    for location in locations {
        let stats = state
            .chronicle
            .stats(TrajectoryStatsRequest {
                storage: location.storage.clone(),
                agent_id: location.agent_id.clone(),
                session_id: location.session_id.clone(),
                root_session_id: location.root_session_id.clone(),
                storage_format: TrajectoryStorageFormat::Lance,
            })
            .await
            .map_err(api_error)?;
        let path = logical_run_path(
            &location.agent_id,
            &location.session_id,
            location.root_session_id.as_deref(),
        );
        out.push(RunSummary {
            agent_id: location.agent_id,
            model_name: None,
            session_id: location.session_id,
            root_session_id: location.root_session_id,
            path,
            row_count: stats.row_count,
            duplicate_event_ids: stats.duplicate_event_ids,
            status: stats.status,
        });
    }
    Ok(out)
}

async fn explorer_runs(
    State(state): State<AppState>,
    Query(query): Query<explorer::ExplorerRunsQuery>,
) -> Result<Json<explorer::RunExplorerPage>, ApiError> {
    let summaries = load_run_summaries(&state).await?;
    let mut judgments_by_run = BTreeMap::new();
    for run in &summaries {
        let session_query = SessionQuery {
            agent_id: run.agent_id.clone(),
            session_id: run.session_id.clone(),
            root_session_id: run.root_session_id.clone(),
            offset: None,
            limit: None,
        };
        let rows = read_judge_rows(&coords(&state.storage, &session_query))
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|row| row.session_id == run.session_id)
            .collect();
        judgments_by_run.insert(explorer::run_key(run), rows);
    }
    Ok(Json(explorer::run_page(
        summaries,
        &judgments_by_run,
        &query,
    )))
}

fn is_empty_storage_error(message: &str) -> bool {
    message.contains("no trajectory sessions under") || message.contains("no sessions under")
}

async fn load_events(state: &AppState, query: &SessionQuery) -> Result<Vec<EventRecord>, ApiError> {
    if let Some(dataset) = &state.dataset {
        if dataset.contains(query) {
            let run = dataset.load(query).map_err(api_error)?;
            let offset = query.offset.unwrap_or(0).min(run.records.len());
            let end = query
                .limit
                .map(|limit| offset.saturating_add(limit).min(run.records.len()))
                .unwrap_or(run.records.len());
            return Ok(run.records[offset..end].to_vec());
        }
    }
    let replay = state
        .chronicle
        .replay(TrajectoryReplayRequest {
            storage: state.storage.as_ref().clone(),
            agent_id: query.agent_id.clone(),
            session_id: query.session_id.clone(),
            root_session_id: query.root_session_id.clone(),
            offset: query.offset.unwrap_or(0),
            limit: query.limit.or(Some(1000)),
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .map_err(api_error)?;
    replay
        .records
        .into_iter()
        .map(|line| serde_json::from_str(&line).map_err(api_error))
        .collect()
}

async fn events(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
) -> Result<Json<Value>, ApiError> {
    let offset = query.offset.unwrap_or(0);
    let requested_limit = query.limit.unwrap_or(1000);
    let records = load_events(&state, &query).await?;
    if let Some(summary) = state
        .dataset
        .as_ref()
        .and_then(|dataset| dataset.summary(&query))
    {
        let next_offset = offset + records.len();
        return Ok(Json(json!({
            "snapshot": {
                "offset": offset,
                "next_offset": next_offset,
                "total": summary.row_count,
                "has_more": next_offset < summary.row_count && !records.is_empty(),
                "limit": requested_limit
            },
            "records": records
        })));
    }
    let stats = state
        .chronicle
        .stats(TrajectoryStatsRequest {
            storage: state.storage.as_ref().clone(),
            agent_id: query.agent_id.clone(),
            session_id: query.session_id.clone(),
            root_session_id: query.root_session_id.clone(),
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .map_err(api_error)?;
    let next_offset = offset + records.len();
    Ok(Json(json!({
        "snapshot": {
            "offset": offset,
            "next_offset": next_offset,
            "total": stats.row_count,
            "has_more": next_offset < stats.row_count && !records.is_empty(),
            "limit": requested_limit
        },
        "records": records
    })))
}

async fn stream(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
) -> Sse<impl tokio_stream::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let stream = IntervalStream::new(tokio::time::interval(std::time::Duration::from_secs(1)))
        .then(move |_| {
            let state = state.clone();
            let query = SessionQuery {
                agent_id: query.agent_id.clone(),
                session_id: query.session_id.clone(),
                root_session_id: query.root_session_id.clone(),
                offset: None,
                limit: None,
            };
            async move {
                if let Some(dataset) = &state.dataset {
                    if let Some(run) = dataset.summaries().into_iter().find(|run| {
                        run.agent_id == query.agent_id && run.session_id == query.session_id
                    }) {
                        let payload = json!({"row_count":run.row_count,"status":run.status});
                        return Ok(SseEvent::default()
                            .event("snapshot")
                            .data(payload.to_string()));
                    }
                }
                let result = state
                    .chronicle
                    .stats(TrajectoryStatsRequest {
                        storage: state.storage.as_ref().clone(),
                        agent_id: query.agent_id,
                        session_id: query.session_id,
                        root_session_id: query.root_session_id,
                        storage_format: TrajectoryStorageFormat::Lance,
                    })
                    .await;
                let payload = match result {
                    Ok(stats) => json!({"row_count":stats.row_count,"status":stats.status}),
                    Err(error) => json!({"error":error.to_string()}),
                };
                Ok(SseEvent::default()
                    .event("snapshot")
                    .data(payload.to_string()))
            }
        });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn storyline(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
) -> Result<Json<Value>, ApiError> {
    let document = EventsDocument::new(load_events(&state, &query).await?);
    Ok(Json(
        serde_json::to_value(events_to_storyline(&document).map_err(api_error)?)
            .map_err(api_error)?,
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
    if let Some(dataset) = &state.dataset {
        if dataset.contains(query) {
            let cache_key = format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}",
                query.agent_id,
                query.session_id,
                query.root_session_id.as_deref().unwrap_or_default(),
                dataset.fingerprint(query).unwrap_or_default()
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
            let loaded = dataset.load(query).map_err(api_error)?;
            let loaded = LoadedTrajectory {
                run: loaded.summary,
                records: loaded.records,
                turns: loaded.turns,
            };
            *state.trajectory_cache.write().await = Some((cache_key, loaded.clone()));
            return Ok(loaded);
        }
    }

    let stats = state
        .chronicle
        .stats(TrajectoryStatsRequest {
            storage: state.storage.as_ref().clone(),
            agent_id: query.agent_id.clone(),
            session_id: query.session_id.clone(),
            root_session_id: query.root_session_id.clone(),
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .map_err(api_error)?;
    let cache_key = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        query.agent_id,
        query.session_id,
        query.root_session_id.as_deref().unwrap_or_default(),
        stats.row_count,
        stats.status
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
    let full_query = SessionQuery {
        offset: Some(0),
        limit: None,
        ..query.clone()
    };
    let records = load_events(state, &full_query).await?;
    let mut by_call = BTreeMap::<String, Vec<u64>>::new();
    for event in &records {
        if let Some(call_id) = event.call_id.as_ref().filter(|id| !id.is_empty()) {
            by_call.entry(call_id.clone()).or_default().push(event.seq);
        }
    }
    let document = events_to_storyline(&EventsDocument::new(records.clone())).map_err(api_error)?;
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
        run: RunSummary {
            agent_id: query.agent_id.clone(),
            model_name: None,
            session_id: query.session_id.clone(),
            root_session_id: query.root_session_id.clone(),
            path: logical_run_path(
                &query.agent_id,
                &query.session_id,
                query.root_session_id.as_deref(),
            ),
            row_count: stats.row_count,
            duplicate_event_ids: stats.duplicate_event_ids,
            status: stats.status,
        },
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
    Ok(read_judge_rows(&coords(&state.storage, query))
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
    let rows = read_judge_rows(&coords(&state.storage, &query))
        .await
        .map_err(api_error)?;
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
    let dataset = write_judge_rows(&coords(&state.storage, &query), &[row])
        .await
        .map_err(api_error)?;
    Ok(Json(
        json!({"status":"ok","dataset":dataset,"judgment":response_row}),
    ))
}

async fn revisions(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(
            read_revisions(&coords(&state.storage, &query))
                .await
                .map_err(api_error)?,
        )
        .map_err(api_error)?,
    ))
}

async fn maintain(
    State(state): State<AppState>,
    Json(query): Json<SessionQuery>,
) -> Result<Json<Value>, ApiError> {
    let report = maintain_raw_events(
        &coords(&state.storage, &query),
        &LanceMaintenanceOptions::default(),
    )
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
    database: String,
    storage_path: String,
    path_column: &'static str,
    tables: Vec<QueryTableSummary>,
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
        field("run_id", "TEXT", "Stable trajectory identifier"),
        field(
            "run_id_explicit",
            "BOOLEAN",
            "Whether run_id came from source data",
        ),
        field("session_id", "TEXT", "Source session identifier"),
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
        field("run_id", "TEXT", "Owning trajectory identifier"),
        field("session_id", "TEXT", "Owning session identifier"),
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
        field("run_id", "TEXT", "Owning trajectory identifier"),
        field("session_id", "TEXT", "Owning session identifier"),
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

async fn query_tables(State(state): State<AppState>) -> Json<QueryCatalog> {
    Json(QueryCatalog {
        database: database_name(&state.storage),
        storage_path: state.storage.as_ref().clone(),
        path_column: "_file_",
        tables: vec![
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
        ],
    })
}

async fn query_sql(
    State(state): State<AppState>,
    Json(request): Json<SqlRequest>,
) -> Result<Response, ApiError> {
    validate_read_only_sql(&request.sql)?;
    let (engine, _) = directory_query_engine(&state).await.map_err(api_error)?;
    let body = engine.query_jsonl(&request.sql).await.map_err(api_error)?;
    Ok(([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response())
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
    let (engine, _) = directory_query_engine(&state).await.map_err(api_error)?;
    let body = engine.query_jsonl(&request.sql).await.map_err(api_error)?;
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

fn database_name(storage: &str) -> String {
    let name = std::path::Path::new(storage)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data");
    let normalized = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    if normalized.is_empty()
        || normalized
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
    {
        "data".into()
    } else {
        normalized
    }
}

async fn directory_query_engine(
    state: &AppState,
) -> anyhow::Result<(ChronicleQueryEngine, String)> {
    let database = database_name(&state.storage);
    let (engine, suffixes, file_backed) = if let Some(dataset) = &state.dataset {
        let (engine, suffixes) = dataset.query_engine()?;
        (engine, suffixes, true)
    } else {
        (
            ChronicleQueryEngine::open_lance(state.storage.as_str()).await?,
            vec![String::new()],
            false,
        )
    };
    let context = engine.context();
    context
        .sql(&format!("CREATE SCHEMA IF NOT EXISTS {database}"))
        .await?
        .collect()
        .await?;
    for table in ["runs", "steps", "tool_calls"] {
        let selects = suffixes
            .iter()
            .map(|suffix| {
                if file_backed {
                    format!("SELECT * FROM {table}{suffix}")
                } else {
                    format!("SELECT '{table}.lance' AS _file_, * FROM {table}{suffix}")
                }
            })
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        context
            .sql(&format!("CREATE VIEW {database}.{table} AS {selects}"))
            .await?
            .collect()
            .await?;
    }
    context
        .sql(&format!(
            "CREATE VIEW {database}.trajectories AS \
             SELECT r.*, \
                    (SELECT COUNT(*) FROM {database}.steps s \
                      WHERE s._file_ = r._file_ AND s.run_id = r.run_id) AS step_count, \
                    (SELECT array_agg(s.step_id ORDER BY s.step_id) FROM {database}.steps s \
                      WHERE s._file_ = r._file_ AND s.run_id = r.run_id) AS step_ids, \
                    (SELECT array_agg(s.source ORDER BY s.step_id) FROM {database}.steps s \
                      WHERE s._file_ = r._file_ AND s.run_id = r.run_id) AS step_sources, \
                    (SELECT array_agg(s.message_json ORDER BY s.step_id) FROM {database}.steps s \
                      WHERE s._file_ = r._file_ AND s.run_id = r.run_id) AS messages_json, \
                    (SELECT COUNT(*) FROM {database}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.run_id = r.run_id) AS tool_call_count, \
                    (SELECT array_agg(t.function_name ORDER BY t.step_id, t.call_index) FROM {database}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.run_id = r.run_id) AS tool_names, \
                    (SELECT array_agg(t.arguments_json ORDER BY t.step_id, t.call_index) FROM {database}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.run_id = r.run_id) AS tool_arguments_json, \
                    (SELECT array_agg(t.results_json ORDER BY t.step_id, t.call_index) FROM {database}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.run_id = r.run_id) AS tool_results_json \
             FROM {database}.runs r"
        ))
        .await?
        .collect()
        .await?;
    Ok((engine, database))
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
        std::fs::write(
            root.join("gateway.json"),
            serde_json::to_vec(&json!([{
                "id":"event-1",
                "session_id":"json-session",
                "step_id":1,
                "agent_model":"model-json",
                "job_id":"json-job",
                "messages":[{"role":"user","content":"hello"}],
                "response":{"role":"assistant","content":"world"}
            }]))
            .unwrap(),
        )
        .unwrap();
        root
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
    fn empty_storage_errors_are_safe_to_render_as_an_empty_run_list() {
        assert!(is_empty_storage_error(
            "trajectory stats: no trajectory sessions under /tmp/store/"
        ));
        assert!(!is_empty_storage_error(
            "trajectory stats: path not found or not a trajectory store"
        ));
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
        assert_eq!(tables["tables"][0]["name"], "runs");
        assert_eq!(tables["tables"][1]["name"], "steps");
        assert_eq!(tables["tables"][2]["name"], "tool_calls");
        assert_eq!(tables["tables"][3]["name"], "trajectories");

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
        assert_eq!(row["step_count"], 1);
        std::fs::remove_dir_all(root).unwrap();
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
            "gateway.json/json-job/json-session"
        );
        assert_eq!(page["path_index"].as_array().unwrap().len(), 1);

        let path_filtered = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/explorer/runs?path=gateway.json%2Fjson-job&limit=10")
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

        let coordinates = "agent_id=model-json&session_id=json-session&root_session_id=json-job";
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
        assert_eq!(analysis.status(), StatusCode::OK);
        let analysis: Value =
            serde_json::from_slice(&analysis.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
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
        assert_eq!(valid.status(), StatusCode::OK);

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
        assert_eq!(saved.status(), StatusCode::OK);
        let saved: Value =
            serde_json::from_slice(&saved.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(saved[0]["score"], 88);
        assert_eq!(saved[0]["call_id"], "__story__");

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

    #[test]
    fn logical_run_paths_match_the_story_storage_hierarchy() {
        assert_eq!(
            logical_run_path("agent", "root", Some("root")),
            "agent/root"
        );
        assert_eq!(
            logical_run_path("agent", "child", Some("root")),
            "agent/root/subagents/child"
        );
        assert_eq!(logical_run_path("agent", "session", None), "agent/session");
    }
}

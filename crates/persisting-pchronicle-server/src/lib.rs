//! Local, loopback-only pChronicle browser.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use persisting_pchronicle::{
    events_to_har, events_to_otlp_json, events_to_storyline, expand_story_locations,
    list_story_read_locations, maintain_raw_events, read_judge_rows, read_revisions,
    write_judge_rows, Chronicle, ChronicleQueryEngine, EventRecord, EventsDocument, JudgeRow,
    LanceMaintenanceOptions, StoryCoords, TrajectoryReplayRequest, TrajectoryStatsRequest,
    TrajectoryStorageFormat,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::StreamExt;

#[derive(Clone)]
struct AppState {
    storage: Arc<String>,
    chronicle: Chronicle,
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

#[derive(Debug, Serialize)]
struct RunSummary {
    agent_id: String,
    session_id: String,
    root_session_id: Option<String>,
    row_count: usize,
    duplicate_event_ids: usize,
    status: String,
}

#[derive(Debug, Deserialize)]
struct SessionQuery {
    agent_id: String,
    session_id: String,
    root_session_id: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}

fn coords(storage: &str, query: &SessionQuery) -> StoryCoords {
    StoryCoords::new(
        storage,
        &query.agent_id,
        &query.session_id,
        query.root_session_id.clone(),
    )
}

pub fn router(storage: impl Into<String>) -> Router {
    let state = AppState {
        storage: Arc::new(storage.into()),
        chronicle: Chronicle,
    };
    Router::new()
        .route("/", get(index))
        .route("/api/v1/health", get(health))
        .route("/api/v1/runs", get(runs))
        .route("/api/v1/events", get(events))
        .route("/api/v1/stream", get(stream))
        .route("/api/v1/storyline", get(storyline))
        .route("/api/v1/export/har", get(export_har))
        .route("/api/v1/export/otlp", get(export_otlp))
        .route("/api/v1/judgments", get(judgments).post(write_judgments))
        .route("/api/v1/revisions", get(revisions))
        .route("/api/v1/maintain", post(maintain))
        .route("/api/v1/query", post(query_sql))
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

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"status":"ok","storage":state.storage.as_str()}))
}

async fn runs(State(state): State<AppState>) -> Result<Json<Vec<RunSummary>>, ApiError> {
    let roots = list_story_read_locations(state.storage.as_ref().clone(), None, None, None)
        .map_err(api_error)?;
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
        out.push(RunSummary {
            agent_id: location.agent_id,
            session_id: location.session_id,
            root_session_id: location.root_session_id,
            row_count: stats.row_count,
            duplicate_event_ids: stats.duplicate_event_ids,
            status: stats.status,
        });
    }
    Ok(Json(out))
}

async fn load_events(state: &AppState, query: &SessionQuery) -> Result<Vec<EventRecord>, ApiError> {
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
    let records = load_events(&state, &query).await?;
    Ok(Json(
        json!({"snapshot":{"offset":offset,"next_offset":offset + records.len()},"records":records}),
    ))
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
    Json(request): Json<JudgmentWrite>,
) -> Result<Json<Value>, ApiError> {
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
    let dataset = write_judge_rows(&coords(&state.storage, &query), &[row])
        .await
        .map_err(api_error)?;
    Ok(Json(json!({"status":"ok","dataset":dataset})))
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
    agent_id: String,
    session_id: String,
    root_session_id: Option<String>,
    sql: String,
}

async fn query_sql(
    State(state): State<AppState>,
    Json(request): Json<SqlRequest>,
) -> Result<Response, ApiError> {
    let normalized = request.sql.trim_start().to_ascii_lowercase();
    if !(normalized.starts_with("select")
        || normalized.starts_with("with")
        || normalized.starts_with("explain"))
    {
        return Err(ApiError {
            code: "read_only_sql",
            message: "only SELECT, WITH, and EXPLAIN are allowed".into(),
        });
    }
    let query = SessionQuery {
        agent_id: request.agent_id,
        session_id: request.session_id,
        root_session_id: request.root_session_id,
        offset: None,
        limit: None,
    };
    let path = coords(&state.storage, &query)
        .lance_event_path()
        .map_err(api_error)?;
    let engine = ChronicleQueryEngine::open_events(path)
        .await
        .map_err(api_error)?;
    let body = engine.query_jsonl(&request.sql).await.map_err(api_error)?;
    Ok(([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response())
}

const INDEX_HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>pChronicle</title><style>
:root{font-family:ui-sans-serif,system-ui;color:#dce7f3;background:#081018}body{margin:0}header{padding:18px 24px;border-bottom:1px solid #243342}main{display:grid;grid-template-columns:320px 1fr;min-height:calc(100vh - 62px)}aside{border-right:1px solid #243342;padding:16px;overflow:auto}.content{padding:20px;overflow:auto}button,input,textarea{background:#111e2a;color:#dce7f3;border:1px solid #34495c;border-radius:6px;padding:8px}button{cursor:pointer}.run{display:block;width:100%;text-align:left;margin:6px 0}.muted{color:#8aa0b5}pre{white-space:pre-wrap;background:#0d1720;border:1px solid #243342;padding:12px;border-radius:8px}.tabs{display:flex;gap:8px;margin-bottom:12px}textarea{width:100%;min-height:90px;box-sizing:border-box}
</style></head><body><header><strong>pChronicle</strong> <span class="muted">wire truth · local analysis</span></header><main><aside><button onclick="loadRuns()">Refresh runs</button><div id="runs"></div></aside><section class="content"><h2 id="title">Select a run</h2><div class="tabs"><button onclick="showView('events')">Events</button><button onclick="showView('storyline')">Storyline</button><button onclick="showView('judgments')">Judgments</button><button onclick="showView('revisions')">Revisions</button><button onclick="download('har')">HAR</button><button onclick="download('otlp')">OTLP</button><button onclick="maintain()">Maintain</button></div><textarea id="sql">SELECT kind, COUNT(*) AS n FROM events GROUP BY kind ORDER BY n DESC</textarea><button onclick="runSql()">Run read-only SQL</button><pre id="output">No run selected.</pre></section></main><script>
let current=null,currentStream=null,lastRows=null;const qs=()=>new URLSearchParams(current||{}).toString();async function loadRuns(){let r=await fetch('/api/v1/runs');let xs=await r.json();runs.innerHTML=xs.map((x,i)=>`<button class="run" onclick='pick(${JSON.stringify(JSON.stringify(x))})'>${x.agent_id} / ${x.session_id}<br><span class="muted">${x.row_count} events</span></button>`).join('')}function pick(s){current=JSON.parse(s);lastRows=current.row_count;title.textContent=current.agent_id+' / '+current.session_id;if(currentStream)currentStream.close();currentStream=new EventSource('/api/v1/stream?'+qs());currentStream.addEventListener('snapshot',e=>{let x=JSON.parse(e.data);if(x.row_count!==undefined&&x.row_count!==lastRows){lastRows=x.row_count;showView('events')}});showView('events')}async function showView(view){if(!current)return;let r=await fetch('/api/v1/'+view+'?'+qs());output.textContent=JSON.stringify(await r.json(),null,2)}async function runSql(){if(!current)return;let r=await fetch('/api/v1/query',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({...current,sql:sql.value})});output.textContent=await r.text()}async function maintain(){if(!current)return;if(!confirm('Compact, index, and vacuum this event store?'))return;let r=await fetch('/api/v1/maintain',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(current)});output.textContent=JSON.stringify(await r.json(),null,2)}async function download(kind){if(!current)return;let r=await fetch('/api/v1/export/'+kind+'?'+qs());let b=await r.blob(),a=document.createElement('a');a.href=URL.createObjectURL(b);a.download=current.session_id+'.'+(kind==='har'?'har':'otlp.json');a.click()}loadRuns();
</script></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;

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
}

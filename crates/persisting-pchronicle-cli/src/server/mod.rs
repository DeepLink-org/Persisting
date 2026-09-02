//! Local, loopback-only pChronicle browser.

mod acceleration;
mod asset;
pub(crate) mod catalog;
mod explorer;
mod physical;
pub(crate) mod problem;
pub(crate) mod request_log;

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use persisting_pchronicle::analysis_compile::{
    AnalysisSpec, CompileError, CompileScope, CompiledQuery, TableSchema, compile,
};
use persisting_pchronicle::document::InputIssue;
use persisting_pchronicle::model::{EventRecord, StorylineTurn};
use persisting_pchronicle::query::ChronicleQueryEngine;
use persisting_pchronicle::search::storyline_steps_fts_available;
#[cfg(test)]
use persisting_pchronicle::storage::StoryCoords;
use persisting_pchronicle::storage::{
    CatalogErrorPolicy, CatalogEventProvenance, CatalogSnapshotOptions, CatalogStorylineKey,
    DEFAULT_DATASET_NAME, DatasetCatalogSnapshot, DatasetMount,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use acceleration::{AccelerationStatus, ServerAcceleration};
use problem::{
    ApiError, CHAIN_LIMIT, LOG_TARGET, QUERY_LOG_LIMIT, ROOT_CAUSE_LIMIT, truncate_utf8,
};
use request_log::{FtsDiagnostics, RequestId};

fn fail(request_id: &RequestId, handler: &'static str, error: anyhow::Error) -> ApiError {
    ApiError::from_anyhow(request_id.as_str(), handler, error)
}

#[cfg(test)]
use problem::BoundaryCode;

#[derive(Clone)]
struct AppState {
    config: Arc<ChronicleServerConfig>,
    catalog: Arc<tokio::sync::RwLock<Option<Arc<CatalogRuntime>>>>,
    catalog_refresh: Arc<tokio::sync::Mutex<()>>,
    catalog_refresh_interval: Duration,
    trajectory_cache: Arc<tokio::sync::RwLock<Option<(String, LoadedTrajectory)>>>,
    /// Gateway-backed Warehouses read canonical events from the latest
    /// manifest for single-trace observation, independent of projection idle.
    live_reads: bool,
    catalog_acl: Option<Arc<catalog::CatalogAcl>>,
    catalog_query_worker: bool,
}

const DEFAULT_CATALOG_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

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

    pub fn front_only() -> Self {
        Self {
            datasets: Vec::new(),
            default_dataset: None,
            catalog_options: CatalogSnapshotOptions::default(),
        }
    }
}

#[derive(Debug)]
struct CatalogRuntime {
    snapshot: Arc<DatasetCatalogSnapshot>,
    engine: Arc<ChronicleQueryEngine>,
    acceleration: ServerAcceleration,
    built_at: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    finish_routes(app_state(config))
}

fn app_state(config: ChronicleServerConfig) -> AppState {
    app_state_with_catalog_refresh_interval(config, DEFAULT_CATALOG_REFRESH_INTERVAL)
}

fn app_state_with_catalog_refresh_interval(
    config: ChronicleServerConfig,
    catalog_refresh_interval: Duration,
) -> AppState {
    AppState {
        config: Arc::new(config),
        catalog: Arc::new(tokio::sync::RwLock::new(None)),
        catalog_refresh: Arc::new(tokio::sync::Mutex::new(())),
        catalog_refresh_interval,
        trajectory_cache: Arc::new(tokio::sync::RwLock::new(None)),
        live_reads: false,
        catalog_acl: None,
        catalog_query_worker: false,
    }
}

#[derive(Clone)]
pub(crate) struct PreparedWarehouse {
    state: AppState,
}

impl PreparedWarehouse {
    pub(crate) async fn prepare(config: ChronicleServerConfig) -> anyhow::Result<Self> {
        let warehouse = Self {
            state: app_state(config),
        };
        warehouse.install_initial_runtime().await?;
        Ok(warehouse)
    }

    pub(crate) async fn prepare_live(config: ChronicleServerConfig) -> anyhow::Result<Self> {
        let mut state = app_state(config);
        state.live_reads = true;
        let warehouse = Self { state };
        warehouse.install_initial_runtime().await?;
        Ok(warehouse)
    }

    pub(crate) async fn prepare_catalog_front(acl: catalog::CatalogAcl) -> anyhow::Result<Self> {
        let mut state = app_state(ChronicleServerConfig::front_only());
        state.catalog_acl = Some(Arc::new(acl));
        Ok(Self { state })
    }

    pub(crate) async fn prepare_query_worker(
        config: ChronicleServerConfig,
    ) -> anyhow::Result<Self> {
        let mut state = app_state(config);
        state.catalog_query_worker = true;
        let warehouse = Self { state };
        warehouse.install_initial_runtime().await?;
        Ok(warehouse)
    }

    async fn install_initial_runtime(&self) -> anyhow::Result<()> {
        if self.state.config.datasets.is_empty() {
            return Ok(());
        }
        let runtime = build_catalog_runtime(&self.state.config).await?;
        self.install_catalog_runtime(runtime).await;
        Ok(())
    }

    async fn install_catalog_runtime(&self, runtime: Arc<CatalogRuntime>) -> String {
        let snapshot_id = runtime.snapshot.snapshot_id().to_string();
        *self.state.catalog.write().await = Some(runtime);
        *self.state.trajectory_cache.write().await = None;
        snapshot_id
    }

    async fn refresh_runtime(&self) -> anyhow::Result<Arc<CatalogRuntime>> {
        let runtime = build_catalog_runtime(&self.state.config).await?;
        self.install_catalog_runtime(runtime.clone()).await;
        Ok(runtime)
    }

    pub(crate) async fn refresh_catalog(&self) -> anyhow::Result<String> {
        let runtime = self.refresh_runtime().await?;
        Ok(runtime.snapshot.snapshot_id().to_string())
    }

    pub(crate) fn router(&self) -> Router {
        finish_routes(self.state.clone())
    }

    pub(crate) fn dataset_names(&self) -> Vec<String> {
        self.state
            .config
            .datasets
            .iter()
            .map(|dataset| dataset.name.clone())
            .collect()
    }

    pub(crate) async fn current_snapshot_id(&self) -> Option<String> {
        self.state
            .catalog
            .read()
            .await
            .as_ref()
            .map(|runtime| runtime.snapshot.snapshot_id().to_string())
    }
}

fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(warehouse_health))
        .route("/runs", get(runs))
        .route("/explorer/runs", get(explorer_runs))
        .route("/explorer/tree", get(explorer_tree))
        .route("/explorer/run", get(explorer_run))
        .route("/explorer/turns", get(explorer_turns))
        .route("/explorer/turn", get(explorer_turn))
        .route("/events", get(events))
        .route("/storyline", get(storyline))
        .route("/trajectory-view", get(trajectory_view))
        .route("/catalog", get(catalog).post(refresh_catalog))
        .route("/physical/sources", get(physical::sources))
        .route("/physical/layout", get(physical::layout))
        .route("/physical/file", get(physical::file))
        .route("/physical/page", get(physical::page))
        .route("/query/tables", get(query_tables))
        .route("/query/evidence", post(query_evidence))
        .route("/analysis/compile", post(compile_analysis))
        .route("/catalog/datasets", get(catalog::list_datasets))
        .route("/catalog/datasets/{name}", get(catalog::get_dataset))
}

fn read_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .nest("/api", api_routes())
        .nest("/api/v1", api_routes())
        .fallback(asset_fallback)
}

fn finish_routes(state: AppState) -> Router {
    read_routes()
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            catalog::catalog_data_plane_layer,
        ))
        .layer(axum::middleware::from_fn(
            request_log::warehouse_request_layer,
        ))
        .with_state(state)
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

pub(crate) async fn serve_prepared_warehouse_with_listener_and_shutdown(
    warehouse: PreparedWarehouse,
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
    axum::serve(listener, warehouse.router())
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
        built_at: Instant::now(),
    }))
}

async fn current_catalog(
    state: &AppState,
    request_id: &RequestId,
) -> Result<Arc<CatalogRuntime>, ApiError> {
    if let Some(runtime) = state.catalog.read().await.as_ref() {
        return Ok(Arc::clone(runtime));
    }
    let _refresh = state.catalog_refresh.lock().await;
    if let Some(runtime) = state.catalog.read().await.as_ref() {
        return Ok(Arc::clone(runtime));
    }
    let runtime = build_catalog_runtime(&state.config)
        .await
        .map_err(|error| fail(request_id, "current_catalog", error))?;
    *state.catalog.write().await = Some(Arc::clone(&runtime));
    Ok(runtime)
}

async fn current_catalog_for_runs(
    state: &AppState,
    request_id: &RequestId,
) -> Result<Arc<CatalogRuntime>, ApiError> {
    let runtime = current_catalog(state, request_id).await?;
    if runtime.built_at.elapsed() < state.catalog_refresh_interval {
        return Ok(runtime);
    }
    let _refresh = state.catalog_refresh.lock().await;
    let current = state.catalog.read().await.clone().unwrap_or(runtime);
    if current.built_at.elapsed() < state.catalog_refresh_interval {
        return Ok(current);
    }
    match build_catalog_runtime(&state.config).await {
        Ok(runtime) => {
            *state.catalog.write().await = Some(Arc::clone(&runtime));
            *state.trajectory_cache.write().await = None;
            Ok(runtime)
        }
        Err(error) => {
            tracing::warn!(
                target: LOG_TARGET,
                root_cause = %truncate_utf8(&error.root_cause().to_string(), ROOT_CAUSE_LIMIT),
                chain = %truncate_utf8(&format!("{error:#}"), CHAIN_LIMIT),
                "automatic Catalog refresh failed; retaining the last valid snapshot"
            );
            Ok(current)
        }
    }
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

async fn catalog(
    State(state): State<AppState>,
    request_id: RequestId,
) -> Result<Json<CatalogResponse>, ApiError> {
    let runtime = current_catalog(&state, &request_id).await?;
    Ok(Json(catalog_response(&state, &runtime)))
}

async fn refresh_catalog(
    State(state): State<AppState>,
    request_id: RequestId,
) -> Result<Json<CatalogResponse>, ApiError> {
    let warehouse = PreparedWarehouse {
        state: state.clone(),
    };
    let runtime = warehouse
        .refresh_runtime()
        .await
        .map_err(|error| fail(&request_id, "refresh_catalog", error))?;
    Ok(Json(catalog_response(&state, &runtime)))
}

async fn runs(
    State(state): State<AppState>,
    request_id: RequestId,
) -> Result<Json<Vec<RunSummary>>, ApiError> {
    Ok(Json(load_run_summaries(&state, None, &request_id).await?))
}

async fn load_run_summaries(
    state: &AppState,
    dataset: Option<&str>,
    request_id: &RequestId,
) -> Result<Vec<RunSummary>, ApiError> {
    let runtime = current_catalog_for_runs(state, request_id).await?;
    let summaries = match dataset {
        Some(dataset) => {
            runtime
                .acceleration
                .run_summaries_for_dataset(&runtime.snapshot, &runtime.engine, dataset)
                .await
        }
        None => {
            runtime
                .acceleration
                .run_summaries(&runtime.snapshot, &runtime.engine)
                .await
        }
    };
    summaries
        .map(|summaries| summaries.as_ref().clone())
        .map_err(|error| fail(request_id, "load_run_summaries", error))
}

fn api_query<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, ApiError> {
    query
        .map(|Query(query)| query)
        .map_err(|_| ApiError::invalid_request("query parameters must be valid"))
}

const EXPLORER_RUN_MATCH_IDENTITY_MAX_ROWS: u64 = 50_000;
const EXPLORER_RUN_MATCH_PREVIEW_LIMIT: u64 = 512;

fn explorer_run_identity_sql(dataset: &str, table: &str, predicate: &str) -> String {
    format!(
        "SELECT DISTINCT _file_ AS source_path, document_id FROM {dataset}.{table} WHERE ({predicate})"
    )
}

fn explorer_run_preview_sql(
    dataset: &str,
    table: &str,
    select: &str,
    predicate: &str,
    limit: u64,
) -> String {
    format!("SELECT {select} FROM {dataset}.{table} WHERE ({predicate}) LIMIT {limit}")
}

async fn explorer_query_jsonl(
    engine: &ChronicleQueryEngine,
    sql: &str,
    max_rows: u64,
    request_id: &RequestId,
) -> Result<String, ApiError> {
    let mut buffer = Vec::new();
    engine
        .write_query_jsonl_with_max_rows(sql, &mut buffer, Some(max_rows))
        .await
        .map_err(|error| fail(request_id, "explorer_query_jsonl", error))?;
    String::from_utf8(buffer).map_err(|error| {
        fail(
            request_id,
            "explorer_query_jsonl",
            anyhow::anyhow!("explorer search JSONL is not UTF-8: {error}"),
        )
    })
}

async fn explorer_runs(
    State(state): State<AppState>,
    request_id: RequestId,
    fts: FtsDiagnostics,
    query: Result<Query<explorer::ExplorerRunsQuery>, QueryRejection>,
) -> Result<Json<explorer::RunExplorerPage>, ApiError> {
    let query = api_query(query)?;
    let dataset_filter = query
        .dataset
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "all");
    let summaries = load_run_summaries(&state, dataset_filter, &request_id).await?;
    let (fts_matches, fts_available, search_mode) = if query
        .q
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        let runtime = current_catalog(&state, &request_id).await?;
        let raw = query.q.as_deref().unwrap_or_default().trim();
        let expression = crate::combine_match_expressions(&[raw.to_owned()])
            .map_err(|error| ApiError::invalid_request(error.to_string()))?
            .ok_or_else(|| ApiError::invalid_request("search query must not be empty"))?;
        let (predicate, fts_available, fts_errors) = crate::find_expression_predicate_for_dataset(
            &runtime.snapshot,
            &expression,
            None,
            dataset_filter,
        )
        .await
        .map_err(|error| fail(&request_id, "explorer_runs", error))?;
        fts.extend(fts_errors);
        let predicate = predicate.ok_or_else(|| {
            fail(
                &request_id,
                "explorer_runs",
                anyhow::anyhow!("run search expression did not produce a predicate"),
            )
        })?;
        let table = if expression.has_text() || expression.has_step_json() {
            "steps"
        } else {
            "runs"
        };
        let mut matches = BTreeMap::new();
        for dataset in runtime.snapshot.datasets() {
            if dataset_filter.is_some_and(|filter| dataset.mount.name != filter) {
                continue;
            }
            let select = if table == "steps" {
                // Keep all searchable step fields available to the preview
                // selector.  A COALESCE expression would hide a hit in (for
                // example) reasoning_content behind a non-empty message.
                "_file_ AS source_path, document_id, message_value, reasoning_content, observation, prompt, model_name"
            } else {
                "_file_ AS source_path, document_id, task, prompt, notes, agent_name, agent_model_name"
            };
            let identity_sql = explorer_run_identity_sql(&dataset.mount.name, table, &predicate);
            let identity_jsonl = explorer_query_jsonl(
                &runtime.engine,
                &identity_sql,
                EXPLORER_RUN_MATCH_IDENTITY_MAX_ROWS,
                &request_id,
            )
            .await?;
            for line in identity_jsonl
                .lines()
                .filter(|line| !line.trim().is_empty())
            {
                let row: Value = serde_json::from_str(line).map_err(|error| {
                    fail(
                        &request_id,
                        "explorer_runs",
                        anyhow::anyhow!("decode run search result: {error}"),
                    )
                })?;
                let Some(file) = row.get("source_path").and_then(Value::as_str) else {
                    continue;
                };
                let Some(document_id) = row.get("document_id").and_then(Value::as_str) else {
                    continue;
                };
                let identity = format!("{}\u{1f}{}\u{1f}{}", dataset.mount.name, file, document_id);
                matches.entry(identity).or_insert_with(String::new);
            }
            let preview_sql = explorer_run_preview_sql(
                &dataset.mount.name,
                table,
                select,
                &predicate,
                EXPLORER_RUN_MATCH_PREVIEW_LIMIT,
            );
            let preview_jsonl = explorer_query_jsonl(
                &runtime.engine,
                &preview_sql,
                EXPLORER_RUN_MATCH_PREVIEW_LIMIT,
                &request_id,
            )
            .await?;
            for line in preview_jsonl.lines().filter(|line| !line.trim().is_empty()) {
                let row: Value = serde_json::from_str(line).map_err(|error| {
                    fail(
                        &request_id,
                        "explorer_runs",
                        anyhow::anyhow!("decode run search preview: {error}"),
                    )
                })?;
                let Some(file) = row.get("source_path").and_then(Value::as_str) else {
                    continue;
                };
                let Some(document_id) = row.get("document_id").and_then(Value::as_str) else {
                    continue;
                };
                let identity = format!("{}\u{1f}{}\u{1f}{}", dataset.mount.name, file, document_id);
                if !matches.contains_key(&identity) {
                    continue;
                }
                let preview = search_preview_from_row(&row, raw, table);
                if let Some(existing) = matches.get_mut(&identity)
                    && existing.is_empty()
                {
                    *existing = preview;
                }
            }
        }
        let mode = if expression.has_text() && expression.has_json() {
            "fts+json"
        } else if expression.has_text() {
            "fts"
        } else {
            "json"
        };
        (matches, fts_available, mode)
    } else {
        // The initial runs page has no query yet, but still advertises whether
        // its mounted Storyline sources support indexed full-text search.
        let runtime = current_catalog(&state, &request_id).await?;
        let mut fts_available = false;
        for run in &summaries {
            let Some(paths) = runtime
                .snapshot
                .storyline_table_paths(&run.dataset, &run.file)
                .map_err(|error| fail(&request_id, "explorer_runs", error))?
            else {
                continue;
            };
            match storyline_steps_fts_available(&paths).await {
                Ok(true) => {
                    fts_available = true;
                    break;
                }
                Ok(false) => {}
                Err(error) => fts.push(format!(
                    "could not determine runs FTS availability: {error:#}"
                )),
            }
        }
        (BTreeMap::new(), fts_available, "none")
    };
    Ok(Json(explorer::run_page_with_fts(
        summaries,
        &query,
        &fts_matches,
        explorer::RunSearchStatus {
            fts_available,
            mode: search_mode,
            tokenizer: fts_available.then_some("jieba"),
        },
    )))
}

fn search_preview_text(raw: &str) -> String {
    // The API deliberately returns the complete normalized field. The Web
    // client owns the viewport-sized excerpt so it can guarantee that the
    // matched term remains visible and highlighted.
    crate::find_preview_text(raw)
}

fn search_preview_from_row(row: &Value, query: &str, table: &str) -> String {
    let columns: &[&str] = if table == "steps" {
        &[
            "message_value",
            "reasoning_content",
            "observation",
            "prompt",
            "model_name",
        ]
    } else {
        &["task", "prompt", "notes", "agent_name", "agent_model_name"]
    };
    let needle = preview_needle(query).to_ascii_lowercase();
    let mut fallback = None;
    for column in columns {
        let Some(value) = row.get(*column).and_then(search_preview_raw_value) else {
            continue;
        };
        let preview = search_preview_text(&value);
        if fallback.is_none() && !preview.is_empty() {
            fallback = Some(preview.clone());
        }
        if !needle.is_empty()
            && crate::find_preview_text(&value)
                .to_ascii_lowercase()
                .contains(&needle)
        {
            return preview;
        }
    }
    fallback.unwrap_or_default()
}

fn search_preview_raw_value(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(value) if value.trim().is_empty() => None,
        Value::String(value) => Some(value.clone()),
        value => serde_json::to_string(value).ok(),
    }
}

fn preview_needle(query: &str) -> String {
    let query = query.trim();
    let candidate = if let Some(hash) = query.find('#') {
        query[hash..]
            .find('(')
            .and_then(|offset| {
                let start = hash + offset + 1;
                query[start..]
                    .find(')')
                    .map(|end| &query[start..start + end])
            })
            .unwrap_or(query)
    } else {
        query
    };
    candidate
        .trim()
        .trim_matches(|character| character == '"' || character == '\'')
        .to_owned()
}

async fn explorer_tree(
    State(state): State<AppState>,
    request_id: RequestId,
    query: Result<Query<explorer::ExplorerTreeQuery>, QueryRejection>,
) -> Result<Json<explorer::CatalogTree>, ApiError> {
    let query = api_query(query)?;
    // Tree navigation is a read of the already-installed catalog. Do not run
    // the five-second automatic catalog refresh on every folder interaction;
    // the runs endpoint remains the freshness boundary for live summaries.
    let runtime = current_catalog(&state, &request_id).await?;
    let summaries = runtime
        .acceleration
        .run_summaries(&runtime.snapshot, &runtime.engine)
        .await
        .map(|summaries| summaries.as_ref().clone())
        .map_err(|error| fail(&request_id, "explorer_tree", error))?;
    let dataset = query
        .dataset
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let prefix = query.prefix.as_deref().unwrap_or("");
    let mut tree = explorer::catalog_tree(&summaries, dataset, prefix, explorer::MAX_TREE_CHILDREN);
    if let Some(name) = tree.dataset.clone() {
        if tree.prefix.is_empty()
            && let Some(dataset) = runtime.snapshot.dataset(&name)
        {
            tree.ready_sources = Some(dataset.ready_source_count());
            tree.error_sources = Some(dataset.error_source_count());
        }
        let (duration_ms, total_tokens) = tree_prefix_metrics(&runtime, &name, &tree.prefix).await;
        tree.duration_ms = duration_ms;
        tree.total_tokens = total_tokens;
    }
    Ok(Json(tree))
}

fn sql_ident(name: &str) -> Option<&str> {
    let mut chars = name.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic() || first == '_')
        .then_some(name)
        .filter(|_| chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_'))
}

async fn tree_prefix_metrics(
    runtime: &CatalogRuntime,
    dataset: &str,
    prefix: &str,
) -> (Option<i64>, Option<u64>) {
    let Some(ident) = sql_ident(dataset) else {
        return (None, None);
    };
    let file_clause = if prefix.is_empty() {
        String::new()
    } else {
        let escaped = prefix.replace('\'', "''");
        format!(" WHERE _file_ = '{escaped}' OR _file_ LIKE '{escaped}/%'")
    };
    let sql = format!(
        "SELECT MIN(timestamp) AS start_ts, MAX(timestamp) AS end_ts FROM {ident}.steps{file_clause}"
    );
    let mut buffer = Vec::new();
    let write = tokio::time::timeout(
        Duration::from_secs(3),
        runtime
            .engine
            .write_query_jsonl_with_max_rows(&sql, &mut buffer, Some(1)),
    )
    .await;
    let Ok(Ok(())) = write else {
        return (None, None);
    };
    let line = String::from_utf8(buffer).unwrap_or_default();
    let line = line.lines().find(|line| !line.trim().is_empty());
    let Some(Ok(row)) = line.map(serde_json::from_str::<Value>) else {
        return (None, None);
    };
    (
        timestamp_span_ms(row.get("start_ts"), row.get("end_ts")),
        row.get("total_tokens").and_then(Value::as_u64),
    )
}

fn timestamp_span_ms(start: Option<&Value>, end: Option<&Value>) -> Option<i64> {
    let start = json_timestamp_ms(start?)?;
    let end = json_timestamp_ms(end?)?;
    (end >= start).then_some(end - start)
}

fn json_timestamp_ms(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|value| value as i64)),
        Value::String(text) if !text.is_empty() => text.parse().ok(),
        _ => None,
    }
}

async fn resolve_run_summary(
    state: &AppState,
    query: &SessionQuery,
    request_id: &RequestId,
) -> Result<RunSummary, ApiError> {
    let dataset_filter = query
        .dataset
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "all");
    let mut matches = load_run_summaries(state, dataset_filter, request_id)
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
    if matches.len() > 1
        && let Some(root) = &query.root_session_id
    {
        matches.retain(|run| run.root_session_id.as_ref() == Some(root));
    }
    if matches.is_empty() {
        return Err(ApiError::not_found("run was not found"));
    }
    if matches.len() > 1 {
        return Err(ApiError::conflict(
            "run selector is ambiguous; include dataset, source file, and session_id",
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

#[cfg(test)]
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

#[derive(Clone)]
struct LoadedEventView {
    provenance: CatalogEventProvenance,
    records: Vec<EventRecord>,
}

async fn load_events(
    state: &AppState,
    query: &SessionQuery,
    request_id: &RequestId,
) -> Result<LoadedEventView, ApiError> {
    let run = resolve_run_summary(state, query, request_id).await?;
    let runtime = current_catalog(state, request_id).await?;
    let key = catalog_storyline_key(&run);
    let document = if state.live_reads {
        runtime.snapshot.load_live_events(&key).await
    } else {
        runtime.snapshot.load_events(&key).await
    }
    .map_err(|error| fail(request_id, "load_events", error))?
    .ok_or_else(|| ApiError::not_found("run was not found"))?;
    let offset = query
        .offset
        .unwrap_or(0)
        .min(document.document.events.len());
    let end = query
        .limit
        .map(|limit| {
            offset
                .saturating_add(limit)
                .min(document.document.events.len())
        })
        .unwrap_or(document.document.events.len());
    Ok(LoadedEventView {
        provenance: document.provenance,
        records: document.document.events[offset..end].to_vec(),
    })
}

async fn events(
    State(state): State<AppState>,
    request_id: RequestId,
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
    let event_view = load_events(&state, &full_query, &request_id).await?;
    let total = event_view.records.len();
    let start = offset.min(total);
    let end = start.saturating_add(requested_limit).min(total);
    let records = event_view.records[start..end].to_vec();
    let next_offset = offset + records.len();
    Ok(Json(json!({
        "provenance": {
            "kind": event_view.provenance,
            "transform": event_view.provenance.transform()
        },
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
    request_id: RequestId,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let query = api_query(query)?;
    let run = resolve_run_summary(&state, &query, &request_id).await?;
    let runtime = current_catalog(&state, &request_id).await?;
    let key = catalog_storyline_key(&run);
    let document = if state.live_reads {
        runtime.snapshot.load_live_storyline(&key).await
    } else {
        runtime.snapshot.load_storyline(&key).await
    }
    .map_err(|error| fail(&request_id, "storyline", error))?
    .ok_or_else(|| ApiError::not_found("run was not found"))?;
    Ok(Json(
        serde_json::to_value(document)
            .map_err(anyhow::Error::from)
            .map_err(|error| fail(&request_id, "storyline", error))?,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<Value>,
}

#[derive(Debug, Serialize)]
struct TrajectoryView {
    run: RunSummary,
    event_provenance: CatalogEventProvenance,
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
        result: map.get("result").or_else(|| map.get("output")).cloned(),
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
            ) && let Some(call) = parse_wire_tool_call(value)
            {
                out.push(call);
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
    event_provenance: CatalogEventProvenance,
    records: Vec<EventRecord>,
    turns: Vec<TrajectoryTurnView>,
}

async fn load_trajectory(
    state: &AppState,
    query: &SessionQuery,
    request_id: &RequestId,
) -> Result<LoadedTrajectory, ApiError> {
    let run = resolve_run_summary(state, query, request_id).await?;
    let runtime = current_catalog(state, request_id).await?;
    let cache_key = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}",
        runtime.snapshot.snapshot_id(),
        run.dataset,
        run.file,
        run.session_id
    );
    if !state.live_reads
        && let Some((_, loaded)) = state
            .trajectory_cache
            .read()
            .await
            .as_ref()
            .filter(|(key, _)| key == &cache_key)
    {
        return Ok(loaded.clone());
    }
    let key = catalog_storyline_key(&run);
    let bundle = if state.live_reads {
        runtime.snapshot.load_live_trajectory_bundle(&key).await
    } else {
        runtime.snapshot.load_trajectory_bundle(&key).await
    }
    .map_err(|error| fail(request_id, "load_trajectory", error))?
    .ok_or_else(|| ApiError::not_found("run was not found"))?;
    let event_provenance = bundle.event_view.provenance;
    let records = bundle.event_view.document.events;
    let document = bundle.storyline;
    // ACTF step records carry the first user input at document level when it
    // is the baseline prompt. Preserve it on the first turn so Explorer can
    // render the user side of the conversation without changing storage.
    let document_prompt = document.prompt.clone();
    let mut by_call = BTreeMap::<String, Vec<u64>>::new();
    for event in &records {
        if let Some(call_id) = event.call_id.as_ref().filter(|id| !id.is_empty()) {
            by_call.entry(call_id.clone()).or_default().push(event.seq);
        }
    }
    let turns = document
        .turns
        .into_iter()
        .enumerate()
        .map(|(turn_index, mut turn)| {
            if turn_index == 0 && turn.source == "agent" && turn.prompt.is_none() {
                turn.prompt = document_prompt.clone();
            }
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
            // Tool outputs are canonicalized on the Storyline tool call, not
            // necessarily on the event payload that supplied the call. Carry
            // that result onto the wire call so AgenticMD can render it next
            // to the matching command even when no observation envelope
            // exists.
            if let Some(native_calls) = turn.tool_calls.as_ref() {
                for wire_call in &mut wire_tool_calls {
                    if wire_call.result.is_none() {
                        wire_call.result = wire_call.id.as_deref().and_then(|id| {
                            native_calls
                                .iter()
                                .find(|call| call.tool_call_id == id)
                                .and_then(|call| call.result.clone())
                        });
                    }
                }
            }
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
        event_provenance,
        records,
        turns,
    };
    if !state.live_reads {
        *state.trajectory_cache.write().await = Some((cache_key, loaded.clone()));
    }
    Ok(loaded)
}

async fn trajectory_view(
    State(state): State<AppState>,
    request_id: RequestId,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<TrajectoryView>, ApiError> {
    let query = api_query(query)?;
    let loaded = load_trajectory(&state, &query, &request_id).await?;
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
        event_provenance: loaded.event_provenance,
        event_kind_counts,
        tool_call_count,
        turns: loaded.turns,
    }))
}

async fn explorer_run(
    State(state): State<AppState>,
    request_id: RequestId,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<explorer::RunAnalysis>, ApiError> {
    let query = api_query(query)?;
    let loaded = load_trajectory(&state, &query, &request_id).await?;
    Ok(Json(explorer::analyze(
        loaded.run,
        &loaded.turns,
        &loaded.records,
        loaded.event_provenance,
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
    request_id: RequestId,
    fts: FtsDiagnostics,
    query: Result<Query<TurnsQuery>, QueryRejection>,
) -> Result<Json<explorer::TurnExplorerPage>, ApiError> {
    let query = api_query(query)?;
    let session = query.session();
    let loaded = load_trajectory(&state, &session, &request_id).await?;
    let runtime = current_catalog(&state, &request_id).await?;
    let paths = runtime
        .snapshot
        .storyline_table_paths(&loaded.run.dataset, &loaded.run.file)
        .map_err(|error| fail(&request_id, "explorer_turns", error))?;
    let mut fts_available = if let Some(paths) = paths.as_ref() {
        match storyline_steps_fts_available(paths).await {
            Ok(available) => available,
            Err(error) => {
                fts.push(format!("{error:#}"));
                false
            }
        }
    } else {
        false
    };
    let mut search_mode = if query
        .q
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        "memory"
    } else {
        "none"
    };
    let (turns, search_query) = if let Some(needle) = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let expression = crate::combine_match_expressions(&[needle.to_owned()])
            .map_err(|error| ApiError::invalid_request(error.to_string()))?
            .ok_or_else(|| ApiError::invalid_request("search query must not be empty"))?;
        let runtime = current_catalog(&state, &request_id).await?;
        let (predicate, available, fts_errors) = crate::find_expression_predicate_for_dataset(
            &runtime.snapshot,
            &expression,
            Some(&loaded.run.file),
            Some(&loaded.run.dataset),
        )
        .await
        .map_err(|error| fail(&request_id, "explorer_turns", error))?;
        fts.extend(fts_errors);
        fts_available = fts_available || available;
        let turns = if expression.has_text() || expression.has_step_json() {
            let predicate = predicate.ok_or_else(|| {
                fail(
                    &request_id,
                    "explorer_turns",
                    anyhow::anyhow!("turn search expression did not produce a predicate"),
                )
            })?;
            let sql = format!(
                "SELECT DISTINCT step_id FROM {}.steps WHERE _file_ = {} AND document_id = {} AND session_id = {} AND ({predicate})",
                loaded.run.dataset,
                crate::sql_string(&loaded.run.file),
                crate::sql_string(&loaded.run.document_id),
                crate::sql_string(&loaded.run.session_id),
            );
            let jsonl = runtime
                .engine
                .query_jsonl(&sql)
                .await
                .map_err(|error| fail(&request_id, "explorer_turns", error))?;
            let step_ids = jsonl
                .lines()
                .filter(|line| !line.trim().is_empty())
                .filter_map(|line| {
                    serde_json::from_str::<Value>(line)
                        .ok()
                        .and_then(|row| row.get("step_id").and_then(Value::as_i64))
                })
                .collect::<BTreeSet<_>>();
            search_mode = if expression.has_text() && expression.has_json() {
                "fts+json"
            } else if expression.has_text() {
                "fts"
            } else {
                "json"
            };
            loaded
                .turns
                .iter()
                .filter(|item| step_ids.contains(&item.turn.id))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            // Run-level JSON predicates have no step identity to display in
            // this view. Keep the detail search scoped to Step expressions,
            // matching the CLI find scope instead of applying an ad-hoc
            // in-memory text filter.
            Vec::new()
        };
        (turns, None)
    } else {
        (loaded.turns.clone(), query.q.as_deref())
    };
    Ok(Json(explorer::turn_page_with_search(
        &turns,
        &loaded.records,
        search_query,
        query.source.as_deref(),
        query.offset.unwrap_or(0),
        query.limit.unwrap_or(100),
        explorer::TurnSearchStatus {
            fts_available,
            mode: search_mode,
            tokenizer: fts_available.then_some("jieba"),
        },
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
    request_id: RequestId,
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
    let loaded = load_trajectory(&state, &session, &request_id).await?;
    let item = loaded
        .turns
        .iter()
        .find(|item| item.turn.id == query.turn_id)
        .ok_or_else(|| ApiError::not_found(format!("turn {} was not found", query.turn_id)))?;
    Ok(Json(explorer::turn_detail(
        item,
        &loaded.records,
        loaded.event_provenance,
    )))
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
    kind: &'static str,
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
            "Optional grouping identifier; the session ID identifies the stored run record",
        ),
        field(
            "run_id_explicit",
            "BOOLEAN",
            "Whether run_id came from source data",
        ),
        field(
            "session_id",
            "TEXT",
            "Run/session identifier within one source file",
        ),
        field("agent_id", "TEXT", "Agent identifier"),
        field("agent_name", "TEXT?", "Agent display name"),
        field("agent_version", "TEXT?", "Agent version"),
        field("agent_model_name", "TEXT?", "Model used by the agent"),
        field(
            "agent_tool_definitions",
            "JSON?",
            "Declared tool definitions",
        ),
        field("agent_extra", "JSON?", "Source-specific agent metadata"),
        field("parent", "TEXT?", "Parent run reference"),
        field("child_session_ids", "JSON?", "Child session identifiers"),
        field("notes", "TEXT?", "Run notes"),
        field("final_metrics", "JSON?", "Final evaluation metrics"),
        field(
            "continued_trajectory_ref",
            "TEXT?",
            "Continuation reference",
        ),
        field("extra", "JSON?", "Source-specific metadata"),
        field(
            "finished_at",
            "TIMESTAMP(NANOSECOND, UTC)?",
            "UTC completion timestamp",
        ),
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
        field("session_id", "TEXT", "Owning run/session identifier"),
        field("step_id", "BIGINT", "Ordered step number"),
        field("kind", "TEXT?", "Captured step kind"),
        field("effective_kind", "TEXT", "Normalized step kind"),
        field(
            "timestamp",
            "TIMESTAMP(NANOSECOND, UTC)?",
            "UTC nanosecond timestamp for ordering and range queries",
        ),
        field(
            "finished_at",
            "TIMESTAMP(NANOSECOND, UTC)?",
            "UTC completion timestamp",
        ),
        field("source", "TEXT", "user, agent, or system"),
        field(
            "message_kind",
            "ENUM",
            "Message representation: null, text, parts, or json",
        ),
        field("message_value", "JSON", "Complete normalized message value"),
        field(
            "reasoning_content",
            "TEXT?",
            "Reasoning content when present",
        ),
        field(
            "reasoning_effort_kind",
            "ENUM?",
            "Reasoning effort representation: null, text, number, or json",
        ),
        field("reasoning_effort_value", "JSON?", "Reasoning effort value"),
        field("metrics", "JSON?", "Per-step metrics"),
        field("model_name", "TEXT?", "Model for this step"),
        field("llm_call_count", "BIGINT?", "Number of model calls"),
        field(
            "is_copied_context",
            "BOOLEAN?",
            "Whether context was copied",
        ),
        field("latency", "BIGINT?", "End-to-end latency in milliseconds"),
        field("ttft", "BIGINT?", "Time to first token in milliseconds"),
        field(
            "had_observation",
            "BOOLEAN",
            "Whether an observation exists",
        ),
        field("extra", "JSON?", "Source-specific metadata"),
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
        field("session_id", "TEXT", "Owning run/session identifier"),
        field("step_id", "BIGINT", "Owning step number"),
        field("call_index", "BIGINT", "Tool-call order within the step"),
        field("tool_call_id", "TEXT", "Tool-call identifier"),
        field("function_name", "TEXT", "Normalized tool name"),
        field("arguments", "TEXT", "Complete call arguments"),
        field("result", "TEXT?", "Single tool result"),
        field("results", "TEXT", "Complete tool results"),
        field(
            "duration",
            "BIGINT?",
            "Tool execution duration in milliseconds",
        ),
        field("extra", "JSON?", "Source-specific metadata"),
    ]
}

fn trajectory_query_fields() -> Vec<QueryFieldSummary> {
    let mut fields = run_query_fields();
    fields.extend([
        field("step_count", "BIGINT", "Number of steps in the run"),
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
        field("format", "TEXT?", "Detected run data format"),
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
        field("_file_", "TEXT", "Dataset-relative recorded event source"),
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

fn format_query_fields() -> Vec<QueryFieldSummary> {
    vec![
        field("id", "TEXT", "Stable record identifier"),
        field("data", "JSON", "Original format document encoded as JSONB"),
    ]
}

async fn query_tables(
    State(state): State<AppState>,
    request_id: RequestId,
) -> Result<Json<QueryCatalog>, ApiError> {
    let runtime = current_catalog(&state, &request_id).await?;
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
                description: "One row per discovered run data source",
                kind: "table",
                grain: "source",
                fields: source_query_fields(),
            },
            QueryTableSummary {
                name: "runs",
                description: "One row per run across the complete data path",
                kind: "table",
                grain: "run",
                fields: run_query_fields(),
            },
            QueryTableSummary {
                name: "steps",
                description: "Ordered user, agent, and system steps for every run",
                kind: "table",
                grain: "step",
                fields: step_query_fields(),
            },
            QueryTableSummary {
                name: "tool_calls",
                description: "Structured tool calls joined to their run and step",
                kind: "table",
                grain: "tool call",
                fields: tool_call_query_fields(),
            },
            QueryTableSummary {
                name: "trajectories",
                description: "One complete run with ordered step and tool-call arrays",
                kind: "view",
                grain: "complete run",
                fields: trajectory_query_fields(),
            },
            QueryTableSummary {
                name: "events",
                description: "Recorded events; empty for sources without recorded events",
                kind: "table",
                grain: "event",
                fields: event_query_fields(),
            },
            QueryTableSummary {
                name: "atif",
                description: "ATIF documents exposed as id plus JSONB data",
                kind: "view",
                grain: "ATIF document",
                fields: format_query_fields(),
            },
            QueryTableSummary {
                name: "storyline",
                description: "Storyline documents exposed as id plus JSONB data",
                kind: "view",
                grain: "Storyline document",
                fields: format_query_fields(),
            },
            QueryTableSummary {
                name: "actf",
                description: "ACTF documents exposed as id plus JSONB data",
                kind: "view",
                grain: "ACTF document",
                fields: format_query_fields(),
            },
            QueryTableSummary {
                name: "openai_msg",
                description: "OpenAI message documents exposed as id plus JSONB data",
                kind: "view",
                grain: "OpenAI message",
                fields: format_query_fields(),
            },
            QueryTableSummary {
                name: "codex",
                description: "Codex documents exposed as id plus JSONB data",
                kind: "view",
                grain: "Codex document",
                fields: format_query_fields(),
            },
            QueryTableSummary {
                name: "markdown",
                description: "Markdown/AgenticMD documents exposed as id plus JSONB data",
                kind: "view",
                grain: "Markdown document",
                fields: format_query_fields(),
            },
            QueryTableSummary {
                name: "claude",
                description: "Claude Code documents exposed as id plus JSONB data",
                kind: "view",
                grain: "Claude Code document",
                fields: format_query_fields(),
            },
        ],
    }))
}

#[derive(Debug, Deserialize)]
struct CompileAnalysisRequest {
    spec: AnalysisSpec,
    snapshot_id: String,
    scope: AnalysisCompileScope,
}

#[derive(Debug, Deserialize)]
struct AnalysisCompileScope {
    #[serde(default)]
    database: String,
    #[serde(default)]
    items: Vec<AnalysisCompileScopeItem>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AnalysisCompileScopeItem {
    Dataset {
        name: String,
    },
    Root {
        dataset: String,
        file: String,
        root_session_id: String,
    },
    Run {
        run: RunSummary,
    },
}

#[derive(Debug, Serialize)]
struct CompileFailureBody {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_detail: Option<String>,
}

struct CompileHttpError {
    status: StatusCode,
    body: CompileFailureBody,
}

impl CompileHttpError {
    fn stale_snapshot() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: CompileFailureBody {
                code: "stale_snapshot".into(),
                message: "catalog snapshot changed; refresh and analyze again".into(),
                field: Some("snapshot_id".into()),
                engine_detail: None,
            },
        }
    }

    fn from_compile(error: CompileError) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            body: CompileFailureBody {
                code: error.code,
                message: error.message,
                field: error.field,
                engine_detail: None,
            },
        }
    }

    fn unplannable(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            body: CompileFailureBody {
                code: "unplannable".into(),
                message: "compiled SQL could not be planned against the live catalog".into(),
                field: None,
                engine_detail: Some(truncate_engine_detail(detail.into())),
            },
        }
    }
}

impl IntoResponse for CompileHttpError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

fn truncate_engine_detail(detail: String) -> String {
    const MAX_DETAIL_CHARS: usize = 1500;
    if detail.chars().count() > MAX_DETAIL_CHARS {
        format!(
            "{}…",
            detail.chars().take(MAX_DETAIL_CHARS).collect::<String>()
        )
    } else {
        detail
    }
}

fn compile_scope_from(scope: AnalysisCompileScope) -> CompileScope {
    let mut dataset = scope.database;
    let mut file = None;
    let mut session_ids = Vec::new();
    let mut document_id = None;
    for item in scope.items {
        match item {
            AnalysisCompileScopeItem::Dataset { name } => dataset = name,
            AnalysisCompileScopeItem::Root {
                dataset: next_dataset,
                file: next_file,
                root_session_id,
            } => {
                dataset = next_dataset;
                file = Some(next_file);
                session_ids.push(root_session_id);
            }
            AnalysisCompileScopeItem::Run { run } => {
                dataset = run.dataset;
                file = Some(run.file);
                session_ids.push(run.session_id);
                document_id = Some(run.document_id).filter(|value| !value.is_empty());
            }
        }
    }
    CompileScope {
        dataset,
        file,
        session_ids,
        document_id,
    }
}

async fn compile_analysis(
    State(state): State<AppState>,
    request_id: RequestId,
    request: Result<Json<CompileAnalysisRequest>, JsonRejection>,
) -> Result<Json<CompiledQuery>, CompileHttpError> {
    let Json(request) = request.map_err(|_| CompileHttpError {
        status: StatusCode::BAD_REQUEST,
        body: CompileFailureBody {
            code: "invalid_request".into(),
            message: "request body must be valid JSON".into(),
            field: None,
            engine_detail: None,
        },
    })?;
    let runtime = current_catalog(&state, &request_id)
        .await
        .map_err(|_| CompileHttpError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body: CompileFailureBody {
                code: "unavailable".into(),
                message: "catalog is not ready".into(),
                field: None,
                engine_detail: None,
            },
        })?;
    if request.snapshot_id != runtime.snapshot.snapshot_id() {
        return Err(CompileHttpError::stale_snapshot());
    }
    let schema = runtime
        .engine
        .introspect_tables()
        .await
        .map_err(|error| CompileHttpError::unplannable(format!("{error:#}")))?
        .into_iter()
        .map(|table| TableSchema {
            name: table.name,
            columns: table.fields.into_iter().map(|field| field.name).collect(),
        })
        .collect::<Vec<_>>();
    let dataset = request.scope.database.clone();
    let compiled = compile(request.spec, &schema, &compile_scope_from(request.scope))
        .map_err(CompileHttpError::from_compile)?;
    tracing::info!(
        target: LOG_TARGET,
        request_id = %request_id.as_str(),
        dataset = %dataset,
        sql = %truncate_utf8(&compiled.sql, QUERY_LOG_LIMIT),
        "warehouse compile"
    );
    runtime
        .engine
        .query(&format!("EXPLAIN {}", compiled.sql))
        .await
        .map_err(|error| CompileHttpError::unplannable(format!("{error:#}")))?;
    Ok(Json(compiled))
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
    request_id: RequestId,
    request: Result<Json<QueryEvidenceRequest>, JsonRejection>,
) -> Result<Json<QueryEvidence>, ApiError> {
    let Json(request) =
        request.map_err(|_| ApiError::invalid_request("request body must be valid JSON"))?;
    validate_read_only_sql(&request.sql).map_err(ApiError::input)?;
    tracing::info!(
        target: LOG_TARGET,
        request_id = %request_id.as_str(),
        sql = %truncate_utf8(&request.sql, QUERY_LOG_LIMIT),
        "warehouse query"
    );
    let max_rows = request.max_rows.unwrap_or(50).clamp(1, 200);
    let max_bytes = request
        .max_bytes
        .unwrap_or(64 * 1024)
        .clamp(1024, 8 * 1024 * 1024);
    let runtime = current_catalog(&state, &request_id).await?;
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
    let bytes = match output
        .finish(write_result)
        .map_err(|error| query_evidence_error(request_id.as_str(), error))?
    {
        QueryEvidenceWriteOutcome::Complete(bytes) => bytes,
        QueryEvidenceWriteOutcome::LimitExceeded => {
            return Err(ApiError::resource_exhausted(
                "query result exceeds max_bytes limit",
            ));
        }
    };
    let body = String::from_utf8(bytes)
        .map_err(anyhow::Error::from)
        .map_err(|error| fail(&request_id, "query_evidence", error))?;
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
                .map_err(|error| fail(&request_id, "query_evidence", error))?,
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

/// Map a query-evidence execution failure to a client-visible error.
///
/// The SQL is caller-supplied, so planning and streaming failures (unknown
/// columns, invalid syntax, unsupported expressions) are input problems. The
/// original message is surfaced so Copilot tool loops and the query console
/// can self-correct instead of retrying against an opaque 500.
fn query_evidence_error(request_id: &str, error: anyhow::Error) -> ApiError {
    let detail = format!("{error:#}");
    const MAX_DETAIL_CHARS: usize = 1500;
    let message = if detail.chars().count() > MAX_DETAIL_CHARS {
        let truncated: String = detail.chars().take(MAX_DETAIL_CHARS).collect();
        format!("{truncated}…")
    } else {
        detail
    };
    ApiError::invalid_request(message)
        .with_request_id(request_id)
        .with_4xx_root_cause(&error)
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

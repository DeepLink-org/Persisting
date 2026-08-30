use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use persisting_pchronicle::storage::{
    DEFAULT_PHYSICAL_PAGE_LIMIT, PhysicalFileLayout, PhysicalLayout, PhysicalPagePreview,
    PhysicalPageQuery, PhysicalSource, inspect_physical_file, inspect_physical_layout,
    inspect_physical_page, list_physical_sources,
};
use serde::Deserialize;

use super::request_log::RequestId;
use super::{ApiError, AppState, api_query, current_catalog};

#[derive(Debug, Deserialize)]
pub(super) struct LayoutQuery {
    dataset: String,
    file: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct FileQuery {
    dataset: String,
    file: String,
    table: String,
    fragment: u64,
    data_file: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct PageQuery {
    dataset: String,
    file: String,
    table: String,
    fragment: u64,
    data_file: String,
    column: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}

pub(super) async fn sources(
    State(state): State<AppState>,
    request_id: RequestId,
) -> Result<Json<Vec<PhysicalSource>>, ApiError> {
    let runtime = current_catalog(&state, &request_id).await?;
    Ok(Json(list_physical_sources(&runtime.snapshot)))
}

pub(super) async fn layout(
    State(state): State<AppState>,
    request_id: RequestId,
    query: Result<Query<LayoutQuery>, QueryRejection>,
) -> Result<Json<PhysicalLayout>, ApiError> {
    let query = api_query(query)?;
    let runtime = current_catalog(&state, &request_id).await?;
    inspect_physical_layout(&runtime.snapshot, &query.dataset, &query.file)
        .await
        .map(Json)
        .map_err(|error| map_inspect(request_id.as_str(), "physical_layout", error))
}

pub(super) async fn file(
    State(state): State<AppState>,
    request_id: RequestId,
    query: Result<Query<FileQuery>, QueryRejection>,
) -> Result<Json<PhysicalFileLayout>, ApiError> {
    let query = api_query(query)?;
    let runtime = current_catalog(&state, &request_id).await?;
    inspect_physical_file(
        &runtime.snapshot,
        &query.dataset,
        &query.file,
        &query.table,
        query.fragment,
        &query.data_file,
    )
    .await
    .map(Json)
    .map_err(|error| map_inspect(request_id.as_str(), "physical_file", error))
}

pub(super) async fn page(
    State(state): State<AppState>,
    request_id: RequestId,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<PhysicalPagePreview>, ApiError> {
    let query = api_query(query)?;
    let runtime = current_catalog(&state, &request_id).await?;
    inspect_physical_page(
        &runtime.snapshot,
        PhysicalPageQuery {
            dataset: &query.dataset,
            file: &query.file,
            table: &query.table,
            fragment_id: query.fragment,
            data_file: &query.data_file,
            column: query.column.as_deref(),
            offset: query.offset.unwrap_or(0),
            limit: query.limit.unwrap_or(DEFAULT_PHYSICAL_PAGE_LIMIT),
        },
    )
    .await
    .map(Json)
    .map_err(|error| map_inspect(request_id.as_str(), "physical_page", error))
}

pub(super) fn map_inspect(
    request_id: &str,
    handler: &'static str,
    error: anyhow::Error,
) -> ApiError {
    // Documented inspect messages from `persisting_pchronicle::storage`.
    // Classify by prefix of a chain layer's Display; do not use a generic
    // `contains("not found")` check.
    for message in error.chain().map(ToString::to_string) {
        if message.starts_with("physical source not found") {
            return ApiError::not_found(message)
                .with_request_id(request_id)
                .with_4xx_root_cause(&error);
        }
        if message.starts_with("physical source is not a Lance dataset") {
            return ApiError::invalid_request(message)
                .with_request_id(request_id)
                .with_4xx_root_cause(&error);
        }
    }
    ApiError::from_anyhow(request_id, handler, error)
}

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::Json;
use persisting_pchronicle::storage::{
    inspect_physical_file, inspect_physical_layout, inspect_physical_page, list_physical_sources,
    PhysicalFileLayout, PhysicalLayout, PhysicalPagePreview, PhysicalSource,
    DEFAULT_PHYSICAL_PAGE_LIMIT,
};
use serde::Deserialize;

use super::{api_query, current_catalog, ApiError, AppState};

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
) -> Result<Json<Vec<PhysicalSource>>, ApiError> {
    let runtime = current_catalog(&state).await?;
    Ok(Json(list_physical_sources(&runtime.snapshot)))
}

pub(super) async fn layout(
    State(state): State<AppState>,
    query: Result<Query<LayoutQuery>, QueryRejection>,
) -> Result<Json<PhysicalLayout>, ApiError> {
    let query = api_query(query)?;
    let runtime = current_catalog(&state).await?;
    inspect_physical_layout(&runtime.snapshot, &query.dataset, &query.file)
        .await
        .map(Json)
        .map_err(map_inspect)
}

pub(super) async fn file(
    State(state): State<AppState>,
    query: Result<Query<FileQuery>, QueryRejection>,
) -> Result<Json<PhysicalFileLayout>, ApiError> {
    let query = api_query(query)?;
    let runtime = current_catalog(&state).await?;
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
    .map_err(map_inspect)
}

pub(super) async fn page(
    State(state): State<AppState>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<PhysicalPagePreview>, ApiError> {
    let query = api_query(query)?;
    let runtime = current_catalog(&state).await?;
    inspect_physical_page(
        &runtime.snapshot,
        &query.dataset,
        &query.file,
        &query.table,
        query.fragment,
        &query.data_file,
        query.column.as_deref(),
        query.offset.unwrap_or(0),
        query.limit.unwrap_or(DEFAULT_PHYSICAL_PAGE_LIMIT),
    )
    .await
    .map(Json)
    .map_err(map_inspect)
}

fn map_inspect(error: anyhow::Error) -> ApiError {
    let message = error.to_string();
    if message.contains("physical source not found") || message.contains("not found") {
        ApiError::not_found(message)
    } else if message.contains("not a Lance dataset") {
        ApiError::invalid_request(message)
    } else {
        ApiError::internal(error)
    }
}

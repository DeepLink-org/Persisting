use crate::analysis_session::{AnalysisScope, AnalysisSpec, CompileFailure, CompiledQuery};
use crate::model::{
    CatalogTree, PhysicalFileLayout, PhysicalLayout, PhysicalPagePreview, PhysicalSource,
    QueryCatalog, QueryEvidence, RunAnalysis, RunPage, RunSummary, TurnDetail,
    TurnPage,
};
use gloo_net::http::{Request, Response};
use serde_json::json;

async fn checked(response: Response) -> Result<Response, String> {
    if response.ok() {
        Ok(response)
    } else {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "Request failed".into());
        Err(format!("HTTP {status}: {body}"))
    }
}

pub async fn explorer_runs(
    q: &str,
    dataset: &str,
    status: &str,
    sort: &str,
    direction: &str,
    path: &str,
    file: &str,
    offset: usize,
) -> Result<RunPage, String> {
    let url = format!(
        "/api/explorer/runs?q={}&dataset={}&status={}&sort={}&direction={}&path={}&file={}&offset={offset}&limit=50",
        urlencoding::encode(q),
        urlencoding::encode(dataset),
        urlencoding::encode(status),
        urlencoding::encode(sort),
        urlencoding::encode(direction),
        urlencoding::encode(path),
        urlencoding::encode(file),
    );
    checked(Request::get(&url).send().await.map_err(|e| e.to_string())?)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn explorer_tree(dataset: &str, prefix: &str) -> Result<CatalogTree, String> {
    let url = format!(
        "/api/explorer/tree?dataset={}&prefix={}",
        urlencoding::encode(dataset),
        urlencoding::encode(prefix),
    );
    checked(Request::get(&url).send().await.map_err(|e| e.to_string())?)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn run_analysis(run: &RunSummary) -> Result<RunAnalysis, String> {
    checked(
        Request::get(&format!("/api/explorer/run?{}", run.query()))
            .send()
            .await
            .map_err(|e| e.to_string())?,
    )
    .await?
    .json()
    .await
    .map_err(|e| e.to_string())
}

pub async fn turns(run: &RunSummary, q: &str, source: &str) -> Result<TurnPage, String> {
    let url = format!(
        "/api/explorer/turns?{}&q={}&source={}&offset=0&limit=500",
        run.query(),
        urlencoding::encode(q),
        urlencoding::encode(source),
    );
    checked(Request::get(&url).send().await.map_err(|e| e.to_string())?)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn turn_detail(run: &RunSummary, turn_id: i64) -> Result<TurnDetail, String> {
    checked(
        Request::get(&format!(
            "/api/explorer/turn?{}&turn_id={turn_id}",
            run.query()
        ))
        .send()
        .await
        .map_err(|e| e.to_string())?,
    )
    .await?
    .json()
    .await
    .map_err(|e| e.to_string())
}

pub async fn query_evidence(sql: &str) -> Result<QueryEvidence, String> {
    query_evidence_with_budget(sql, 50, 64 * 1024).await
}

pub async fn query_evidence_interactive(sql: &str) -> Result<QueryEvidence, String> {
    query_evidence_with_budget(sql, 100, 4 * 1024 * 1024).await
}

async fn query_evidence_with_budget(
    sql: &str,
    max_rows: usize,
    max_bytes: usize,
) -> Result<QueryEvidence, String> {
    let response = Request::post("/api/query/evidence")
        .json(&json!({ "sql": sql, "max_rows": max_rows, "max_bytes": max_bytes }))
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    checked(response)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn compile_analysis(
    spec: &AnalysisSpec,
    snapshot_id: &str,
    scope: &AnalysisScope,
) -> Result<CompiledQuery, CompileFailure> {
    let response = Request::post("/api/analysis/compile")
        .json(&json!({
            "spec": spec,
            "snapshot_id": snapshot_id,
            "scope": scope,
        }))
        .map_err(|error| CompileFailure {
            code: "invalid_request".into(),
            message: error.to_string(),
            field: None,
            engine_detail: None,
        })?
        .send()
        .await
        .map_err(|error| CompileFailure {
            code: "unavailable".into(),
            message: error.to_string(),
            field: None,
            engine_detail: None,
        })?;
    if response.ok() {
        return response.json().await.map_err(|error| CompileFailure {
            code: "invalid_request".into(),
            message: error.to_string(),
            field: None,
            engine_detail: None,
        });
    }
    let status = response.status();
    match response.json::<CompileFailure>().await {
        Ok(failure) => Err(failure),
        Err(_) => Err(CompileFailure {
            code: "invalid_request".into(),
            message: format!("HTTP {status}: compile failed"),
            field: None,
            engine_detail: None,
        }),
    }
}

pub async fn query_catalog() -> Result<QueryCatalog, String> {
    checked(
        Request::get("/api/query/tables")
            .send()
            .await
            .map_err(|e| e.to_string())?,
    )
    .await?
    .json()
    .await
    .map_err(|e| e.to_string())
}

pub async fn refresh_catalog() -> Result<(), String> {
    checked(
        Request::post("/api/catalog")
            .send()
            .await
            .map_err(|e| e.to_string())?,
    )
    .await?;
    Ok(())
}

pub async fn physical_sources() -> Result<Vec<PhysicalSource>, String> {
    checked(
        Request::get("/api/physical/sources")
            .send()
            .await
            .map_err(|e| e.to_string())?,
    )
    .await?
    .json()
    .await
    .map_err(|e| e.to_string())
}

pub async fn physical_layout(dataset: &str, file: &str) -> Result<PhysicalLayout, String> {
    let url = format!(
        "/api/physical/layout?dataset={}&file={}",
        urlencoding::encode(dataset),
        urlencoding::encode(file),
    );
    checked(Request::get(&url).send().await.map_err(|e| e.to_string())?)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn physical_file(
    dataset: &str,
    file: &str,
    table: &str,
    fragment: u64,
    data_file: &str,
) -> Result<PhysicalFileLayout, String> {
    let url = format!(
        "/api/physical/file?dataset={}&file={}&table={}&fragment={fragment}&data_file={}",
        urlencoding::encode(dataset),
        urlencoding::encode(file),
        urlencoding::encode(table),
        urlencoding::encode(data_file),
    );
    checked(Request::get(&url).send().await.map_err(|e| e.to_string())?)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn physical_page(
    dataset: &str,
    file: &str,
    table: &str,
    fragment: u64,
    data_file: &str,
    column: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<PhysicalPagePreview, String> {
    let mut url = format!(
        "/api/physical/page?dataset={}&file={}&table={}&fragment={fragment}&data_file={}&offset={offset}&limit={limit}",
        urlencoding::encode(dataset),
        urlencoding::encode(file),
        urlencoding::encode(table),
        urlencoding::encode(data_file),
    );
    if let Some(column) = column {
        url.push_str("&column=");
        url.push_str(&urlencoding::encode(column));
    }
    checked(Request::get(&url).send().await.map_err(|e| e.to_string())?)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

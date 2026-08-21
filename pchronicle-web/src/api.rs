use crate::model::{
    QueryCatalog, QueryEvidence, RunAnalysis, RunPage, RunSummary, TurnDetail, TurnPage,
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
    offset: usize,
) -> Result<RunPage, String> {
    let url = format!(
        "/api/explorer/runs?q={}&dataset={}&status={}&sort={}&direction={}&path={}&offset={offset}&limit=50",
        urlencoding::encode(q),
        urlencoding::encode(dataset),
        urlencoding::encode(status),
        urlencoding::encode(sort),
        urlencoding::encode(direction),
        urlencoding::encode(path),
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

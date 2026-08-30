use crate::analysis_session::{AnalysisScope, AnalysisSpec, CompileFailure, CompiledQuery};
use crate::model::{
    CatalogTree, PhysicalFileLayout, PhysicalLayout, PhysicalPagePreview, PhysicalSource,
    QueryCatalog, QueryEvidence, RunAnalysis, RunPage, RunSummary, TurnDetail, TurnPage,
};
use gloo_net::http::{Request, Response};
use serde::de::DeserializeOwned;
use serde_json::json;

#[derive(Clone, Debug, PartialEq)]
pub struct ApiFailure {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub request_id: Option<String>,
    pub field: Option<String>,
    pub engine_detail: Option<String>,
    pub raw: String,
}

impl ApiFailure {
    pub fn network(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            status: 0,
            code: "unavailable".into(),
            message: message.clone(),
            request_id: None,
            field: None,
            engine_detail: None,
            raw: message,
        }
    }
}

impl std::fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.request_id.as_deref() {
            Some(id) => write!(f, "{} (request_id={id})", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

impl From<ApiFailure> for CompileFailure {
    fn from(failure: ApiFailure) -> Self {
        Self {
            code: failure.code,
            message: failure.message,
            field: failure.field,
            engine_detail: failure.engine_detail,
            request_id: failure.request_id,
        }
    }
}

pub(crate) fn parse_api_failure(status: u16, body: &str) -> ApiFailure {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        let code = value
            .get("code")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let message = value
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or(body)
            .to_string();
        let request_id = value
            .get("request_id")
            .and_then(|value| value.as_str())
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        let field = value
            .get("field")
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        let engine_detail = value
            .get("engine_detail")
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        return ApiFailure {
            status,
            code,
            message,
            request_id,
            field,
            engine_detail,
            raw: body.to_owned(),
        };
    }
    ApiFailure {
        status,
        code: String::new(),
        message: format!("HTTP {status}: {body}"),
        request_id: None,
        field: None,
        engine_detail: None,
        raw: body.to_owned(),
    }
}

async fn checked(response: Response) -> Result<Response, ApiFailure> {
    if response.ok() {
        Ok(response)
    } else {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "Request failed".into());
        Err(parse_api_failure(status, &body))
    }
}

async fn send_checked(send: Result<Response, gloo_net::Error>) -> Result<Response, ApiFailure> {
    checked(send.map_err(|error| ApiFailure::network(error.to_string()))?).await
}

async fn json_checked<T: DeserializeOwned>(
    send: Result<Response, gloo_net::Error>,
) -> Result<T, ApiFailure> {
    send_checked(send)
        .await?
        .json()
        .await
        .map_err(|error| ApiFailure::network(error.to_string()))
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
    limit: usize,
) -> Result<RunPage, ApiFailure> {
    let url = format!(
        "/api/explorer/runs?q={}&dataset={}&status={}&sort={}&direction={}&path={}&file={}&offset={offset}&limit={limit}",
        urlencoding::encode(q),
        urlencoding::encode(dataset),
        urlencoding::encode(status),
        urlencoding::encode(sort),
        urlencoding::encode(direction),
        urlencoding::encode(path),
        urlencoding::encode(file),
    );
    json_checked(Request::get(&url).send().await).await
}

pub async fn explorer_tree(dataset: &str, prefix: &str) -> Result<CatalogTree, ApiFailure> {
    let url = format!(
        "/api/explorer/tree?dataset={}&prefix={}",
        urlencoding::encode(dataset),
        urlencoding::encode(prefix),
    );
    json_checked(Request::get(&url).send().await).await
}

pub async fn run_analysis(run: &RunSummary) -> Result<RunAnalysis, ApiFailure> {
    json_checked(
        Request::get(&format!("/api/explorer/run?{}", run.query()))
            .send()
            .await,
    )
    .await
}

pub async fn turns(run: &RunSummary, q: &str, source: &str) -> Result<TurnPage, ApiFailure> {
    let url = format!(
        "/api/explorer/turns?{}&q={}&source={}&offset=0&limit=500",
        run.query(),
        urlencoding::encode(q),
        urlencoding::encode(source),
    );
    json_checked(Request::get(&url).send().await).await
}

pub async fn turn_detail(run: &RunSummary, turn_id: i64) -> Result<TurnDetail, ApiFailure> {
    json_checked(
        Request::get(&format!(
            "/api/explorer/turn?{}&turn_id={turn_id}",
            run.query()
        ))
        .send()
        .await,
    )
    .await
}

pub async fn query_evidence(sql: &str) -> Result<QueryEvidence, ApiFailure> {
    query_evidence_with_budget(sql, 50, 64 * 1024).await
}

pub async fn query_evidence_interactive(sql: &str) -> Result<QueryEvidence, ApiFailure> {
    query_evidence_with_budget(sql, 100, 4 * 1024 * 1024).await
}

async fn query_evidence_with_budget(
    sql: &str,
    max_rows: usize,
    max_bytes: usize,
) -> Result<QueryEvidence, ApiFailure> {
    let request = Request::post("/api/query/evidence")
        .json(&json!({ "sql": sql, "max_rows": max_rows, "max_bytes": max_bytes }))
        .map_err(|error| ApiFailure::network(error.to_string()))?;
    json_checked(request.send().await).await
}

pub async fn compile_analysis(
    spec: &AnalysisSpec,
    snapshot_id: &str,
    scope: &AnalysisScope,
) -> Result<CompiledQuery, CompileFailure> {
    let request = Request::post("/api/analysis/compile")
        .json(&json!({
            "spec": spec,
            "snapshot_id": snapshot_id,
            "scope": scope,
        }))
        .map_err(|error| CompileFailure::from(ApiFailure::network(error.to_string())))?;
    let response = request
        .send()
        .await
        .map_err(|error| CompileFailure::from(ApiFailure::network(error.to_string())))?;
    if response.ok() {
        return response
            .json()
            .await
            .map_err(|error| CompileFailure::from(ApiFailure::network(error.to_string())));
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Err(CompileFailure::from(parse_api_failure(status, &body)))
}

pub async fn query_catalog() -> Result<QueryCatalog, ApiFailure> {
    json_checked(Request::get("/api/query/tables").send().await).await
}

pub async fn refresh_catalog() -> Result<(), ApiFailure> {
    send_checked(Request::post("/api/catalog").send().await).await?;
    Ok(())
}

pub async fn physical_sources() -> Result<Vec<PhysicalSource>, ApiFailure> {
    json_checked(Request::get("/api/physical/sources").send().await).await
}

pub async fn physical_layout(dataset: &str, file: &str) -> Result<PhysicalLayout, ApiFailure> {
    let url = format!(
        "/api/physical/layout?dataset={}&file={}",
        urlencoding::encode(dataset),
        urlencoding::encode(file),
    );
    json_checked(Request::get(&url).send().await).await
}

pub async fn physical_file(
    dataset: &str,
    file: &str,
    table: &str,
    fragment: u64,
    data_file: &str,
) -> Result<PhysicalFileLayout, ApiFailure> {
    let url = format!(
        "/api/physical/file?dataset={}&file={}&table={}&fragment={fragment}&data_file={}",
        urlencoding::encode(dataset),
        urlencoding::encode(file),
        urlencoding::encode(table),
        urlencoding::encode(data_file),
    );
    json_checked(Request::get(&url).send().await).await
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
) -> Result<PhysicalPagePreview, ApiFailure> {
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
    json_checked(Request::get(&url).send().await).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_api_failure_reads_code_and_request_id() {
        let failure = parse_api_failure(
            400,
            r#"{"code":"resource_exhausted","message":"limit","request_id":"abc123"}"#,
        );
        assert_eq!(failure.code, "resource_exhausted");
        assert_eq!(failure.message, "limit");
        assert_eq!(failure.request_id.as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_api_failure_falls_back_for_non_json() {
        let failure = parse_api_failure(500, "not-json");
        assert_eq!(failure.code, "");
        assert_eq!(failure.message, "HTTP 500: not-json");
        assert!(failure.request_id.is_none());
    }

    #[test]
    fn network_failure_is_unavailable() {
        let failure = ApiFailure::network("connection refused");
        assert_eq!(failure.code, "unavailable");
        assert!(failure.request_id.is_none());
    }
}

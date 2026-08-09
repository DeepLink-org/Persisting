use crate::model::{EventRecord, EventsPage, QueryCatalog, RunSummary, TrajectoryView};
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

pub async fn runs() -> Result<Vec<RunSummary>, String> {
    checked(
        Request::get("/api/v1/runs")
            .send()
            .await
            .map_err(|e| e.to_string())?,
    )
    .await?
    .json()
    .await
    .map_err(|e| e.to_string())
}

pub async fn trajectory(run: &RunSummary) -> Result<TrajectoryView, String> {
    checked(
        Request::get(&format!("/api/v1/trajectory-view?{}", run.query()))
            .send()
            .await
            .map_err(|e| e.to_string())?,
    )
    .await?
    .json()
    .await
    .map_err(|e| e.to_string())
}

pub async fn all_events(run: &RunSummary) -> Result<Vec<EventRecord>, String> {
    let mut offset = 0;
    let mut records = Vec::new();
    loop {
        let url = format!("/api/v1/events?{}&offset={offset}&limit=1000", run.query());
        let page: EventsPage = checked(Request::get(&url).send().await.map_err(|e| e.to_string())?)
            .await?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        let next = page.snapshot.next_offset;
        records.extend(page.records);
        if !page.snapshot.has_more || next <= offset || records.len() >= page.snapshot.total {
            return Ok(records);
        }
        offset = next;
    }
}

pub async fn sql(sql: &str) -> Result<String, String> {
    let response = Request::post("/api/v1/query")
        .json(&json!({ "sql": sql }))
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    checked(response)
        .await?
        .text()
        .await
        .map_err(|e| e.to_string())
}

pub async fn query_catalog() -> Result<QueryCatalog, String> {
    checked(
        Request::get("/api/v1/query/tables")
            .send()
            .await
            .map_err(|e| e.to_string())?,
    )
    .await?
    .json()
    .await
    .map_err(|e| e.to_string())
}

use anyhow::{Context, Result};
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use persisting_pchronicle::{DatasetMount, DEFAULT_DATASET_NAME};
use persisting_pchronicle_cli::server::{warehouse_router, ChronicleServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

fn example_uri(name: &str) -> String {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/data")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn warehouse() -> Result<axum::Router> {
    let mut config = ChronicleServerConfig::mounted(vec![
        DatasetMount::new(DEFAULT_DATASET_NAME, example_uri("atif"))?,
        DatasetMount::new("openai", example_uri("openai-messages"))?,
        DatasetMount::new("actf", example_uri("actf"))?,
    ])?;
    config.default_dataset = Some(DEFAULT_DATASET_NAME.into());
    Ok(warehouse_router(config))
}

async fn json_body(response: axum::response::Response) -> Result<Value> {
    let bytes = response
        .into_body()
        .collect()
        .await
        .context("collect response body")?
        .to_bytes();
    serde_json::from_slice(&bytes).context("decode JSON response")
}

#[tokio::test]
async fn warehouse_read_route_matrix_exposes_the_documented_surface() -> Result<()> {
    let app = warehouse()?;
    for (path, assertion) in [
        ("/api/health", "health"),
        ("/api/catalog", "catalog"),
        ("/api/query/tables", "tables"),
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK, "GET {path}");
        let body = json_body(response).await?;
        match assertion {
            "health" => assert_eq!(body, json!({"status":"ok","mode":"read_only"})),
            "catalog" => {
                assert_eq!(body["datasets"].as_array().map(Vec::len), Some(3));
            }
            "tables" => {
                assert_eq!(body["read_only"], true);
                assert_eq!(body["datasets"].as_array().map(Vec::len), Some(3));
                assert_eq!(body["tables"].as_array().map(Vec::len), Some(6));
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}

#[tokio::test]
async fn warehouse_write_route_matrix_never_exposes_dataset_mutations() -> Result<()> {
    let app = warehouse()?;
    for (method, path) in [
        (Method::POST, "/api/maintain"),
        (Method::POST, "/api/query"),
        (Method::PUT, "/api/events"),
        (Method::DELETE, "/api/catalog"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))?,
            )
            .await?;
        assert!(
            matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "{method} {path} returned {}",
            response.status()
        );
    }
    Ok(())
}

#[tokio::test]
async fn catalog_refresh_is_the_only_allowed_read_side_post() -> Result<()> {
    let response = warehouse()?
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/catalog")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await?;
    assert_eq!(body["datasets"].as_array().map(Vec::len), Some(3));
    Ok(())
}

#[tokio::test]
async fn evidence_query_is_read_only_and_server_bounded() -> Result<()> {
    let app = warehouse()?;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/query/evidence")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "sql": "SELECT * FROM dataset.steps ORDER BY step_id",
                        "max_rows": 1,
                        "max_bytes": 4096
                    })
                    .to_string(),
                ))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await?;
    assert_eq!(body["returned_rows"], 1);
    assert_eq!(body["truncated"], true);
    assert_eq!(body["max_rows"], 1);
    assert_eq!(body["rows"].as_array().map(Vec::len), Some(1));

    for sql in ["", "DELETE FROM dataset.runs", "SELECT 1; SELECT 2"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/query/evidence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"sql": sql}).to_string()))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "sql={sql:?}");
        assert_eq!(json_body(response).await?["code"], "read_only_sql");
    }
    Ok(())
}

#[tokio::test]
async fn spa_fallback_never_masks_unknown_api_routes() -> Result<()> {
    let app = warehouse()?;
    let page = app
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty())?)
        .await?;
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(
        page.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );

    let unknown = app
        .oneshot(
            Request::builder()
                .uri("/api/does-not-exist")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    Ok(())
}

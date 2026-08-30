use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use persisting_pchronicle::storage::{DatasetLocation, DatasetMount};
use serde::{Deserialize, Serialize};
use url::Url;

use super::problem::ApiError;

pub(crate) const ACCESS_KEY_HEADER: &str = "x-pchronicle-access-key";
pub(crate) const SECRET_KEY_HEADER: &str = "x-pchronicle-secret-key";
const MAX_CATALOG_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CatalogLibrary {
    pub name: String,
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_key: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CatalogUser {
    pub name: String,
    secret_key: String,
    datasets: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CatalogAcl {
    libraries: BTreeMap<String, CatalogLibrary>,
    users_by_access_key: HashMap<String, CatalogUser>,
}

#[derive(Debug, Deserialize)]
struct CatalogFile {
    #[serde(default)]
    libraries: BTreeMap<String, CatalogLibraryFile>,
    #[serde(default)]
    users: BTreeMap<String, CatalogUserFile>,
}

#[derive(Debug, Deserialize)]
struct CatalogLibraryFile {
    uri: String,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    access_key: Option<String>,
    #[serde(default)]
    secret_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogUserFile {
    access_key: String,
    secret_key: String,
    #[serde(default)]
    datasets: Vec<String>,
}

impl CatalogAcl {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("read catalog config metadata {}", path.display()))?;
        anyhow::ensure!(metadata.is_file(), "catalog config must be a regular file");
        anyhow::ensure!(
            metadata.len() <= MAX_CATALOG_CONFIG_BYTES,
            "catalog config exceeds the {MAX_CATALOG_CONFIG_BYTES} byte limit"
        );
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("read catalog config {}", path.display()))?;
        Self::parse(&content)
    }

    pub(crate) fn parse(content: &str) -> Result<Self> {
        let file: CatalogFile = toml::from_str(content).context("parse catalog config")?;
        anyhow::ensure!(
            !file.libraries.is_empty(),
            "catalog config needs at least one library"
        );
        anyhow::ensure!(
            !file.users.is_empty(),
            "catalog config needs at least one user"
        );

        let mut libraries = BTreeMap::new();
        let mut s3_credential: Option<(Option<String>, Option<String>, String, String)> = None;
        for (name, library) in file.libraries {
            let mount = DatasetMount::new(&name, "validation")
                .with_context(|| format!("catalog library name '{name}'"))?;
            let location = DatasetLocation::parse(&library.uri)
                .with_context(|| format!("catalog library '{name}' URI"))?;
            let uri = location.as_str().to_owned();
            let is_s3 = uri.starts_with("s3://");
            match (library.access_key.as_deref(), library.secret_key.as_deref()) {
                (None, None) => anyhow::ensure!(
                    !is_s3,
                    "catalog library '{name}' is s3:// and must set access_key and secret_key"
                ),
                (Some(access_key), Some(secret_key)) => {
                    anyhow::ensure!(
                        is_s3,
                        "catalog library '{name}' is not s3:// and must not set backend keys"
                    );
                    let access_key = access_key.trim();
                    let secret_key = secret_key.trim();
                    anyhow::ensure!(
                        !access_key.is_empty(),
                        "catalog library '{name}' access_key is empty"
                    );
                    anyhow::ensure!(
                        !secret_key.is_empty(),
                        "catalog library '{name}' secret_key is empty"
                    );
                    let tuple = (
                        library.endpoint.clone(),
                        library.region.clone(),
                        access_key.to_owned(),
                        secret_key.to_owned(),
                    );
                    match &s3_credential {
                        None => s3_credential = Some(tuple),
                        Some(existing) => anyhow::ensure!(
                            existing == &tuple,
                            "all s3:// libraries must share the same endpoint, region, and backend keys"
                        ),
                    }
                }
                _ => anyhow::bail!(
                    "catalog library '{name}' must set both access_key and secret_key"
                ),
            }
            libraries.insert(
                mount.name.clone(),
                CatalogLibrary {
                    name: mount.name,
                    uri,
                    endpoint: library.endpoint,
                    region: library.region,
                    access_key: library.access_key.map(|value| value.trim().to_owned()),
                    secret_key: library.secret_key.map(|value| value.trim().to_owned()),
                },
            );
        }

        let mut users_by_access_key = HashMap::new();
        for (name, user) in file.users {
            let access_key = user.access_key.trim().to_owned();
            let secret_key = user.secret_key.trim().to_owned();
            anyhow::ensure!(
                !access_key.is_empty(),
                "catalog user '{name}' access_key is empty"
            );
            anyhow::ensure!(
                !secret_key.is_empty(),
                "catalog user '{name}' secret_key is empty"
            );
            for dataset in &user.datasets {
                anyhow::ensure!(
                    libraries.contains_key(dataset),
                    "catalog user '{name}' grants unknown library '{dataset}'"
                );
            }
            let catalog_user = CatalogUser {
                name,
                secret_key,
                datasets: user.datasets,
            };
            anyhow::ensure!(
                users_by_access_key
                    .insert(access_key, catalog_user)
                    .is_none(),
                "catalog user access keys must be unique"
            );
        }

        Ok(Self {
            libraries,
            users_by_access_key,
        })
    }

    pub(crate) fn authenticate(&self, access_key: &str, secret_key: &str) -> Option<&CatalogUser> {
        let user = self.users_by_access_key.get(access_key)?;
        if !secret_keys_match(&user.secret_key, secret_key) {
            return None;
        }
        Some(user)
    }

    pub(crate) fn list_for(&self, user: &CatalogUser) -> Vec<CatalogLibraryPublic> {
        user.datasets
            .iter()
            .filter_map(|name| self.libraries.get(name))
            .map(CatalogLibraryPublic::from)
            .collect()
    }

    pub(crate) fn ticket_for(&self, user: &CatalogUser, name: &str) -> Option<CatalogLibrary> {
        if !user.datasets.iter().any(|dataset| dataset == name) {
            return None;
        }
        self.libraries.get(name).cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogLibraryPublic {
    pub name: String,
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

impl From<&CatalogLibrary> for CatalogLibraryPublic {
    fn from(library: &CatalogLibrary) -> Self {
        Self {
            name: library.name.clone(),
            uri: library.uri.clone(),
            endpoint: library.endpoint.clone(),
            region: library.region.clone(),
        }
    }
}

pub(crate) fn apply_library_env(library: &CatalogLibrary) {
    if let Some(endpoint) = library.endpoint.as_deref() {
        unsafe {
            std::env::set_var("AWS_ENDPOINT", endpoint);
            std::env::set_var("AWS_ENDPOINT_URL_S3", endpoint);
            if endpoint.starts_with("http://") {
                std::env::set_var("AWS_ALLOW_HTTP", "true");
            }
        }
    }
    if let Some(region) = library.region.as_deref() {
        unsafe {
            std::env::set_var("AWS_REGION", region);
        }
    }
    if let (Some(access_key), Some(secret_key)) =
        (library.access_key.as_deref(), library.secret_key.as_deref())
    {
        unsafe {
            std::env::set_var("AWS_ACCESS_KEY_ID", access_key);
            std::env::set_var("AWS_SECRET_ACCESS_KEY", secret_key);
        }
    }
}

pub(super) fn catalog_unauthorized() -> ApiError {
    ApiError::unauthorized("catalog credentials are invalid")
}

pub(crate) fn parse_catalog_alias_target(input: &str) -> Result<String> {
    let input = input.trim();
    let url = Url::parse(input).context("parse catalog alias URL")?;
    anyhow::ensure!(
        url.scheme() == "catalog",
        "catalog alias target must use catalog://"
    );
    anyhow::ensure!(
        url.username().is_empty() && url.password().is_none(),
        "catalog alias URL must not contain embedded credentials"
    );
    anyhow::ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "catalog alias URL must not contain a query string or fragment"
    );
    anyhow::ensure!(
        url.path() == "/" || url.path().is_empty(),
        "catalog alias URL must not contain a path"
    );
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("catalog alias URL must include a host"))?;
    let address: std::net::IpAddr = host
        .parse()
        .with_context(|| format!("catalog alias host '{host}' must be a loopback IP"))?;
    anyhow::ensure!(
        address.is_loopback(),
        "catalog alias host must be a loopback address"
    );
    let port = url
        .port()
        .ok_or_else(|| anyhow!("catalog alias URL must include a port"))?;
    Ok(format!("catalog://{host}:{port}"))
}

pub(crate) fn catalog_http_base(catalog_url: &str) -> Result<String> {
    let normalized = parse_catalog_alias_target(catalog_url)?;
    Ok(normalized.replacen("catalog://", "http://", 1))
}

fn secret_keys_match(expected: &str, provided: &str) -> bool {
    let left = expected.as_bytes();
    let right = provided.as_bytes();
    if left.len() != right.len() {
        let mut acc = 0u8;
        for byte in left {
            acc |= *byte;
        }
        acc == 0 && false
    } else {
        left.iter()
            .zip(right)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

pub(crate) fn credentials_from_headers(
    headers: &axum::http::HeaderMap,
) -> Option<(String, String)> {
    let access = headers
        .get(ACCESS_KEY_HEADER)?
        .to_str()
        .ok()?
        .trim()
        .to_owned();
    let secret = headers
        .get(SECRET_KEY_HEADER)?
        .to_str()
        .ok()?
        .trim()
        .to_owned();
    if access.is_empty() || secret.is_empty() {
        return None;
    }
    Some((access, secret))
}

fn parent_handles_path(path: &str) -> bool {
    let rest = path
        .strip_prefix("/api/v1")
        .or_else(|| path.strip_prefix("/api"))
        .unwrap_or(path);
    rest == "/health" || rest == "/catalog/datasets" || rest.starts_with("/catalog/datasets/")
}

pub(super) async fn list_datasets(
    axum::extract::State(state): axum::extract::State<super::AppState>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<Vec<CatalogLibraryPublic>>, ApiError> {
    let acl = state
        .catalog_acl
        .as_ref()
        .ok_or_else(|| ApiError::not_found("catalog is not enabled"))?;
    let (access_key, secret_key) =
        credentials_from_headers(&headers).ok_or_else(catalog_unauthorized)?;
    let user = acl
        .authenticate(&access_key, &secret_key)
        .ok_or_else(catalog_unauthorized)?;
    Ok(axum::Json(acl.list_for(user)))
}

pub(super) async fn get_dataset(
    axum::extract::State(state): axum::extract::State<super::AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<CatalogLibrary>, ApiError> {
    let acl = state
        .catalog_acl
        .as_ref()
        .ok_or_else(|| ApiError::not_found("catalog is not enabled"))?;
    let (access_key, secret_key) =
        credentials_from_headers(&headers).ok_or_else(catalog_unauthorized)?;
    let user = acl
        .authenticate(&access_key, &secret_key)
        .ok_or_else(catalog_unauthorized)?;
    let ticket = acl
        .ticket_for(user, &name)
        .ok_or_else(|| ApiError::not_found("dataset not found"))?;
    Ok(axum::Json(ticket))
}

pub(super) async fn catalog_data_plane_layer(
    axum::extract::State(state): axum::extract::State<super::AppState>,
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    if state.catalog_query_worker || state.catalog_acl.is_none() {
        return next.run(request).await;
    }
    let path = request.uri().path().to_owned();
    if !path.starts_with("/api/") || parent_handles_path(&path) {
        return next.run(request).await;
    }
    match dispatch_query_worker(&state, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct CatalogWorkerJob {
    request_id: String,
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
    mounts: Vec<CatalogLibrary>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CatalogWorkerResult {
    status: u16,
    #[serde(default)]
    content_type: Option<String>,
    body: Vec<u8>,
}

async fn dispatch_query_worker(
    state: &super::AppState,
    request: axum::http::Request<axum::body::Body>,
) -> Result<axum::response::Response, ApiError> {
    let acl = state
        .catalog_acl
        .as_ref()
        .ok_or_else(|| ApiError::not_found("catalog is not enabled"))?;
    let (access_key, secret_key) =
        credentials_from_headers(request.headers()).ok_or_else(catalog_unauthorized)?;
    let user = acl
        .authenticate(&access_key, &secret_key)
        .ok_or_else(catalog_unauthorized)?
        .clone();
    tracing::debug!(
        target: super::problem::LOG_TARGET,
        user = %user.name,
        libraries = user.datasets.len(),
        path = %request.uri().path(),
        "dispatch catalog query worker"
    );
    if user.datasets.is_empty() {
        return Err(ApiError::not_found("dataset not found"));
    }
    let mounts: Vec<CatalogLibrary> = user
        .datasets
        .iter()
        .filter_map(|name| acl.ticket_for(&user, name))
        .collect();
    let request_id = request
        .extensions()
        .get::<super::request_log::RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_default();
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or("").to_owned();
    let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|error| ApiError::invalid_request(format!("read catalog query body: {error}")))?;
    let job = CatalogWorkerJob {
        request_id,
        method,
        path,
        query,
        body: body.to_vec(),
        mounts,
    };
    let payload = serde_json::to_vec(&job)
        .map_err(|error| ApiError::internal("", "catalog_worker", anyhow::anyhow!(error)))?;
    let exe = std::env::current_exe()
        .map_err(|error| ApiError::internal("", "catalog_worker", anyhow::anyhow!(error)))?;
    let mut child = tokio::process::Command::new(exe)
        .arg("serve")
        .arg("--catalog-query-worker")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| ApiError::internal("", "catalog_worker", anyhow::anyhow!(error)))?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            ApiError::internal(
                "",
                "catalog_worker",
                anyhow::anyhow!("catalog query worker stdin is missing"),
            )
        })?;
        stdin
            .write_all(&payload)
            .await
            .map_err(|error| ApiError::internal("", "catalog_worker", anyhow::anyhow!(error)))?;
        stdin
            .shutdown()
            .await
            .map_err(|error| ApiError::internal("", "catalog_worker", anyhow::anyhow!(error)))?;
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), child.wait_with_output())
        .await
        .map_err(|_| ApiError::unavailable())?
        .map_err(|error| ApiError::internal("", "catalog_worker", anyhow::anyhow!(error)))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(
            target: super::problem::LOG_TARGET,
            handler = "catalog_worker",
            exit = output.status.code().unwrap_or(-1),
            stderr = %super::problem::truncate_utf8(&stderr, super::problem::QUERY_LOG_LIMIT),
            "warehouse request failed"
        );
        return Err(ApiError::internal(
            "",
            "catalog_worker",
            anyhow::anyhow!("catalog query worker exited unsuccessfully"),
        ));
    }
    let result: CatalogWorkerResult = serde_json::from_slice(&output.stdout)
        .map_err(|error| ApiError::internal("", "catalog_worker", anyhow::anyhow!(error)))?;
    let status = axum::http::StatusCode::from_u16(result.status)
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = axum::response::Response::new(axum::body::Body::from(result.body));
    *response.status_mut() = status;
    if let Some(content_type) = result.content_type
        && let Ok(value) = axum::http::HeaderValue::from_str(&content_type)
    {
        response
            .headers_mut()
            .insert(axum::http::header::CONTENT_TYPE, value);
    }
    Ok(response)
}

pub(crate) async fn run_catalog_query_worker() -> Result<()> {
    use std::io::{Read, Write};

    use axum::body::Body;
    use tower::ServiceExt;

    let mut stdin = Vec::new();
    std::io::stdin()
        .read_to_end(&mut stdin)
        .context("read catalog query worker job")?;
    let job: CatalogWorkerJob =
        serde_json::from_slice(&stdin).context("decode catalog query worker job")?;
    if let Some(library) = job.mounts.first() {
        apply_library_env(library);
    }
    anyhow::ensure!(!job.mounts.is_empty(), "catalog query worker needs mounts");
    let mounts = job
        .mounts
        .iter()
        .map(|library| DatasetMount::new(&library.name, &library.uri))
        .collect::<Result<Vec<_>>>()?;
    let config = super::ChronicleServerConfig::mounted(mounts)?;
    let warehouse = super::PreparedWarehouse::prepare_query_worker(config).await?;
    let mut uri = job.path;
    if !job.query.is_empty() {
        uri.push('?');
        uri.push_str(&job.query);
    }
    let mut builder = axum::http::Request::builder()
        .method(job.method.as_str())
        .uri(uri);
    if !job.body.is_empty() {
        builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
    }
    let request = builder
        .body(Body::from(job.body))
        .context("build catalog query worker request")?;
    let response = warehouse
        .router()
        .oneshot(request)
        .await
        .map_err(|error| anyhow!(error))?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .context("read catalog query worker response")?;
    let result = CatalogWorkerResult {
        status,
        content_type,
        body: body.to_vec(),
    };
    serde_json::to_writer(std::io::stdout(), &result)
        .context("write catalog query worker result")?;
    std::io::stdout().flush().ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[libraries.prod]
uri = "s3://bucket/prod"
endpoint = "http://127.0.0.1:9000"
region = "us-west-2"
access_key = "BACKEND_AK"
secret_key = "BACKEND_SK"

[libraries.evals]
uri = "s3://bucket/evals"
endpoint = "http://127.0.0.1:9000"
region = "us-west-2"
access_key = "BACKEND_AK"
secret_key = "BACKEND_SK"

[users.alice]
access_key = "USER_AK"
secret_key = "USER_SK"
datasets = ["prod", "evals"]

[users.bob]
access_key = "BOB_AK"
secret_key = "BOB_SK"
datasets = ["evals"]
"#;

    #[test]
    fn parse_rejects_unknown_grant() {
        let error = CatalogAcl::parse(
            r#"
[libraries.prod]
uri = "s3://bucket/prod"
access_key = "a"
secret_key = "b"
[users.alice]
access_key = "u"
secret_key = "s"
datasets = ["missing"]
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown library 'missing'"), "{error}");
    }

    #[test]
    fn parse_rejects_duplicate_user_keys() {
        let error = CatalogAcl::parse(
            r#"
[libraries.prod]
uri = "s3://bucket/prod"
access_key = "a"
secret_key = "b"
[users.alice]
access_key = "same"
secret_key = "s1"
datasets = ["prod"]
[users.bob]
access_key = "same"
secret_key = "s2"
datasets = ["prod"]
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unique"), "{error}");
    }

    #[test]
    fn parse_rejects_mismatched_s3_backend_keys() {
        let error = CatalogAcl::parse(
            r#"
[libraries.prod]
uri = "s3://bucket/prod"
access_key = "a"
secret_key = "b"
[libraries.evals]
uri = "s3://bucket/evals"
access_key = "c"
secret_key = "d"
[users.alice]
access_key = "u"
secret_key = "s"
datasets = ["prod"]
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("share the same"), "{error}");
    }

    #[test]
    fn authenticate_and_filter_datasets() {
        let acl = CatalogAcl::parse(SAMPLE).unwrap();
        assert!(acl.authenticate("USER_AK", "wrong").is_none());
        let alice = acl.authenticate("USER_AK", "USER_SK").unwrap();
        let listed: Vec<_> = acl
            .list_for(alice)
            .into_iter()
            .map(|library| library.name)
            .collect();
        assert_eq!(listed, vec!["prod", "evals"]);
        let ticket = acl.ticket_for(alice, "prod").unwrap();
        assert_eq!(ticket.secret_key.as_deref(), Some("BACKEND_SK"));
        assert!(acl.ticket_for(alice, "missing").is_none());
        let bob = acl.authenticate("BOB_AK", "BOB_SK").unwrap();
        assert!(acl.ticket_for(bob, "prod").is_none());
        assert_eq!(acl.list_for(bob)[0].name, "evals");
        assert!(
            acl.list_for(bob)
                .iter()
                .all(|library| library.endpoint.is_some())
        );
    }

    #[test]
    fn public_list_omits_backend_secrets() {
        let acl = CatalogAcl::parse(SAMPLE).unwrap();
        let alice = acl.authenticate("USER_AK", "USER_SK").unwrap();
        let json = serde_json::to_string(&acl.list_for(alice)).unwrap();
        assert!(!json.contains("BACKEND_SK"));
        assert!(!json.contains("BACKEND_AK"));
    }

    #[test]
    fn catalog_alias_target_must_be_loopback_with_port() {
        assert!(parse_catalog_alias_target("catalog://127.0.0.1:8081").is_ok());
        assert!(parse_catalog_alias_target("catalog://8.8.8.8:8081").is_err());
        assert!(parse_catalog_alias_target("catalog://127.0.0.1").is_err());
        assert!(parse_catalog_alias_target("s3://bucket/prod").is_err());
    }

    #[test]
    fn parent_keeps_health_and_catalog_ticket_routes() {
        assert!(parent_handles_path("/api/health"));
        assert!(parent_handles_path("/api/v1/catalog/datasets"));
        assert!(parent_handles_path("/api/v1/catalog/datasets/prod"));
        assert!(!parent_handles_path("/api/catalog"));
        assert!(!parent_handles_path("/api/explorer/runs"));
        assert!(!parent_handles_path("/api/query/tables"));
    }

    async fn catalog_front() -> axum::Router {
        let acl = CatalogAcl::parse(SAMPLE).unwrap();
        crate::server::PreparedWarehouse::prepare_catalog_front(acl)
            .await
            .unwrap()
            .router()
    }

    async fn catalog_body(response: axum::response::Response) -> (axum::http::StatusCode, String) {
        use http_body_util::BodyExt;

        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    fn catalog_request(
        uri: &str,
        access_key: Option<&str>,
        secret_key: Option<&str>,
    ) -> axum::http::Request<axum::body::Body> {
        let mut builder = axum::http::Request::builder().uri(uri);
        if let Some(access_key) = access_key {
            builder = builder.header(ACCESS_KEY_HEADER, access_key);
        }
        if let Some(secret_key) = secret_key {
            builder = builder.header(SECRET_KEY_HEADER, secret_key);
        }
        builder.body(axum::body::Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn catalog_list_requires_credentials_and_omits_backend_secrets() {
        use tower::ServiceExt;

        let app = catalog_front().await;
        let (status, _) = catalog_body(
            app.clone()
                .oneshot(catalog_request("/api/v1/catalog/datasets", None, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

        let (status, body) = catalog_body(
            app.clone()
                .oneshot(catalog_request(
                    "/api/v1/catalog/datasets",
                    Some("USER_AK"),
                    Some("wrong"),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
        assert!(!body.contains("USER_SK"));
        assert!(!body.contains("BACKEND_SK"));

        let (status, body) = catalog_body(
            app.oneshot(catalog_request(
                "/api/v1/catalog/datasets",
                Some("USER_AK"),
                Some("USER_SK"),
            ))
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(body.contains("\"name\":\"prod\""));
        assert!(!body.contains("BACKEND_AK"));
        assert!(!body.contains("BACKEND_SK"));
        assert!(!body.contains("access_key"));
        assert!(!body.contains("secret_key"));
    }

    #[tokio::test]
    async fn catalog_ticket_hides_unauthorized_names_as_not_found() {
        use tower::ServiceExt;

        let app = catalog_front().await;
        let (status, body) = catalog_body(
            app.clone()
                .oneshot(catalog_request(
                    "/api/v1/catalog/datasets/prod",
                    Some("USER_AK"),
                    Some("USER_SK"),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(body.contains("BACKEND_SK"));
        assert!(body.contains("BACKEND_AK"));

        let (status, _) = catalog_body(
            app.clone()
                .oneshot(catalog_request(
                    "/api/v1/catalog/datasets/prod",
                    Some("BOB_AK"),
                    Some("BOB_SK"),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

        let (status, _) = catalog_body(
            app.oneshot(catalog_request(
                "/api/v1/catalog/datasets/missing",
                Some("USER_AK"),
                Some("USER_SK"),
            ))
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn catalog_data_plane_requires_headers_without_spawning_worker() {
        use tower::ServiceExt;

        let app = catalog_front().await;
        let (status, _) = catalog_body(
            app.clone()
                .oneshot(catalog_request("/api/query/tables", None, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

        let (status, _) = catalog_body(
            app.oneshot(catalog_request("/api/health", None, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
    }
}

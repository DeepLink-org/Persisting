//! Exec workers have an immutable authenticated scope. No storage client or
//! runtime is inherited from the listening process.
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, Semaphore};
use tower::ServiceExt;

use super::{
    catalog::{CatalogLibrary, apply_library_env},
    problem::ApiError,
};

const MAX_WORKERS: usize = 8;
const MAX_REQUESTS: usize = 32;
const FRAME_LIMIT: usize = 40 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Serialize, Deserialize)]
pub(super) struct WorkerRequest {
    pub method: String,
    pub uri: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct WorkerResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl WorkerResponse {
    pub(super) fn into_response(self) -> Result<axum::response::Response> {
        let mut response = axum::response::Response::new(axum::body::Body::from(self.body));
        *response.status_mut() = axum::http::StatusCode::from_u16(self.status)?;
        for (name, value) in self.headers {
            response
                .headers_mut()
                .append(name.parse::<axum::http::HeaderName>()?, value.parse()?);
        }
        Ok(response)
    }
}

#[derive(Serialize, Deserialize)]
struct Bootstrap {
    mounts: Vec<CatalogLibrary>,
}

type Slot = Arc<Mutex<Option<Worker>>>;

pub(super) struct WorkerPool {
    slots: Mutex<HashMap<String, Slot>>,
    requests: Semaphore,
}

impl Default for WorkerPool {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            requests: Semaphore::new(MAX_REQUESTS),
        }
    }
}

impl WorkerPool {
    pub(super) fn admit(&self) -> Result<tokio::sync::SemaphorePermit<'_>, ApiError> {
        self.requests
            .try_acquire()
            .map_err(|_| ApiError::unavailable())
    }

    async fn slot(&self, scope: &str) -> Result<Slot, ApiError> {
        let mut slots = self.slots.lock().await;
        if let Some(slot) = slots.get(scope) {
            return Ok(slot.clone());
        }
        if slots.len() >= MAX_WORKERS {
            // Only evict a worker with no in-flight or queued request. Wait for
            // its exit before spawning a replacement, keeping the process cap.
            let idle = slots
                .iter()
                .find(|(_, slot)| Arc::strong_count(slot) == 1)
                .map(|(key, _)| key.clone());
            let Some(idle) = idle else {
                return Err(ApiError::unavailable());
            };
            if let Some(slot) = slots.remove(&idle)
                && let Some(mut worker) = slot.lock().await.take()
            {
                let _ = worker.child.kill().await;
            }
        }
        let slot = Arc::new(Mutex::new(None));
        slots.insert(scope.to_owned(), slot.clone());
        Ok(slot)
    }

    pub(super) async fn execute(
        &self,
        scope: String,
        mounts: Vec<CatalogLibrary>,
        request: WorkerRequest,
    ) -> Result<axum::response::Response, ApiError> {
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            let slot = self.slot(&scope).await?;
            let mut guard = slot.lock().await;
            // Ownership stays in this future during IPC: cancellation, timeout
            // or a partial frame drops/kills it rather than reusing dirty pipes.
            let mut worker = match guard.take() {
                Some(worker) => worker,
                None => Worker::start(&scope, mounts).await.map_err(worker_error)?,
            };
            let response = worker.exchange(&request).await.map_err(worker_error)?;
            let response = response.into_response().map_err(worker_error)?;
            *guard = Some(worker);
            Ok(response)
        })
        .await
        .map_err(|_| ApiError::unavailable())?
    }
}

fn worker_error(error: anyhow::Error) -> ApiError {
    // Protocol/OS diagnostics only; never log bootstrap payloads or child stderr.
    ApiError::internal("", "catalog_worker", error)
}

struct Worker {
    child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: tokio::process::ChildStdout,
    _home: tempfile::TempDir,
}

fn command(
    exe: PathBuf,
    home: &std::path::Path,
    cache: &std::path::Path,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(exe);
    command
        .arg("serve")
        .arg("--catalog-query-worker")
        .env_clear()
        .current_dir(home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home)
        .env("XDG_CACHE_HOME", home)
        .env("PCHRONICLE_CACHE_DIR", cache)
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .env("RAYON_NUM_THREADS", "2")
        .env("AWS_CONFIG_FILE", home.join("no-aws-config"))
        .env(
            "AWS_SHARED_CREDENTIALS_FILE",
            home.join("no-aws-credentials"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Keep worker diagnostics visible to the serve process. Catalog UI
        // requests run in this child, so hiding stderr also hid cache refresh
        // failures and made a stalled browse impossible to diagnose.
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    // Retain only platform essentials and explicitly configured trust roots.
    for key in [
        "PATH",
        "SystemRoot",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "PCHRONICLE_QUERY_MEMORY_LIMIT",
        "RUST_LOG",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

impl Worker {
    async fn start(scope: &str, mounts: Vec<CatalogLibrary>) -> Result<Self> {
        let home = tempfile::tempdir()?;
        let root = std::env::var_os("PCHRONICLE_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::cache_dir().map(|p| p.join("pchronicle")))
            .context("no catalog worker cache directory")?;
        let cache = std::path::absolute(root)?.join("workers").join(scope);
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&cache)?;
        let mut child = command(std::env::current_exe()?, home.path(), &cache).spawn()?;
        let input = child.stdin.take().context("worker stdin missing")?;
        let output = child.stdout.take().context("worker stdout missing")?;
        let mut worker = Self {
            child,
            input,
            output,
            _home: home,
        };
        worker.send(&Bootstrap { mounts }).await?;
        let ready: bool = worker.receive().await?;
        anyhow::ensure!(ready, "catalog worker failed to initialize");
        Ok(worker)
    }

    async fn send(&mut self, message: &impl Serialize) -> Result<()> {
        let bytes = encode(message)?;
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        Ok(())
    }

    async fn receive<T: DeserializeOwned>(&mut self) -> Result<T> {
        let size = self.output.read_u32().await? as usize;
        anyhow::ensure!(size <= FRAME_LIMIT, "worker frame too large");
        let mut bytes = vec![0; size];
        self.output.read_exact(&mut bytes).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn exchange(&mut self, request: &WorkerRequest) -> Result<WorkerResponse> {
        self.send(request).await?;
        self.receive().await
    }
}

fn encode(message: &impl Serialize) -> Result<Vec<u8>> {
    let payload = serde_json::to_vec(message)?;
    anyhow::ensure!(payload.len() <= FRAME_LIMIT, "worker frame too large");
    let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
    bytes.extend(payload);
    Ok(bytes)
}

fn read_frame<T: DeserializeOwned>(input: &mut impl Read) -> Result<Option<T>> {
    let mut size = [0; 4];
    if input.read(&mut size[..1])? == 0 {
        return Ok(None);
    }
    input.read_exact(&mut size[1..])?;
    let size = u32::from_be_bytes(size) as usize;
    anyhow::ensure!(size <= FRAME_LIMIT, "worker frame too large");
    let mut bytes = vec![0; size];
    input.read_exact(&mut bytes)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

pub(super) fn validate_backends(mounts: &[CatalogLibrary]) -> Result<()> {
    anyhow::ensure!(!mounts.is_empty(), "catalog worker needs mounts");
    let mut backend = None;
    for library in mounts.iter().filter(|m| m.uri.starts_with("s3://")) {
        anyhow::ensure!(
            library.access_key.is_some() && library.secret_key.is_some(),
            "explicit S3 credentials required"
        );
        let identity = (
            &library.endpoint,
            &library.region,
            &library.access_key,
            &library.secret_key,
        );
        if let Some(previous) = backend {
            anyhow::ensure!(
                previous == identity,
                "datasets use different S3 credentials or endpoints; select a dataset explicitly"
            );
        }
        backend = Some(identity);
    }
    Ok(())
}

/// Called before main constructs any runtime or threads. Credentials arrive
/// only over stdin, and remain fixed for the lifetime of this process.
pub(crate) fn run() -> Result<()> {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let bootstrap: Bootstrap = read_frame(&mut input)?.context("missing worker bootstrap")?;
    validate_backends(&bootstrap.mounts)?;
    if let Some(library) = bootstrap.mounts.iter().find(|m| m.uri.starts_with("s3://")) {
        apply_library_env(library);
    }
    let mounts = bootstrap
        .mounts
        .iter()
        .map(|m| persisting_pchronicle::storage::DatasetMount::new(&m.name, &m.uri))
        .collect::<Result<Vec<_>>>()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(8)
        .enable_all()
        .build()?;
    let warehouse = runtime.block_on(super::PreparedWarehouse::prepare_query_worker(
        super::ChronicleServerConfig::mounted(mounts)?,
    ))?;
    output.write_all(&encode(&true)?)?;
    output.flush()?;
    while let Some(job) = read_frame::<WorkerRequest>(&mut input)? {
        anyhow::ensure!(
            job.body.len() <= 1024 * 1024,
            "worker request body too large"
        );
        let result = runtime.block_on(async {
            let mut builder = axum::http::Request::builder()
                .method(job.method.as_str())
                .uri(job.uri);
            for (name, value) in job.headers {
                builder = builder.header(name, value);
            }
            let response = warehouse
                .router()
                .oneshot(builder.body(axum::body::Body::from(job.body))?)
                .await?;
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .filter(|(name, _)| {
                    matches!(
                        name.as_str(),
                        "content-type"
                            | "server-timing"
                            | "x-request-id"
                            | "cache-control"
                            | "retry-after"
                            | "content-disposition"
                    )
                })
                .filter_map(|(name, value)| {
                    value
                        .to_str()
                        .ok()
                        .map(|v| (name.to_string(), v.to_owned()))
                })
                .collect();
            let body = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
                .await?
                .to_vec();
            Ok::<_, anyhow::Error>(WorkerResponse {
                status,
                headers,
                body,
            })
        })?;
        output.write_all(&encode(&result)?)?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_reject_truncation_and_oversize_and_preserve_boundaries() {
        let mut bytes = encode(&true).unwrap();
        bytes.extend(encode(&false).unwrap());
        let mut input = bytes.as_slice();
        assert_eq!(read_frame::<bool>(&mut input).unwrap(), Some(true));
        assert_eq!(read_frame::<bool>(&mut input).unwrap(), Some(false));
        assert_eq!(read_frame::<bool>(&mut input).unwrap(), None);
        assert!(read_frame::<bool>(&mut &bytes[..3]).is_err());
        assert!(read_frame::<bool>(&mut &((FRAME_LIMIT + 1) as u32).to_be_bytes()[..]).is_err());
    }

    #[tokio::test]
    async fn pool_bounds_admission_and_never_evicts_queued_scopes() {
        let pool = WorkerPool::default();
        let permits: Vec<_> = (0..MAX_REQUESTS).map(|_| pool.admit().unwrap()).collect();
        assert!(pool.admit().is_err());
        drop(permits);
        assert!(pool.admit().is_ok());
        let mut slots = Vec::new();
        for index in 0..MAX_WORKERS {
            slots.push(pool.slot(&index.to_string()).await.unwrap());
        }
        assert!(Arc::ptr_eq(&slots[0], &pool.slot("0").await.unwrap()));
        assert!(pool.slot("overflow").await.is_err());
        slots.remove(0);
        assert!(pool.slot("replacement").await.is_ok());
        assert_eq!(pool.slots.lock().await.len(), MAX_WORKERS);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_ipc_kills_worker_and_clears_slot() {
        let home = tempfile::tempdir().unwrap();
        let mut child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap() as i32;
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let pool = WorkerPool::default();
        let slot = pool.slot("test").await.unwrap();
        *slot.lock().await = Some(Worker {
            child,
            input,
            output,
            _home: home,
        });
        let result = tokio::time::timeout(
            Duration::from_millis(30),
            pool.execute(
                "test".into(),
                Vec::new(),
                WorkerRequest {
                    method: "GET".into(),
                    uri: "/api/health".into(),
                    headers: Vec::new(),
                    body: Vec::new(),
                },
            ),
        )
        .await;
        assert!(result.is_err());
        assert!(slot.lock().await.is_none());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                // Signal zero observes process existence without sending a signal.
                if unsafe { libc::kill(pid, 0) } == -1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn exec_environment_excludes_ambient_credentials() {
        let home = tempfile::tempdir().unwrap();
        let cmd = command(PathBuf::from("pchronicle"), home.path(), home.path());
        let env: HashMap<_, _> = cmd.as_std().get_envs().collect();
        for key in [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_PROFILE",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        ] {
            assert!(!env.contains_key(std::ffi::OsStr::new(key)));
        }
        assert_eq!(
            env[std::ffi::OsStr::new("HOME")],
            Some(home.path().as_os_str())
        );
        assert_eq!(
            env[std::ffi::OsStr::new("AWS_EC2_METADATA_DISABLED")],
            Some(std::ffi::OsStr::new("true"))
        );
    }

    #[test]
    fn mixed_s3_credentials_are_rejected_instead_of_using_first_key() {
        let library = CatalogLibrary {
            name: "one".into(),
            uri: "s3://bucket/one".into(),
            endpoint: None,
            region: None,
            access_key: Some("ak".into()),
            secret_key: Some("sk".into()),
        };
        let mut other = library.clone();
        other.name = "two".into();
        other.uri = "s3://bucket/two".into();
        assert!(validate_backends(&[library.clone(), other.clone()]).is_ok());
        other.secret_key = Some("another-secret".into());
        let error = validate_backends(&[library, other])
            .unwrap_err()
            .to_string();
        assert!(!error.contains("another-secret"));
        assert!(error.contains("select a dataset"));
    }
}

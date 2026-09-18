//! Exec workers have an immutable authenticated scope. No storage client or
//! runtime is inherited from the listening process.
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Once},
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, Notify, OwnedMutexGuard, Semaphore};
use tokio::time::Instant;
use tower::ServiceExt;

use super::{
    catalog::{CatalogLibrary, apply_library_env},
    problem::{ApiError, ExecutionStage},
};

const MAX_WORKERS: usize = 8;
const MAX_REQUESTS: usize = 32;
const MAX_WORKERS_PER_SCOPE: usize = 4;
const WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const REAP_INTERVAL: Duration = Duration::from_secs(30);
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
#[serde(tag = "type", content = "value")]
enum WorkerEvent {
    Progress(super::request_progress::Snapshot),
    Response(WorkerResponse),
}

#[derive(Serialize, Deserialize)]
struct Bootstrap {
    mounts: Vec<CatalogLibrary>,
}

struct SlotState {
    worker: Option<Worker>,
    idle_since: Instant,
}

struct Slot {
    scope: String,
    state: Arc<Mutex<SlotState>>,
}

#[derive(Default)]
struct PoolState {
    slots: Mutex<Vec<Slot>>,
    available: Arc<Notify>,
}

// A reserved slot is never visible as idle, including while its child starts.
// Drop wakes all scopes: a released slot can satisfy a different scope by
// eviction even if the first waiter has reached its per-scope limit.
struct Lease {
    guard: Option<OwnedMutexGuard<SlotState>>,
    available: Arc<Notify>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(mut guard) = self.guard.take() {
            guard.idle_since = Instant::now();
            drop(guard);
        }
        self.available.notify_waiters();
    }
}

impl PoolState {
    async fn try_lease(&self, scope: &str) -> Result<Option<Lease>, ApiError> {
        let mut slots = self.slots.lock().await;
        for slot in slots.iter().filter(|slot| slot.scope == scope) {
            if let Ok(guard) = slot.state.clone().try_lock_owned() {
                return Ok(Some(Lease {
                    guard: Some(guard),
                    available: self.available.clone(),
                }));
            }
        }
        if slots.iter().filter(|slot| slot.scope == scope).count() >= MAX_WORKERS_PER_SCOPE {
            return Ok(None);
        }
        if slots.len() >= MAX_WORKERS {
            // Never enqueue on a busy slot. Reclaim only an idle child from
            // another scope, and wait for its exit before reusing its capacity.
            let idle = slots
                .iter()
                .enumerate()
                .filter_map(|(index, slot)| {
                    slot.state
                        .clone()
                        .try_lock_owned()
                        .ok()
                        .map(|guard| (index, guard))
                })
                .min_by_key(|(_, guard)| guard.idle_since);
            let Some((index, mut guard)) = idle else {
                return Ok(None);
            };
            if let Some(worker) = guard.worker.as_mut() {
                worker
                    .child
                    .kill()
                    .await
                    .map_err(|error| worker_error(error.into()))?;
            }
            slots.remove(index);
        }
        let state = Arc::new(Mutex::new(SlotState {
            worker: None,
            idle_since: Instant::now(),
        }));
        let guard = state
            .clone()
            .try_lock_owned()
            .expect("new worker slot is idle");
        slots.push(Slot {
            scope: scope.to_owned(),
            state,
        });
        Ok(Some(Lease {
            guard: Some(guard),
            available: self.available.clone(),
        }))
    }

    async fn reap_idle(&self) {
        let mut slots = self.slots.lock().await;
        for index in (0..slots.len()).rev() {
            let Ok(mut guard) = slots[index].state.clone().try_lock_owned() else {
                continue;
            };
            if guard.idle_since.elapsed() < WORKER_IDLE_TIMEOUT {
                continue;
            }
            if let Some(worker) = guard.worker.as_mut() {
                if worker.child.kill().await.is_err() {
                    continue;
                }
            }
            slots.remove(index);
        }
        self.available.notify_waiters();
    }
}

pub(super) struct WorkerPool {
    state: Arc<PoolState>,
    requests: Semaphore,
    reaper: Once,
}

impl Default for WorkerPool {
    fn default() -> Self {
        Self {
            state: Arc::new(PoolState::default()),
            requests: Semaphore::new(MAX_REQUESTS),
            reaper: Once::new(),
        }
    }
}

impl WorkerPool {
    pub(super) fn admit(&self) -> Result<tokio::sync::SemaphorePermit<'_>, ApiError> {
        self.requests
            .try_acquire()
            .map_err(|_| ApiError::unavailable().with_stage(ExecutionStage::Admission))
    }

    async fn lease(&self, scope: &str) -> Result<Lease, ApiError> {
        self.reaper.call_once(|| {
            let state = Arc::downgrade(&self.state);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(REAP_INTERVAL).await;
                    let Some(state) = state.upgrade() else {
                        break;
                    };
                    state.reap_idle().await;
                }
            });
        });
        loop {
            // Register before checking capacity so completion cannot be missed
            // between a failed checkout and going to sleep.
            let ready = self.state.available.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            if let Some(lease) = self.state.try_lease(scope).await? {
                return Ok(lease);
            }
            ready.await;
        }
    }

    pub(super) async fn execute(
        &self,
        scope: String,
        mounts: Vec<CatalogLibrary>,
        request: WorkerRequest,
    ) -> Result<axum::response::Response, ApiError> {
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            super::request_progress::phase("worker_queue");
            let mut lease = self.lease(&scope).await?;
            let guard = lease.guard.as_mut().expect("reserved worker slot");
            // Ownership stays in this future during IPC: cancellation, timeout
            // or a partial frame drops/kills it rather than reusing dirty pipes.
            super::request_progress::phase("worker_start");
            let mut worker = match guard.worker.take() {
                Some(worker) => worker,
                None => Worker::start(&scope, mounts).await.map_err(worker_error)?,
            };
            super::request_progress::phase("worker_execution");
            let response = worker.exchange(&request).await.map_err(worker_error)?;
            let response = response.into_response().map_err(worker_error)?;
            guard.worker = Some(worker);
            Ok(response)
        })
        .await
        .map_err(|_| ApiError::unavailable().with_stage(ExecutionStage::Query))?
    }
}

fn worker_error(error: anyhow::Error) -> ApiError {
    // Protocol/OS diagnostics only; never log bootstrap payloads or child stderr.
    ApiError::internal("", "catalog_worker", error).with_stage(ExecutionStage::Worker)
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
    blocks: &std::path::Path,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(exe);
    command
        .arg("--log-level")
        .arg(super::request_log::initialized_log_level().as_arg())
        .arg("serve")
        .arg("--catalog-query-worker")
        .env_clear()
        .current_dir(home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home)
        .env("XDG_CACHE_HOME", home)
        .env("PCHRONICLE_CACHE_DIR", cache)
        // `env_clear` plus a throwaway HOME makes the Lance block cache resolve
        // under a directory that dies with the worker, so every worker refetched
        // the same index pages from the object store. Name it explicitly.
        .env("PCHRONICLE_LANCE_CACHE_DIR", blocks)
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
        "PCHRONICLE_LANCE_CACHE_CAPACITY_BYTES",
        // Workers issue the reads, so admission tuning that never reaches them
        // tunes nothing.
        "PCHRONICLE_OBJECT_STORE_CONCURRENCY",
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
        let root = std::path::absolute(root)?;
        let cache = root.join("workers").join(scope);
        // Blocks are keyed by store, object version and size, so every worker
        // and every scope can share them. Keeping them beside the per-scope
        // caches rather than inside one means a reader does not refetch what
        // another worker already paid for.
        let blocks = root.join("blocks");
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&cache)?;
        builder.create(&blocks)?;
        let mut child =
            command(std::env::current_exe()?, home.path(), &cache, &blocks).spawn()?;
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
        loop {
            match self.receive::<WorkerEvent>().await? {
                WorkerEvent::Progress(snapshot) => {
                    if let Some(p) = super::request_progress::current() {
                        p.worker(snapshot);
                    }
                }
                WorkerEvent::Response(response) => return Ok(response),
            }
        }
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
pub(crate) fn run(level: crate::LogLevel) -> Result<()> {
    // Handlers execute here, so `ApiError::internal` emits its `root_cause`
    // line in this process. Without a subscriber the inherited stderr stayed
    // empty and every worker-side failure reached the browser as a bare 500.
    super::request_log::init_warehouse_tracing(level);
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
        let id = job
            .headers
            .iter()
            .find(|(name, _)| name == "x-request-id")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let progress = super::request_progress::Progress::new(
            id,
            job.method.clone(),
            job.uri.split('?').next().unwrap_or_default().to_owned(),
            true,
        );
        let result = runtime.block_on(async {
            let operation = async {
            let mut builder = axum::http::Request::builder()
                .method(job.method.as_str())
                .uri(job.uri);
            for (name, value) in job.headers {
                builder = builder.header(name, value);
            }
            let response = warehouse
                .router()
                .oneshot({ let mut request=builder.body(axum::body::Body::from(job.body))?;
                    request.extensions_mut().insert(progress.clone()); request })
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
            };
            tokio::pin!(operation);
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            loop {
                tokio::select! {
                    result = &mut operation => break result,
                    _ = tick.tick() => { output.write_all(&encode(&WorkerEvent::Progress(progress.snapshot()))?)?; output.flush()?; }
                }
            }
        })?;
        output.write_all(&encode(&WorkerEvent::Progress(progress.snapshot()))?)?;
        output.write_all(&encode(&WorkerEvent::Response(result))?)?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
    async fn pool_scales_reuses_bounds_and_wakes_waiters() {
        let pool = WorkerPool::default();
        let permits: Vec<_> = (0..MAX_REQUESTS).map(|_| pool.admit().unwrap()).collect();
        assert!(pool.admit().is_err());
        drop(permits);
        assert!(pool.admit().is_ok());

        let first = pool.lease("same").await.unwrap();
        let second = pool.lease("same").await.unwrap();
        assert_eq!(pool.state.slots.lock().await.len(), 2);
        let first_slot = OwnedMutexGuard::mutex(first.guard.as_ref().unwrap()).clone();
        drop(first);
        let reused = pool.lease("same").await.unwrap();
        assert!(Arc::ptr_eq(
            &first_slot,
            OwnedMutexGuard::mutex(reused.guard.as_ref().unwrap())
        ));
        let mut busy = vec![second, reused];
        for _ in busy.len()..MAX_WORKERS_PER_SCOPE {
            busy.push(pool.lease("same").await.unwrap());
        }
        assert!(pool.state.try_lease("same").await.unwrap().is_none());
        for index in MAX_WORKERS_PER_SCOPE..MAX_WORKERS {
            busy.push(pool.lease(&format!("other-{index}")).await.unwrap());
        }
        assert!(pool.state.try_lease("overflow").await.unwrap().is_none());
        let waiting = pool.lease("same");
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        busy.remove(0);
        let lease = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pool.state.slots.lock().await.len(), MAX_WORKERS);
        drop(lease);
        let replacement = pool.lease("replacement").await.unwrap();
        assert_eq!(pool.state.slots.lock().await.len(), MAX_WORKERS);
        drop(replacement);
    }

    #[tokio::test]
    async fn reaper_removes_only_idle_expired_slots() {
        let pool = WorkerPool::default();
        let mut busy = pool.lease("scope").await.unwrap();
        busy.guard.as_mut().unwrap().idle_since = Instant::now() - WORKER_IDLE_TIMEOUT;
        let idle = pool.lease("scope").await.unwrap();
        drop(idle);
        {
            let slots = pool.state.slots.lock().await;
            slots[1].state.lock().await.idle_since = Instant::now() - WORKER_IDLE_TIMEOUT;
        }
        pool.state.reap_idle().await;
        assert_eq!(pool.state.slots.lock().await.len(), 1);
        drop(busy);
        pool.state.reap_idle().await;
        assert_eq!(pool.state.slots.lock().await.len(), 1);
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
        let mut lease = pool.lease("test").await.unwrap();
        lease.guard.as_mut().unwrap().worker = Some(Worker {
            child,
            input,
            output,
            _home: home,
        });
        drop(lease);
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
        let lease = pool.lease("test").await.unwrap();
        assert!(lease.guard.as_ref().unwrap().worker.is_none());
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
        let cmd = command(
            PathBuf::from("pchronicle"),
            home.path(),
            home.path(),
            home.path(),
        );
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
    fn exec_environment_names_a_surviving_block_cache() {
        let home = tempfile::tempdir().unwrap();
        let blocks = tempfile::tempdir().unwrap();
        let cmd = command(
            PathBuf::from("pchronicle"),
            home.path(),
            home.path(),
            blocks.path(),
        );
        let env: HashMap<_, _> = cmd.as_std().get_envs().collect();
        // Without this the cache resolves under the worker's throwaway HOME, so
        // each worker refetches every index page the last one already read.
        assert_eq!(
            env[std::ffi::OsStr::new("PCHRONICLE_LANCE_CACHE_DIR")],
            Some(blocks.path().as_os_str())
        );
        assert_ne!(
            env[std::ffi::OsStr::new("PCHRONICLE_LANCE_CACHE_DIR")],
            Some(home.path().as_os_str())
        );
    }

    #[test]
    fn exec_environment_forwards_object_store_admission_tuning() {
        let home = tempfile::tempdir().unwrap();
        // Workers issue the object-store reads, and `command` clears the
        // environment. A knob missing from the allowlist silently tunes only
        // the parent, which reads almost nothing.
        // SAFETY: single-threaded test asserting how `command` forwards it.
        unsafe { std::env::set_var("PCHRONICLE_OBJECT_STORE_CONCURRENCY", "8") };
        let cmd = command(
            PathBuf::from("pchronicle"),
            home.path(),
            home.path(),
            home.path(),
        );
        let env: HashMap<_, _> = cmd.as_std().get_envs().collect();
        assert_eq!(
            env[std::ffi::OsStr::new("PCHRONICLE_OBJECT_STORE_CONCURRENCY")],
            Some(std::ffi::OsStr::new("8"))
        );
        unsafe { std::env::remove_var("PCHRONICLE_OBJECT_STORE_CONCURRENCY") };
    }

    #[test]
    fn exec_arguments_carry_the_serve_log_level() {
        let home = tempfile::tempdir().unwrap();
        let cmd = command(
            PathBuf::from("pchronicle"),
            home.path(),
            home.path(),
            home.path(),
        );
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        // Handlers run in the child, so it must install a subscriber at the
        // serve level; otherwise `root_cause` diagnostics are dropped there.
        let level = args
            .iter()
            .position(|arg| arg == "--log-level")
            .map(|index| args[index + 1].clone());
        assert_eq!(
            level.as_deref(),
            Some(super::super::request_log::initialized_log_level().as_arg()),
            "{args:?}"
        );
        assert!(
            args.contains(&"--catalog-query-worker".to_owned()),
            "{args:?}"
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

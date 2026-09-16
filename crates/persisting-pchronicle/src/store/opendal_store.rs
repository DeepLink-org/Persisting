//! Small OpenDAL facade for pChronicle's control-plane objects.
//!
//! Lance still owns its internal storage bridge for opening datasets. This
//! module keeps pChronicle's own reads, listings and conditional writes on
//! OpenDAL so backend differences are handled in one place.

use crate::store::object_store_io_gate::{self as io_gate, IoKind};
use anyhow::{Context, Result, anyhow};
use futures::TryStreamExt;
use opendal::layers::RetryLayer;
use opendal::{EntryMode, ErrorKind, Metadata, Operator};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use url::Url;

#[derive(Clone, Default, PartialEq, Eq)]
pub struct StoreConfig {
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
}

impl std::fmt::Debug for StoreConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreConfig")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

impl StoreConfig {
    pub(crate) fn fingerprint(&self) -> String {
        blake3::hash(
            &serde_json::to_vec(&(
                &self.endpoint,
                &self.region,
                &self.access_key,
                &self.secret_key,
            ))
            .expect("serialize S3 configuration"),
        )
        .to_hex()
        .to_string()
    }
}

/// Retries for transient object-store failures (DNS blips, connect resets,
/// 5xx, rate limits). Tuned for long imports over flaky endpoints: up to 8
/// retries with exponential backoff + jitter, capped at 30s.
fn with_object_store_retries(operator: Operator) -> Operator {
    operator.layer(
        RetryLayer::new()
            .with_notify(|event: opendal::layers::RetryEvent<'_>| {
                tracing::warn!(
                    target: "pchronicle.opendal",
                    attempt = event.attempt,
                    retry_after_ms = event.retry_after.as_millis() as u64,
                    op = ?event.op,
                    error = %event.err,
                    "retrying temporary object-store error"
                );
            })
            .with_jitter()
            .with_factor(2.0)
            .with_min_delay(Duration::from_millis(500))
            .with_max_delay(Duration::from_secs(30))
            .with_max_times(8),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Version {
    pub(crate) etag: Option<String>,
    pub(crate) version: Option<String>,
}

impl Version {
    pub(crate) fn condition(&self) -> Option<&str> {
        self.etag.as_deref().or(self.version.as_deref())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Store {
    operator: Operator,
    fallback_lock: Option<Arc<tokio::sync::Mutex<()>>>,
    io_scope: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) path: String,
    pub(crate) metadata: Metadata,
}

#[derive(Clone, Debug)]
pub(crate) struct ShallowEntry {
    pub(crate) path: String,
    pub(crate) mode: EntryMode,
    pub(crate) metadata: Metadata,
}

static SHARED_MEMORY: OnceLock<Mutex<HashMap<String, Operator>>> = OnceLock::new();
static SHARED_LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

const MAX_CACHED_OPERATORS: usize = 128;
const OPERATOR_IDLE_TTL: Duration = Duration::from_secs(300);

#[derive(Default)]
struct OperatorRegistry {
    entries: HashMap<String, (Operator, Instant)>,
}

impl OperatorRegistry {
    fn get(&mut self, key: &str, now: Instant) -> Option<Operator> {
        self.entries
            .retain(|_, (_, used)| now.duration_since(*used) < OPERATOR_IDLE_TTL);
        self.entries.get_mut(key).map(|(operator, used)| {
            *used = now;
            operator.clone()
        })
    }

    fn insert(&mut self, key: String, operator: Operator, now: Instant) {
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_CACHED_OPERATORS {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| key.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key, (operator, now));
    }
}

static OPERATORS: OnceLock<Mutex<OperatorRegistry>> = OnceLock::new();

impl Store {
    pub(crate) async fn from_uri_with_config(uri: &str, config: StoreConfig) -> Result<Self> {
        if !uri.starts_with("s3://") || config == StoreConfig::default() {
            return Self::from_uri(uri).await;
        }
        let parsed = Url::parse(uri).context("parse object-store URI")?;
        let bucket = parsed
            .host_str()
            .ok_or_else(|| anyhow!("S3 URI must name a bucket"))?;
        let root = parsed.path().trim_matches('/');
        let io_scope = io_gate::scope_for_endpoint(uri, config.endpoint.as_deref().unwrap_or(""));
        let cache_key = format!(
            "{}\0{}",
            operator_cache_key(uri, parsed.as_str()),
            config.fingerprint()
        );
        let registry = OPERATORS.get_or_init(|| Mutex::new(OperatorRegistry::default()));
        let cached = registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&cache_key, Instant::now());
        let operator = if let Some(operator) = cached {
            operator
        } else {
            let mut builder = opendal::services::S3::default()
                .bucket(bucket)
                .root(root)
                .region(config.region.as_deref().unwrap_or("us-east-1"));
            if let Some(v) = config.endpoint.as_deref() {
                builder = builder.endpoint(v);
            }
            if let Some(v) = config.access_key.as_deref() {
                builder = builder.access_key_id(v);
            }
            if let Some(v) = config.secret_key.as_deref() {
                builder = builder.secret_access_key(v);
            }
            let operator = with_object_store_retries(Operator::new(builder)?.finish());
            registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(cache_key, operator.clone(), Instant::now());
            operator
        };
        Ok(Self {
            operator,
            fallback_lock: None,
            io_scope,
        })
    }

    pub(crate) async fn from_uri(uri: &str) -> Result<Self> {
        let uri = uri.trim();
        let normalized = normalize_uri(uri)?;
        let cache_key = operator_cache_key(uri, &normalized);
        let shared_memory = uri.contains("://")
            && Url::parse(uri)
                .map(|parsed| parsed.scheme() == "shared-memory")
                .unwrap_or(false);
        // Memory operators own the data itself and must not be evicted like clients.
        let operator = if normalized.starts_with("memory://") {
            let mut map = SHARED_MEMORY
                .get_or_init(|| Mutex::new(HashMap::new()))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(operator) = map.get(uri) {
                operator.clone()
            } else {
                let operator = with_object_store_retries(Operator::from_uri(normalized.as_str())?);
                map.insert(uri.to_owned(), operator.clone());
                operator
            }
        } else {
            let registry = OPERATORS.get_or_init(|| Mutex::new(OperatorRegistry::default()));
            let cached = registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&cache_key, Instant::now());
            if let Some(operator) = cached {
                operator
            } else {
                // Construct outside the registry lock so one backend cannot block all others.
                let operator = with_object_store_retries(
                    Operator::from_uri(normalized.as_str())
                        .with_context(|| format!("open OpenDAL store {uri}"))?,
                );
                registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(cache_key, operator.clone(), Instant::now());
                operator
            }
        };
        let fallback_lock = if shared_memory {
            let locks = SHARED_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
            let mut locks = locks
                .lock()
                .map_err(|_| anyhow!("shared-memory lock registry poisoned"))?;
            Some(
                locks
                    .entry(uri.to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                    .clone(),
            )
        } else {
            None
        };
        Ok(Self {
            operator,
            fallback_lock,
            io_scope: io_gate::scope_key(uri),
        })
    }

    async fn remote<T, F, Fut>(&self, kind: IoKind, request: F) -> Result<T>
    where
        F: FnOnce(Operator) -> Fut,
        Fut: std::future::IntoFuture<Output = opendal::Result<T>>,
    {
        let _permit = io_gate::acquire(&self.io_scope, kind).await;
        match request(self.operator.clone()).into_future().await {
            Ok(value) => {
                io_gate::note_success(&self.io_scope);
                Ok(value)
            }
            Err(error) => {
                if is_transient_error(&error) {
                    tracing::warn!(target: "pchronicle.opendal",
                        scope = %self.io_scope, kind = kind.as_str(), error = %error,
                        "remote operation failed; applying shared cooldown");
                    io_gate::note_failure(&self.io_scope, kind);
                }
                Err(error.into())
            }
        }
    }

    pub(crate) async fn read(&self, path: &str) -> Result<Option<(Vec<u8>, Version)>> {
        let path_owned = path.to_owned();
        let metadata = match self
            .remote(IoKind::Read, |operator| async move {
                operator.stat(&path_owned).await
            })
            .await
        {
            Ok(metadata) => metadata,
            Err(error)
                if error
                    .downcast_ref::<opendal::Error>()
                    .is_some_and(|error| error.kind() == ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let path_owned = path.to_owned();
        let bytes = self
            .remote(IoKind::Read, |operator| async move {
                operator.read(&path_owned).await
            })
            .await?
            .to_vec();
        Ok(Some((bytes, version(&metadata))))
    }

    pub(crate) async fn write_create(&self, path: &str, bytes: Vec<u8>) -> Result<()> {
        let path = path.to_owned();
        self.remote(IoKind::Write, |operator| async move {
            operator.write_with(&path, bytes).if_not_exists(true).await
        })
        .await
        .map(|_| ())
    }

    pub(crate) async fn write_match(
        &self,
        path: &str,
        bytes: Vec<u8>,
        expected: &Version,
    ) -> Result<()> {
        if self.fallback_lock.is_some() {
            // ponytail: the in-process shared-memory test backend has no CAS
            // primitive; callers hold its per-root mutex for the full mutation.
            return self.write_overwrite(path, bytes).await;
        }
        let condition = expected.condition().ok_or_else(|| {
            anyhow!("OpenDAL backend did not return an ETag/version for conditional write")
        })?;
        let path = path.to_owned();
        let condition = condition.to_owned();
        let log_path = path.clone();
        let log_condition = condition.clone();
        let result = self
            .remote(IoKind::Write, |operator| async move {
                let result = operator
                    .write_with(&path, bytes.clone())
                    .if_match(&condition)
                    .await;
                match result {
                    Ok(_) => Ok(()),
                    // Some S3-compatible gateways compare the If-Match header against
                    // their unquoted ETag, so retry once with the compatibility form.
                    Err(error)
                        if error.kind() == ErrorKind::ConditionNotMatch
                            && let Some(unquoted) = unquoted_etag(&condition) =>
                    {
                        match operator.write_with(&path, bytes).if_match(unquoted).await {
                            Ok(_) => Ok(()),
                            Err(retry_error)
                                if retry_error.kind() != ErrorKind::ConditionNotMatch =>
                            {
                                Err(error)
                            }
                            Err(retry_error) => Err(retry_error),
                        }
                    }
                    Err(error) => Err(error),
                }
            })
            .await;
        result.map_err(|error| {
            if let Some(error) = error.downcast_ref::<opendal::Error>()
                && is_conflict(error)
            {
                tracing::debug!(
                    target: "pchronicle.opendal",
                    path = log_path,
                    if_match = log_condition,
                    error = %error,
                    kind = ?error.kind(),
                    "conditional object write conflict (If-Match)"
                );
            }
            error.into()
        })
    }

    pub(crate) async fn write_overwrite(&self, path: &str, bytes: Vec<u8>) -> Result<()> {
        let path = path.to_owned();
        self.remote(IoKind::Write, |operator| async move {
            operator.write(&path, bytes).await
        })
        .await
        .map(|_| ())
    }

    pub(crate) async fn list(&self, prefix: &str) -> Result<Vec<Entry>> {
        let prefix = prefix.to_owned();
        self.remote(IoKind::Read, |operator| async move {
            let mut lister = operator.lister_with(&prefix).recursive(true).await?;
            let mut entries = Vec::new();
            while let Some(entry) = lister.try_next().await? {
                if entry.metadata().mode() == EntryMode::FILE {
                    entries.push(Entry {
                        path: entry.path().to_string(),
                        metadata: entry.metadata().clone(),
                    });
                }
            }
            Ok(entries)
        })
        .await
    }

    /// Non-recursive listing of the immediate children under `prefix`.
    /// Returns both files and directories so callers can navigate lazily.
    pub(crate) async fn list_shallow(&self, prefix: &str) -> Result<Vec<ShallowEntry>> {
        let prefix = prefix.to_owned();
        self.remote(IoKind::Read, |operator| async move {
            let mut lister = operator.lister_with(&prefix).recursive(false).await?;
            let mut entries = Vec::new();
            while let Some(entry) = lister.try_next().await? {
                entries.push(ShallowEntry {
                    path: entry.path().to_string(),
                    mode: entry.metadata().mode(),
                    metadata: entry.metadata().clone(),
                });
            }
            Ok(entries)
        })
        .await
    }

    pub(crate) async fn stat_file(&self, path: &str) -> Result<Option<Entry>> {
        let path_owned = path.to_owned();
        match self
            .remote(IoKind::Read, |operator| async move {
                operator.stat(&path_owned).await
            })
            .await
        {
            Ok(metadata) if metadata.mode() == EntryMode::FILE => Ok(Some(Entry {
                path: path.to_string(),
                metadata,
            })),
            Ok(_) => Ok(None),
            Err(error)
                if error
                    .downcast_ref::<opendal::Error>()
                    .is_some_and(|error| error.kind() == ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) async fn exists(&self) -> Result<bool> {
        self.remote(IoKind::Read, |operator| async move {
            Ok(operator
                .lister_with("")
                .recursive(true)
                .await?
                .try_next()
                .await?
                .is_some())
        })
        .await
    }

    pub(crate) async fn remove_all(&self) -> Result<()> {
        self.remote(IoKind::Write, |operator| async move {
            operator.delete_with("").recursive(true).await
        })
        .await
        .map(|_| ())
    }

    pub(crate) async fn remove(&self, path: &str) -> Result<()> {
        let path = path.to_owned();
        self.remote(IoKind::Write, |operator| async move {
            operator.delete_with(&path).recursive(true).await
        })
        .await
        .map(|_| ())
    }

    pub(crate) fn fallback_lock(&self) -> Option<Arc<tokio::sync::Mutex<()>>> {
        self.fallback_lock.clone()
    }
}

fn operator_cache_key(uri: &str, normalized: &str) -> String {
    let config: BTreeMap<_, _> = std::env::vars()
        .filter(|(key, _)| {
            key.starts_with("AWS_")
                || key.starts_with("AZURE_")
                || key.starts_with("GOOGLE_")
                || matches!(
                    key.as_str(),
                    "HTTP_PROXY" | "HTTPS_PROXY" | "NO_PROXY" | "ALL_PROXY"
                )
        })
        .collect();
    let fingerprint = blake3::hash(&serde_json::to_vec(&config).unwrap_or_default()).to_hex();
    format!("{uri}\0{normalized}\0{fingerprint}")
}

pub(crate) fn is_conflict(error: &opendal::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::AlreadyExists | ErrorKind::ConditionNotMatch
    )
}

/// Strip one pair of surrounding double quotes from a conditional-write ETag.
/// Returns `None` for unquoted or empty conditions.
fn unquoted_etag(condition: &str) -> Option<&str> {
    let inner = condition.strip_prefix('"')?.strip_suffix('"')?;
    (!inner.is_empty()).then_some(inner)
}

pub(crate) fn version(metadata: &Metadata) -> Version {
    Version {
        etag: metadata.etag().map(ToOwned::to_owned),
        version: metadata.version().map(ToOwned::to_owned),
    }
}

fn is_transient_error(error: &opendal::Error) -> bool {
    // RetryLayer marks all returned errors persistent, even 404/403/412.
    // Never classify by response headers or request IDs in the display text.
    matches!(error.kind(), ErrorKind::Unexpected | ErrorKind::RateLimited)
        && (error.is_temporary() || error.is_persistent())
}

fn normalize_uri(uri: &str) -> Result<String> {
    if !uri.contains("://") {
        let path = std::path::Path::new(uri);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        return Ok(format!("fs://{}", path.to_string_lossy()));
    }
    let mut parsed = Url::parse(uri).context("parse object-store URI")?;
    let scheme = match parsed.scheme() {
        "gs" => "gcs",
        "az" => "azblob",
        "shared-memory" => "memory",
        other => other,
    }
    .to_string();
    if scheme != parsed.scheme() {
        parsed
            .set_scheme(&scheme)
            .map_err(|_| anyhow!("invalid URI scheme"))?;
    }
    Ok(parsed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn explicit_backends_scope_gate_by_endpoint_and_bucket_not_credentials() {
        let config = StoreConfig {
            endpoint: Some("http://127.0.0.1:18060".into()),
            region: Some("us-east-1".into()),
            access_key: Some("scope-test-key".into()),
            secret_key: Some("scope-test-secret".into()),
        };
        let a = Store::from_uri_with_config("s3://scope-test/a", config.clone())
            .await
            .unwrap();
        let mut other = config.clone();
        other.endpoint = Some("http://127.0.0.1:18061".into());
        let b = Store::from_uri_with_config("s3://scope-test/a", other.clone())
            .await
            .unwrap();
        assert_ne!(a.io_scope, b.io_scope);
        assert_ne!(config.fingerprint(), other.fingerprint());
        other = config.clone();
        other.secret_key = Some("rotated-secret".into());
        assert_ne!(config.fingerprint(), other.fingerprint());
        let c = Store::from_uri_with_config("s3://scope-test/b", other)
            .await
            .unwrap();
        assert_eq!(a.io_scope, c.io_scope);
        assert!(!format!("{config:?}").contains("scope-test-secret"));
        assert!(!format!("{config:?}").contains("scope-test-key"));
        // Saturate A without making a network request: B must retain admission.
        let _a = io_gate::acquire(&a.io_scope, IoKind::Read).await;
        let _b = tokio::time::timeout(
            Duration::from_millis(100),
            io_gate::acquire(&b.io_scope, IoKind::Read),
        )
        .await
        .unwrap();
    }

    #[test]
    fn temporary_and_exhausted_transport_errors_still_trigger_backoff() {
        for kind in [ErrorKind::Unexpected, ErrorKind::RateLimited] {
            assert!(is_transient_error(
                &opendal::Error::new(kind, "backend failure").set_temporary()
            ));
            assert!(is_transient_error(
                &opendal::Error::new(kind, "backend failure").set_persistent()
            ));
        }
        assert!(!is_transient_error(
            &opendal::Error::new(ErrorKind::Unexpected, "invalid response").set_permanent()
        ));
        assert!(!is_transient_error(
            &opendal::Error::new(ErrorKind::ConfigInvalid, "connection").set_persistent()
        ));
    }

    #[tokio::test]
    async fn missing_markers_with_connection_headers_do_not_trigger_cooldown() -> Result<()> {
        // RetryLayer marks even non-retryable errors persistent. S3 includes
        // response headers in the error context, including `connection`.
        let mut store = Store::from_uri("shared-memory://missing-marker-gate-test").await?;
        store.io_scope = io_gate::scope_key("s3://missing-marker-gate-test");
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::PermissionDenied,
            ErrorKind::ConditionNotMatch,
        ] {
            let error = store
                .remote::<(), _, _>(IoKind::Read, |_| async move {
                    Err(opendal::Error::new(kind, "S3 response")
                        .with_context("response", "connection: keep-alive; request-id: 503429")
                        .set_persistent())
                })
                .await
                .unwrap_err();
            assert_eq!(error.downcast_ref::<opendal::Error>().unwrap().kind(), kind);
            tokio::time::timeout(
                Duration::from_millis(200),
                store.remote(IoKind::Read, |_| async { Ok(()) }),
            )
            .await
            .context("non-transient response started AIMD cooldown")??;
        }
        Ok(())
    }

    #[tokio::test]
    async fn operator_registry_bounds_clients_without_invalidating_live_handles() -> Result<()> {
        let store = Store::from_uri("shared-memory://registry-test").await?;
        let mut registry = OperatorRegistry::default();
        let now = Instant::now();
        registry.insert("first".into(), store.operator.clone(), now);
        let live = registry.get("first", now).unwrap();
        for n in 0..MAX_CACHED_OPERATORS {
            registry.insert(
                n.to_string(),
                store.operator.clone(),
                now + Duration::from_millis(1),
            );
        }
        assert_eq!(registry.entries.len(), MAX_CACHED_OPERATORS);
        assert!(!registry.entries.contains_key("first"));
        live.write("probe", "still alive").await?;
        assert_eq!(live.read("probe").await?.to_vec(), b"still alive");
        assert!(
            registry
                .get("0", now + OPERATOR_IDLE_TTL + Duration::from_secs(1))
                .is_none()
        );
        assert!(registry.entries.is_empty());
        Ok(())
    }

    #[test]
    fn unquoted_etag_strips_one_quote_pair() {
        assert_eq!(unquoted_etag("\"abc\""), Some("abc"));
        assert_eq!(unquoted_etag("\"\""), None);
        assert_eq!(unquoted_etag("abc"), None);
        assert_eq!(unquoted_etag("\"abc"), None);
    }

    #[tokio::test]
    async fn object_store_operator_accepts_retry_layer() -> Result<()> {
        let store = Store::from_uri("shared-memory://pchronicle-retry-layer/root").await?;
        store
            .write_overwrite("probe.json", b"{\"ok\":true}".to_vec())
            .await?;
        let loaded = store.read("probe.json").await?.context("probe missing")?;
        assert_eq!(loaded.0, b"{\"ok\":true}");
        Ok(())
    }
}

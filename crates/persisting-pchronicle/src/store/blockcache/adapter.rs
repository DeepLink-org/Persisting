//! Lance bridge and ObjectStore read-through adapter. Only validated range reads
//! are cached; HEAD and full GET retain the backend's native semantics/stream.

use super::{BlockCache, CacheConfig};
use crate::store::object_store_io_gate::{self as io_gate, IoKind};
use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt, stream::BoxStream};
use object_store::{
    CopyOptions, GetOptions, GetResult, GetResultPayload, ListResult, MultipartUpload, ObjectMeta,
    ObjectStore, PutMultipartOptions, PutOptions, PutPayload, PutResult, Result as ObjectResult,
    UploadPart, path::Path,
};
use std::{ops::Range, sync::Arc};

/// Admission and feedback live at the object-request boundary. The backend
/// already retries transport errors; do not replay an entire Lance open here.
async fn remote_request<T>(
    uri: &str,
    request: impl std::future::Future<Output = ObjectResult<T>>,
) -> ObjectResult<T> {
    let _permit = io_gate::acquire(uri, IoKind::Read).await;
    let result = request.await;
    match &result {
        Ok(_) => io_gate::note_success(uri),
        Err(error) if io_gate::is_transient_error(error) => {
            io_gate::note_failure(uri, IoKind::Read)
        }
        Err(_) => {}
    }
    result
}

async fn remote_write_request<T>(
    uri: &str,
    request: impl std::future::Future<Output = ObjectResult<T>>,
) -> ObjectResult<T> {
    let _permit = io_gate::acquire(uri, IoKind::Write).await;
    let result = request.await;
    match &result {
        Ok(_) => io_gate::note_success(uri),
        Err(error) if io_gate::is_transient_error(error) => {
            io_gate::note_failure(uri, IoKind::Write)
        }
        Err(_) => {}
    }
    result
}

struct GatedMultipartUpload {
    inner: Arc<tokio::sync::Mutex<Box<dyn MultipartUpload>>>,
    io_scope: String,
}

impl std::fmt::Debug for GatedMultipartUpload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatedMultipartUpload")
            .field("io_scope", &self.io_scope)
            .finish()
    }
}

#[async_trait]
impl MultipartUpload for GatedMultipartUpload {
    fn put_part(&mut self, data: PutPayload) -> UploadPart {
        let inner = Arc::clone(&self.inner);
        let scope = self.io_scope.clone();
        Box::pin(async move {
            // ponytail: serialize multipart parts because object_store's trait
            // returns a 'static future from &mut self; parallelism can be added
            // when the trait exposes owned part handles.
            let mut inner = inner.lock().await;
            remote_write_request(&scope, inner.put_part(data)).await
        })
    }

    async fn complete(&mut self) -> ObjectResult<PutResult> {
        let mut inner = self.inner.lock().await;
        remote_write_request(&self.io_scope, inner.complete()).await
    }

    async fn abort(&mut self) -> ObjectResult<()> {
        let mut inner = self.inner.lock().await;
        remote_write_request(&self.io_scope, inner.abort()).await
    }
}

#[derive(Debug, Clone)]
pub struct LanceCacheWrapper {
    pub capacity_bytes: u64,
    pub block_size_bytes: u64,
    pub root: std::path::PathBuf,
}

fn backend_identity_from_env() -> String {
    let backend: std::collections::BTreeMap<_, _> = std::env::vars()
        .filter(|(key, _)| {
            key.starts_with("AWS_") || key.starts_with("AZURE_") || key.starts_with("GOOGLE_")
        })
        .collect();
    blake3::hash(serde_json::to_string(&backend).unwrap().as_bytes())
        .to_hex()
        .to_string()
}

impl lance_io::object_store::WrappingObjectStore for LanceCacheWrapper {
    fn wrap(&self, prefix: &str, original: Arc<dyn ObjectStore>) -> Arc<dyn ObjectStore> {
        // Lance uses e.g. s3$bucket, not a URI. Convert only its scheme delimiter.
        let uri = prefix.replacen('$', "://", 1);
        if !io_gate::is_remote_uri(&uri) {
            return original;
        }
        let config = CacheConfig::new(
            self.root.clone(),
            self.capacity_bytes,
            self.block_size_bytes,
        );
        Arc::new(CachedObjectStore::new(
            original,
            config,
            format!("{uri}#{}", backend_identity_from_env()),
        ))
    }
}

pub fn lance_wrapper(capacity_bytes: u64) -> Arc<dyn lance_io::object_store::WrappingObjectStore> {
    let mut config = CacheConfig::from_env();
    config.capacity_bytes = capacity_bytes;
    Arc::new(LanceCacheWrapper {
        capacity_bytes,
        block_size_bytes: config.block_size_bytes,
        root: config.root,
    })
}

pub fn lance_store_params(capacity_bytes: u64) -> lance_io::object_store::ObjectStoreParams {
    lance_io::object_store::ObjectStoreParams {
        object_store_wrapper: Some(lance_wrapper(capacity_bytes)),
        ..Default::default()
    }
}

#[derive(Debug, Clone)]
pub struct CachedObjectStore {
    inner: Arc<dyn ObjectStore>,
    cache: BlockCache,
    block_size_bytes: u64,
    store_uri: String,
    io_scope: String,
}

impl CachedObjectStore {
    /// `store_uri` is the cache namespace: include the backend/credential scope
    /// as well as bucket. Use a digest for any sensitive configuration.
    pub fn new(inner: Arc<dyn ObjectStore>, config: CacheConfig, store_uri: String) -> Self {
        let block_size_bytes = config.block_size_bytes;
        Self {
            inner,
            cache: BlockCache::new(config),
            block_size_bytes,
            io_scope: io_gate::scope_key(&store_uri),
            store_uri,
        }
    }

    async fn read_block(
        &self,
        path: &Path,
        meta: &ObjectMeta,
        options: GetOptions,
        key: &str,
        range: Range<u64>,
    ) -> ObjectResult<Bytes> {
        let block = range.start / self.block_size_bytes;
        let file = self.cache.block_path(key, block);
        self.cache
            .get_or_fetch(&file, (range.end - range.start) as usize, async {
                remote_request(&self.io_scope, async {
                    let result = self.inner.get_opts(path, options.clone()).await?;
                    // Do not publish mismatched bytes even if a compatible backend
                    // ignores If-Match or the requested VersionId.
                    if result.range != range
                        || result.meta.size != meta.size
                        || options
                            .version
                            .as_ref()
                            .is_some_and(|v| result.meta.version.as_ref() != Some(v))
                        || options
                            .if_match
                            .as_ref()
                            .is_some_and(|tag| result.meta.e_tag.as_ref() != Some(tag))
                    {
                        return Err(object_store::Error::Precondition {
                            path: path.to_string(),
                            source: "object changed while reading cached block".into(),
                        });
                    }
                    result.bytes().await
                })
                .await
            })
            .await
    }
}

impl std::fmt::Display for CachedObjectStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cached({})", self.inner)
    }
}

#[async_trait]
impl ObjectStore for CachedObjectStore {
    async fn put_opts(&self, p: &Path, b: PutPayload, o: PutOptions) -> ObjectResult<PutResult> {
        remote_write_request(&self.io_scope, self.inner.put_opts(p, b, o)).await
    }
    async fn put_multipart_opts(
        &self,
        p: &Path,
        o: PutMultipartOptions,
    ) -> ObjectResult<Box<dyn MultipartUpload>> {
        let upload =
            remote_write_request(&self.io_scope, self.inner.put_multipart_opts(p, o)).await?;
        Ok(Box::new(GatedMultipartUpload {
            inner: Arc::new(tokio::sync::Mutex::new(upload)),
            io_scope: self.io_scope.clone(),
        }))
    }
    async fn get_opts(&self, p: &Path, o: GetOptions) -> ObjectResult<GetResult> {
        if o.head || o.range.is_none() {
            return remote_request(&self.io_scope, self.inner.get_opts(p, o)).await;
        }
        // Fetch metadata for the requested version with all caller conditions.
        let head = remote_request(
            &self.io_scope,
            self.inner.get_opts(
                p,
                GetOptions {
                    head: true,
                    range: None,
                    ..o.clone()
                },
            ),
        )
        .await?;
        o.check_preconditions(&head.meta)?;
        let version = head
            .meta
            .version
            .clone()
            .filter(|v| !v.is_empty() && v != "null");
        let etag = head
            .meta
            .e_tag
            .clone()
            .filter(|v| !v.is_empty() && !v.starts_with("W/"));
        if version.is_none() && etag.is_none() {
            return remote_request(&self.io_scope, self.inner.get_opts(p, o)).await;
        }
        // A backend that did not identify the requested version cannot safely
        // populate a version-keyed cache. Preserve its native GET semantics.
        if o.version.is_some() && o.version != head.meta.version {
            return remote_request(&self.io_scope, self.inner.get_opts(p, o)).await;
        }
        let range = o
            .range
            .as_ref()
            .unwrap()
            .as_range(head.meta.size)
            .map_err(|source| object_store::Error::Generic {
                store: "pchronicle-cache",
                source: Box::new(source),
            })?;
        // v2 deliberately never reads the previous unnamespaced cache entries.
        let key = serde_json::to_string(&(
            "v2",
            &self.store_uri,
            p.as_ref(),
            &version,
            &etag,
            head.meta.size,
            self.block_size_bytes,
        ))
        .unwrap();
        let options = GetOptions {
            version: version.or(o.version),
            if_match: etag.or(o.if_match),
            ..o
        };
        let this = self.clone();
        let path = p.clone();
        let meta = head.meta.clone();
        let block_size = self.block_size_bytes;
        let blocks = (range.start / block_size
            ..range.end.saturating_add(block_size - 1) / block_size)
            .map(|block| {
                let block_start = block * block_size;
                let block_end = block_start.saturating_add(block_size).min(meta.size);
                let visible_start = range.start.max(block_start);
                let visible_end = range.end.min(block_end);
                (block_start, block_end, visible_start, visible_end)
            })
            .collect::<Vec<_>>();
        // Keep at most four misses in flight. The stream remains lazy, while
        // adjacent uncached blocks overlap their S3 requests.
        let stream = futures::stream::iter(blocks)
            .map(
                move |(block_start, block_end, visible_start, visible_end)| {
                    let this = this.clone();
                    let path = path.clone();
                    let meta = meta.clone();
                    let key = key.clone();
                    let mut options = options.clone();
                    async move {
                        options.range = Some((block_start..block_end).into());
                        let bytes = this
                            .read_block(&path, &meta, options, &key, block_start..block_end)
                            .await?;
                        Ok::<_, object_store::Error>(bytes.slice(
                            (visible_start - block_start) as usize
                                ..(visible_end - block_start) as usize,
                        ))
                    }
                },
            )
            .buffered(4);
        Ok(GetResult {
            payload: GetResultPayload::Stream(stream.boxed()),
            meta: head.meta,
            range,
            attributes: head.attributes,
        })
    }
    fn delete_stream(
        &self,
        p: BoxStream<'static, ObjectResult<Path>>,
    ) -> BoxStream<'static, ObjectResult<Path>> {
        let uri = self.io_scope.clone();
        let stream = self.inner.delete_stream(p);
        futures::stream::unfold(stream, move |mut stream| {
            let uri = uri.clone();
            async move {
                let _permit = io_gate::acquire(&uri, IoKind::Write).await;
                match stream.next().await {
                    Some(result) => {
                        if result.is_ok() {
                            io_gate::note_success(&uri);
                        } else if result
                            .as_ref()
                            .err()
                            .is_some_and(io_gate::is_transient_error)
                        {
                            io_gate::note_failure(&uri, IoKind::Write);
                        }
                        Some((result, stream))
                    }
                    None => None,
                }
            }
        })
        .boxed()
    }
    fn list(&self, p: Option<&Path>) -> BoxStream<'static, ObjectResult<ObjectMeta>> {
        let uri = self.io_scope.clone();
        let stream = self.inner.list(p);
        futures::stream::unfold(stream, move |mut stream| {
            let uri = uri.clone();
            async move {
                let _permit = io_gate::acquire(&uri, IoKind::Read).await;
                match stream.next().await {
                    Some(result) => {
                        if result.is_ok() {
                            io_gate::note_success(&uri);
                        } else if result
                            .as_ref()
                            .err()
                            .is_some_and(io_gate::is_transient_error)
                        {
                            io_gate::note_failure(&uri, IoKind::Read);
                        }
                        Some((result, stream))
                    }
                    None => None,
                }
            }
        })
        .boxed()
    }
    async fn list_with_delimiter(&self, p: Option<&Path>) -> ObjectResult<ListResult> {
        remote_request(&self.io_scope, self.inner.list_with_delimiter(p)).await
    }
    async fn copy_opts(&self, a: &Path, b: &Path, o: CopyOptions) -> ObjectResult<()> {
        remote_write_request(&self.io_scope, self.inner.copy_opts(a, b, o)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::{ObjectStoreExt, memory::InMemory};

    #[tokio::test]
    async fn range_cache_preserves_head_and_full_get_semantics() {
        let root = tempfile::tempdir().unwrap();
        let inner = Arc::new(InMemory::new());
        let path = Path::from("dataset/data.lance");
        inner
            .put(&path, Bytes::from_static(b"0123456789").into())
            .await
            .unwrap();
        let cached = CachedObjectStore::new(
            inner.clone(),
            CacheConfig::new(root.path().into(), 1024, 4),
            "s3://test-bucket".into(),
        );

        let head = cached.head(&path).await.unwrap();
        assert_eq!(head.size, 10);
        assert_eq!(
            cached.get_range(&path, 3..8).await.unwrap(),
            Bytes::from_static(b"34567")
        );
        assert_eq!(
            cached.get(&path).await.unwrap().bytes().await.unwrap(),
            Bytes::from_static(b"0123456789")
        );
    }
}

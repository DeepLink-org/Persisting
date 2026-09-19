use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};
use persisting_pchronicle::storage::{BlockCache, CacheConfig};
use std::hint::black_box;
use tempfile::tempdir;

fn block_cache(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = tempdir().unwrap();
    let cache = BlockCache::new(CacheConfig::new(
        dir.path().into(),
        64 * 1024 * 1024,
        64 * 1024,
    ));
    let path = dir.path().join("block");
    rt.block_on(cache.get_or_fetch(&path, 4, async { Ok(Bytes::from_static(b"data")) }))
        .unwrap();
    c.bench_function("block_cache_hit", |b| {
        b.iter(|| rt.block_on(cache.get_or_fetch(black_box(&path), 4, async { unreachable!() })))
    });
}

criterion_group!(benches, block_cache);
criterion_main!(benches);

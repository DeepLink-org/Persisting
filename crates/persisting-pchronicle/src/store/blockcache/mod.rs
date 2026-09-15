mod adapter;
mod block;
mod config;

pub use adapter::{CachedObjectStore, LanceCacheWrapper, lance_store_params};
pub use block::{BlockCache, CacheStats};
pub use config::{
    CacheConfig, DEFAULT_BLOCK_SIZE_BYTES, DEFAULT_CAPACITY_BYTES, SERVE_CAPACITY_BYTES,
    capacity_for_serve, default_cache_dir,
};

pub fn configured_capacity_bytes() -> u64 {
    CacheConfig::from_env().capacity_bytes
}

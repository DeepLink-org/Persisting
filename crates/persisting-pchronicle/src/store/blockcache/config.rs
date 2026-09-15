use std::path::PathBuf;

pub const DEFAULT_CAPACITY_BYTES: u64 = 512 * 1024 * 1024;
pub const SERVE_CAPACITY_BYTES: u64 = 8 * DEFAULT_CAPACITY_BYTES;
pub const DEFAULT_BLOCK_SIZE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub root: PathBuf,
    pub capacity_bytes: u64,
    pub block_size_bytes: u64,
}

impl CacheConfig {
    pub fn new(root: PathBuf, capacity_bytes: u64, block_size_bytes: u64) -> Self {
        assert!(capacity_bytes > 0 && block_size_bytes > 0);
        Self {
            root,
            capacity_bytes,
            block_size_bytes,
        }
    }

    pub fn from_env() -> Self {
        let capacity = std::env::var("PCHRONICLE_LANCE_CACHE_CAPACITY_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_CAPACITY_BYTES);
        Self::new(default_cache_dir(), capacity, DEFAULT_BLOCK_SIZE_BYTES)
    }
}

pub fn default_cache_dir() -> PathBuf {
    std::env::var("PCHRONICLE_LANCE_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("pchronicle")
                .join("blocks")
        })
}

pub const fn capacity_for_serve(is_serve: bool) -> u64 {
    if is_serve {
        SERVE_CAPACITY_BYTES
    } else {
        DEFAULT_CAPACITY_BYTES
    }
}

//! Process-wide admission control for memory-intensive scalar-index builds.

use std::sync::{Arc, OnceLock};

/// Lance index creation performs external sorts whose merge buffers are large
/// relative to the default DataFusion memory pool. Serializing builds keeps
/// concurrent Run finalization from multiplying those reservations.
pub(crate) async fn acquire() -> tokio::sync::OwnedMutexGuard<()> {
    static GATE: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    GATE.get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
        .lock_owned()
        .await
}

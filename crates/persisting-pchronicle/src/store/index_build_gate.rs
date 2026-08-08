//! Process-wide admission control for memory-intensive scalar-index builds.

use std::sync::{Arc, OnceLock};

/// Lance index creation performs external sorts whose merge buffers are large
/// relative to the default DataFusion memory pool. Serializing builds keeps
/// concurrent Run finalization from multiplying those reservations.
pub(crate) async fn acquire() -> tokio::sync::OwnedSemaphorePermit {
    static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone()
        .acquire_owned()
        .await
        .expect("pChronicle index-build gate cannot be closed")
}

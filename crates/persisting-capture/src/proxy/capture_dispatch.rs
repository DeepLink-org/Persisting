//! Fire-and-forget capture dispatch from the proxy (never blocks LLM responses).

use std::sync::Arc;

use crate::engine::{CaptureEngine, CaptureEvent, CaptureInvocation};

/// Run capture on a background task; failures are logged and written to dead letter inside `apply`.
pub fn spawn_capture_apply(engine: CaptureEngine, inv: CaptureInvocation, event: CaptureEvent) {
    tokio::spawn(async move {
        if let Err(e) = engine.apply(&inv, event).await {
            tracing::warn!("capture apply: {e:#}");
        }
    });
}

/// Same as [`spawn_capture_apply`] but keeps `Arc` engine for call sites that hold shared state.
pub fn spawn_capture_apply_arc(
    engine: Arc<CaptureEngine>,
    inv: CaptureInvocation,
    event: CaptureEvent,
) {
    tokio::spawn(async move {
        if let Err(e) = engine.apply(&inv, event).await {
            tracing::warn!("capture apply: {e:#}");
        }
    });
}

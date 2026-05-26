//! Per-`seq_key` ordered capture apply queue — preserves event order within a session.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc;

use super::runtime::CaptureRuntimeInner;
use super::{CaptureEvent, CaptureInvocation};
use crate::dead_letter;

const APPLY_QUEUE_CAPACITY: usize = 256;

struct ApplyJob {
    inv: CaptureInvocation,
    event: CaptureEvent,
}

/// Serializes `apply` calls per session `seq_key` while keeping the proxy non-blocking.
#[derive(Clone)]
pub(crate) struct ApplyDispatcher {
    inner: Arc<CaptureRuntimeInner>,
    queues: Arc<DashMap<String, mpsc::Sender<ApplyJob>>>,
}

impl ApplyDispatcher {
    pub(crate) fn new(inner: Arc<CaptureRuntimeInner>) -> Self {
        Self {
            inner,
            queues: Arc::new(DashMap::new()),
        }
    }

    pub(crate) fn enqueue(&self, inv: CaptureInvocation, event: CaptureEvent) {
        let seq_key = inv.route.seq_key();
        let tx = self
            .queues
            .entry(seq_key)
            .or_insert_with(|| self.spawn_consumer())
            .clone();

        let job = ApplyJob { inv, event };
        if let Err(e) = tx.try_send(job) {
            let (inv, event) = match e {
                mpsc::error::TrySendError::Full(j) | mpsc::error::TrySendError::Closed(j) => {
                    (j.inv, j.event)
                }
            };
            let storage = self.inner.session_deps.storage.as_path();
            if let Err(dl) = dead_letter::append_dead_letter(
                storage,
                &inv,
                &event,
                "apply queue full or closed",
                None,
            ) {
                tracing::error!("dead letter write failed: {dl:#}");
            }
            tracing::warn!(
                target: "persisting_capture",
                seq_key = %inv.route.seq_key(),
                "capture apply queue rejected job"
            );
        }
    }

    fn spawn_consumer(&self) -> mpsc::Sender<ApplyJob> {
        let (tx, mut rx) = mpsc::channel::<ApplyJob>(APPLY_QUEUE_CAPACITY);
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                if let Err(e) = inner.apply(&job.inv, job.event).await {
                    tracing::warn!(
                        target: "persisting_capture",
                        "capture apply: {e:#}"
                    );
                }
            }
        });
        tx
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::capture_call::CaptureCall;
    use crate::config::CaptureLevel;
    use crate::engine::CaptureEngine;
    use crate::engine::{CaptureEvent, LlmRequestCaptured, LlmResponseCompleted};
    use crate::protocol::ProtocolKind;
    use crate::provider::ProviderKind;
    use crate::record::CaptureRecord;
    use crate::session_index::SessionIndexStore;
    use crate::session_storage::CaptureRoute;
    use crate::sink::CaptureSink;

    struct OrderRecordingSink {
        order: Mutex<Vec<String>>,
        next_seq: Mutex<HashMap<String, u64>>,
    }

    impl OrderRecordingSink {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                order: Mutex::new(Vec::new()),
                next_seq: Mutex::new(HashMap::new()),
            })
        }

        fn drain_order(&self) -> Vec<String> {
            self.order.lock().unwrap().clone()
        }
    }

    impl CaptureSink for OrderRecordingSink {
        fn append(
            &self,
            route: &CaptureRoute,
            _agent_id: &str,
            record: &mut CaptureRecord,
        ) -> anyhow::Result<()> {
            let mut guard = self.next_seq.lock().unwrap();
            let seq = guard.entry(route.seq_key()).or_insert(0);
            record.seq = *seq;
            *seq += 1;
            drop(guard);
            self.order.lock().unwrap().push(format!(
                "{}:{}",
                record.kind,
                record.call_id.as_deref().unwrap_or("")
            ));
            Ok(())
        }

        fn peek_next_seq(&self, route: &CaptureRoute) -> Option<u64> {
            Some(
                self.next_seq
                    .lock()
                    .unwrap()
                    .get(&route.seq_key())
                    .copied()
                    .unwrap_or(0),
            )
        }
    }

    fn sample_inv(call_id: &str) -> CaptureInvocation {
        CaptureInvocation {
            route: CaptureRoute {
                root_session: Some("run-1".into()),
                session_id: "sess".into(),
                storage_session_id: "run-1".into(),
                subagent_id: None,
            },
            agent_id: "agent".into(),
            call: CaptureCall {
                call_id: call_id.into(),
                trace_id: "t1".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
            },
            request_headers: Vec::new(),
            level: CaptureLevel::Dialogue,
            client_model: "m".into(),
            upstream_model: "m".into(),
            provider: ProviderKind::OpenAi,
            protocol: ProtocolKind::ChatCompletions,
            debug_on: false,
        }
    }

    #[tokio::test]
    async fn dispatcher_preserves_request_before_response_order() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(dir.path().to_path_buf());
        let sink = OrderRecordingSink::new();
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let engine = CaptureEngine::new(sink.clone(), index, storage.clone(), false)
            .await
            .unwrap();

        let inv_req = sample_inv("call-a");
        let inv_resp = sample_inv("call-a");
        engine.spawn_apply(
            inv_req,
            CaptureEvent::LlmRequest(LlmRequestCaptured {
                path: "/v1/chat/completions".into(),
                body_bytes: 10,
                user_content: Some("hi".into()),
                body_json: None,
                model_rewritten: false,
            }),
        );
        engine.spawn_apply(
            inv_resp,
            CaptureEvent::LlmResponseCompleted(LlmResponseCompleted {
                status: 200,
                resp_bytes: bytes::Bytes::from_static(
                    br#"{"choices":[{"message":{"content":"ok"}}]}"#,
                ),
                streaming: false,
                stream_metrics: None,
                assistant_content: Some("ok".into()),
            }),
        );

        engine.flush().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        engine.flush().await.unwrap();

        let order = sink.drain_order();
        assert!(
            order.len() >= 2
                && order[0].starts_with("llm.request:")
                && order[1].starts_with("llm.response"),
            "expected request before response, got {order:?}"
        );
    }
}

//! Per-`story_id` ordered capture apply queue — preserves event order within a story.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc;

use super::runtime::CaptureRuntimeInner;
use super::{CallContext, Event};
use crate::dead_letter;

const APPLY_QUEUE_CAPACITY: usize = 256;

struct ApplyJob {
    ctx: CallContext,
    event: Event,
}

/// Serializes `apply` calls per story while keeping the proxy non-blocking.
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

    pub(crate) fn enqueue(&self, ctx: CallContext, event: Event) {
        let story_id = ctx.story_id().as_str().to_string();
        let tx = self
            .queues
            .entry(story_id)
            .or_insert_with(|| self.spawn_consumer())
            .clone();

        let job = ApplyJob { ctx, event };
        if let Err(e) = tx.try_send(job) {
            let (ctx, event) = match e {
                mpsc::error::TrySendError::Full(j) | mpsc::error::TrySendError::Closed(j) => {
                    (j.ctx, j.event)
                }
            };
            let storage = self.inner.story_deps.storage.as_path();
            if let Err(dl) = dead_letter::append_dead_letter(
                storage,
                &ctx,
                &event,
                "apply queue full or closed",
                None,
            ) {
                tracing::error!("dead letter write failed: {dl:#}");
            }
            tracing::warn!(
                target: "persisting_capture",
                story_id = %ctx.story_id().as_str(),
                "capture apply queue rejected job"
            );
        }
    }

    fn spawn_consumer(&self) -> mpsc::Sender<ApplyJob> {
        let (tx, mut rx) = mpsc::channel::<ApplyJob>(APPLY_QUEUE_CAPACITY);
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                if let Err(e) = inner.apply(&job.ctx, job.event).await {
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
    use crate::config::CaptureLevel;
    use crate::engine::CaptureEngine;
    use crate::engine::{CompleteEvent, Event, RequestEvent};
    use crate::protocol::ProtocolKind;
    use crate::provider::ProviderKind;
    use crate::record::CaptureRecord;
    use crate::session_index::SessionIndexStore;
    use crate::session_storage::CaptureRoute;
    use crate::sink::CaptureSink;
    use crate::Call;

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

    fn sample_ctx(call_id: &str) -> CallContext {
        CallContext::new(
            CaptureRoute {
                root_session: Some("run-1".into()),
                session_id: "sess".into(),
                storage_session_id: "run-1".into(),
                subagent_id: None,
            },
            "agent",
            Call {
                call_id: call_id.into(),
                trace_id: "t1".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
            },
            Vec::new(),
            CaptureLevel::Dialogue,
            "m",
            "m",
            ProviderKind::OpenAi,
            ProtocolKind::ChatCompletions,
            false,
        )
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

        let ctx_req = sample_ctx("call-a");
        let ctx_resp = sample_ctx("call-a");
        engine.spawn_apply(
            ctx_req,
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                body_bytes: 10,
                user_content: Some("hi".into()),
                body_json: None,
                model_rewritten: false,
            }),
        );
        engine.spawn_apply(
            ctx_resp,
            Event::ResponseComplete(CompleteEvent {
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

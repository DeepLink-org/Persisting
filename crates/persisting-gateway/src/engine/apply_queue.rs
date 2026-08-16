//! Per-`story_id` ordered capture apply queue — preserves event order within a story.

use std::path::PathBuf;
use std::sync::{mpsc as std_mpsc, Arc, Mutex};

use dashmap::DashMap;
use tokio::sync::{mpsc, oneshot};

use super::coordinator::CaptureRuntimeInner;
use super::{CallContext, Event};
use crate::dead_letter;

const APPLY_QUEUE_CAPACITY: usize = 256;
const REJECTED_EVENT_QUEUE_CAPACITY: usize = 256;

enum RejectedEventMessage {
    Event { ctx: Arc<CallContext>, event: Event },
    Finish,
}

struct RejectedEventWriter {
    tx: std_mpsc::SyncSender<RejectedEventMessage>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl RejectedEventWriter {
    fn new(storage: Arc<PathBuf>) -> Self {
        let (tx, rx) = std_mpsc::sync_channel(REJECTED_EVENT_QUEUE_CAPACITY);
        let worker = std::thread::Builder::new()
            .name("persisting-dead-letter".to_string())
            .spawn(move || {
                while let Ok(message) = rx.recv() {
                    let RejectedEventMessage::Event { ctx, event } = message else {
                        break;
                    };
                    if let Err(error) = dead_letter::append_dead_letter(
                        storage.as_path(),
                        &ctx,
                        &event,
                        "apply queue full or closed",
                        None,
                    ) {
                        tracing::error!("dead letter write failed: {error:#}");
                    }
                }
            })
            .expect("dead-letter writer thread must start with capture runtime");
        Self {
            tx,
            worker: Mutex::new(Some(worker)),
        }
    }

    fn try_record(&self, ctx: Arc<CallContext>, event: Event) {
        if let Err(error) = self.tx.try_send(RejectedEventMessage::Event { ctx, event }) {
            tracing::warn!(
                target: "persisting_gateway",
                "dead-letter queue rejected overloaded capture event: {error}"
            );
        }
    }
}

impl Drop for RejectedEventWriter {
    fn drop(&mut self) {
        let _ = self.tx.send(RejectedEventMessage::Finish);
        if let Some(worker) = self.worker.lock().expect("dead-letter worker mutex").take() {
            let _ = worker.join();
        }
    }
}

enum ApplyJob {
    Capture {
        ctx: Arc<CallContext>,
        event: Event,
        /// WAL sequence to ack only after the canonical sink confirms success.
        wal_seq: Option<u64>,
    },
    Barrier {
        ack: oneshot::Sender<()>,
    },
}

/// Serializes `apply` calls per story while keeping the proxy non-blocking.
#[derive(Clone)]
pub(crate) struct ApplyDispatcher {
    inner: Arc<CaptureRuntimeInner>,
    queues: Arc<DashMap<String, mpsc::Sender<ApplyJob>>>,
    rejected: Arc<RejectedEventWriter>,
}

impl ApplyDispatcher {
    pub(crate) fn new(inner: Arc<CaptureRuntimeInner>) -> Self {
        let rejected = Arc::new(RejectedEventWriter::new(Arc::clone(
            &inner.story_deps.storage,
        )));
        Self {
            inner,
            queues: Arc::new(DashMap::new()),
            rejected,
        }
    }

    pub(crate) fn enqueue(&self, ctx: Arc<CallContext>, event: Event, wal_seq: Option<u64>) {
        let story_id = ctx.story_id().as_str().to_string();
        let tx = self
            .queues
            .entry(story_id)
            .or_insert_with(|| self.spawn_consumer())
            .clone();

        let job = ApplyJob::Capture {
            ctx,
            event,
            wal_seq,
        };
        if let Err(e) = tx.try_send(job) {
            self.record_rejected_job(e.into_inner());
        }
    }

    /// Wait for all jobs already accepted by every per-story queue to finish.
    pub(crate) async fn flush(&self) -> anyhow::Result<()> {
        let queues: Vec<_> = self
            .queues
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        for tx in queues {
            let (ack, done) = oneshot::channel();
            tx.send(ApplyJob::Barrier { ack })
                .await
                .map_err(|_| anyhow::anyhow!("apply queue closed while flushing"))?;
            done.await
                .map_err(|_| anyhow::anyhow!("apply queue barrier dropped"))?;
        }
        Ok(())
    }

    fn spawn_consumer(&self) -> mpsc::Sender<ApplyJob> {
        let (tx, mut rx) = mpsc::channel::<ApplyJob>(APPLY_QUEUE_CAPACITY);
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                match job {
                    ApplyJob::Capture {
                        ctx,
                        event,
                        wal_seq,
                    } => {
                        match inner.apply(&ctx, event).await {
                            Ok(()) => {
                                if let Some(seq) = wal_seq {
                                    inner.wal.ack(seq);
                                }
                            }
                            Err(e) => {
                                // Keep the WAL entry pending. The dead letter is an
                                // operator diagnostic, not a durable-store substitute;
                                // restart replay may recover a transient sink failure.
                                tracing::warn!(
                                    target: "persisting_gateway",
                                    "capture apply: {e:#}"
                                );
                            }
                        }
                    }
                    ApplyJob::Barrier { ack } => {
                        let _ = ack.send(());
                    }
                }
            }
        });
        tx
    }

    fn record_rejected_job(&self, job: ApplyJob) {
        let ApplyJob::Capture {
            ctx,
            event,
            wal_seq: _,
        } = job
        else {
            return;
        };
        self.rejected.try_record(Arc::clone(&ctx), event);
        // A rejected queue job never reached the durable sink, so its WAL row
        // deliberately remains pending for restart replay.
        tracing::warn!(
            target: "persisting_gateway",
            story_id = %ctx.story_id().as_str(),
            "capture apply queue rejected job"
        );
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
    use crate::record::EventRecord;
    use crate::session::index::SessionIndexStore;
    use crate::session::storage::CaptureRoute;
    use crate::sink::CaptureEventSink;
    use crate::Call;

    struct OrderRecordingSink {
        order: Mutex<Vec<String>>,
        next_seq: Mutex<HashMap<String, u64>>,
    }

    struct SlowSink;

    impl CaptureEventSink for SlowSink {
        fn append(
            &self,
            _route: &CaptureRoute,
            _agent_id: &str,
            _record: &mut EventRecord,
        ) -> anyhow::Result<()> {
            std::thread::sleep(std::time::Duration::from_millis(400));
            Ok(())
        }
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

    impl CaptureEventSink for OrderRecordingSink {
        fn append(
            &self,
            route: &CaptureRoute,
            _agent_id: &str,
            record: &mut EventRecord,
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
                method: "POST".into(),
                url: None,
                body_bytes: 10,
                user_content: Some("hi".into()),
                body_json: None,
                semantic: None,
                model_rewritten: false,
                headers: vec![],
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
                semantic: None,
                headers: vec![],
            }),
        );

        engine.flush().await.unwrap();

        let order = sink.drain_order();
        assert!(
            order.len() >= 2
                && order[0].starts_with("llm.request:")
                && order[1].starts_with("llm.response"),
            "expected request before response, got {order:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn durable_sink_wait_does_not_block_tokio_worker() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(dir.path().to_path_buf());
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let engine = CaptureEngine::new(Arc::new(SlowSink), index, storage, false)
            .await
            .unwrap();

        let started = std::time::Instant::now();
        engine.spawn_apply(
            sample_ctx("slow-call"),
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                method: "POST".into(),
                url: None,
                body_bytes: 10,
                user_content: Some("hi".into()),
                body_json: None,
                semantic: None,
                model_rewritten: false,
                headers: vec![],
            }),
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "synchronous sink wait starved the only Tokio worker"
        );
        engine.flush().await.unwrap();
    }
}

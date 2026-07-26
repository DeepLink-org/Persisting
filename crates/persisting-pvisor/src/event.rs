use anyhow::Result;
use async_trait::async_trait;
use persisting_proto::{AttemptId, EventEnvelope, EventId, RunId, EVENT_SCHEMA_VERSION};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

#[async_trait]
pub trait EventSink: Send + Sync {
    async fn append(&self, event: &EventEnvelope) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct NoopEventSink;

#[async_trait]
impl EventSink for NoopEventSink {
    async fn append(&self, _event: &EventEnvelope) -> Result<()> {
        Ok(())
    }
}

/// In-memory sink intended for embedding, tests, and early integrations.
#[derive(Debug, Default)]
pub struct MemoryEventSink {
    events: Mutex<Vec<EventEnvelope>>,
}

impl MemoryEventSink {
    pub fn events(&self) -> Vec<EventEnvelope> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[async_trait]
impl EventSink for MemoryEventSink {
    async fn append(&self, event: &EventEnvelope) -> Result<()> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event.clone());
        Ok(())
    }
}

/// Assigns one monotonic event sequence to an Attempt and fans events out to
/// the canonical sink plus live subscribers.
#[derive(Clone)]
pub struct RunEventPublisher {
    run_id: RunId,
    attempt_id: AttemptId,
    producer: String,
    next_seq: Arc<AtomicU64>,
    sink: Arc<dyn EventSink>,
    live: broadcast::Sender<EventEnvelope>,
}

impl RunEventPublisher {
    pub(crate) fn new(
        run_id: RunId,
        attempt_id: AttemptId,
        producer: impl Into<String>,
        sink: Arc<dyn EventSink>,
        live: broadcast::Sender<EventEnvelope>,
    ) -> Self {
        Self {
            run_id,
            attempt_id,
            producer: producer.into(),
            next_seq: Arc::new(AtomicU64::new(0)),
            sink,
            live,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.live.subscribe()
    }

    pub async fn publish(
        &self,
        kind: impl Into<String>,
        source: impl Into<String>,
        payload: Value,
    ) -> Result<EventEnvelope> {
        let event = EventEnvelope {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id: EventId::new(format!("event-{}", uuid::Uuid::new_v4())),
            run_id: self.run_id.clone(),
            attempt_id: Some(self.attempt_id.clone()),
            storyline_id: None,
            turn_id: None,
            call_id: None,
            seq: self.next_seq.fetch_add(1, Ordering::AcqRel),
            timestamp_unix_ms: crate::runtime::unix_now_ms(),
            kind: kind.into(),
            source: source.into(),
            producer: self.producer.clone(),
            payload,
        };
        let _ = self.live.send(event.clone());
        self.sink.append(&event).await?;
        Ok(event)
    }
}

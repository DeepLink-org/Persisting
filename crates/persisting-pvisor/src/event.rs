use anyhow::Result;
use async_trait::async_trait;
use persisting_control::{AttemptId, RunId};
use persisting_pchronicle::{EventIdentity, EventRecord, EVENT_SCHEMA_VERSION};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Whether an append error proves that the event was not persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventAppendErrorKind {
    /// The sink guarantees that this event was not committed.
    Rejected,
    /// The caller cannot know whether the sink committed the event.
    Unknown,
}

#[async_trait]
pub trait EventSink: Send + Sync {
    async fn append(&self, event: &EventRecord) -> Result<()>;

    /// Errors are ambiguous by default. A sink may opt into `Rejected` only
    /// when it can prove the append had no durable effect.
    fn classify_append_error(&self, _error: &anyhow::Error) -> EventAppendErrorKind {
        EventAppendErrorKind::Unknown
    }
}

#[derive(Debug, Default)]
pub struct NoopEventSink;

#[async_trait]
impl EventSink for NoopEventSink {
    async fn append(&self, _event: &EventRecord) -> Result<()> {
        Ok(())
    }
}

/// In-memory sink intended for embedding, tests, and early integrations.
#[derive(Debug, Default)]
pub struct MemoryEventSink {
    events: Mutex<Vec<EventRecord>>,
}

impl MemoryEventSink {
    pub fn events(&self) -> Vec<EventRecord> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[async_trait]
impl EventSink for MemoryEventSink {
    async fn append(&self, event: &EventRecord) -> Result<()> {
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
    live: broadcast::Sender<EventRecord>,
}

impl RunEventPublisher {
    pub(crate) fn new(
        run_id: RunId,
        attempt_id: AttemptId,
        producer: impl Into<String>,
        sink: Arc<dyn EventSink>,
        live: broadcast::Sender<EventRecord>,
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

    pub fn subscribe(&self) -> broadcast::Receiver<EventRecord> {
        self.live.subscribe()
    }

    pub(crate) fn classify_append_error(&self, error: &anyhow::Error) -> EventAppendErrorKind {
        self.sink.classify_append_error(error)
    }

    pub async fn publish(
        &self,
        kind: impl Into<String>,
        source: impl Into<String>,
        payload: Value,
    ) -> Result<EventRecord> {
        let event = EventRecord {
            identity: EventIdentity {
                schema_version: EVENT_SCHEMA_VERSION,
                event_id: Some(format!("event-{}", uuid::Uuid::new_v4())),
                run_id: Some(self.run_id.to_string()),
                attempt_id: Some(self.attempt_id.to_string()),
                timestamp_unix_ms: Some(crate::util::unix_now_ms()),
                producer: Some(self.producer.clone()),
                ..EventIdentity::default()
            },
            seq: self.next_seq.fetch_add(1, Ordering::AcqRel),
            kind: kind.into(),
            source: source.into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload,
        };
        self.sink.append(&event).await?;
        // Live observers only see events accepted by the canonical sink.
        let _ = self.live.send(event.clone());
        Ok(event)
    }
}

use anyhow::Result;
use async_trait::async_trait;
use persisting_agentctl::{AttemptId, RunId};
use persisting_pchronicle::{EventIdentity, EventRecord, EVENT_SCHEMA_VERSION};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::sync::Mutex as AsyncMutex;

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
    next_seq: Arc<AsyncMutex<u64>>,
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
            next_seq: Arc::new(AsyncMutex::new(0)),
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
        // Persistence and sequence assignment form one per-Attempt critical
        // section. A definitely rejected append reuses its sequence; an
        // ambiguous failure consumes it because the sink may have committed.
        let mut next_seq = self.next_seq.lock().await;
        let seq = *next_seq;
        let following_seq = seq.checked_add(1).ok_or_else(|| {
            anyhow::anyhow!("event sequence exhausted for Attempt {}", self.attempt_id)
        })?;
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
            seq,
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
        if let Err(error) = self.sink.append(&event).await {
            if self.sink.classify_append_error(&error) == EventAppendErrorKind::Unknown {
                *next_seq = following_seq;
            }
            return Err(error);
        }
        *next_seq = following_seq;
        drop(next_seq);
        // Live observers only see events accepted by the canonical sink.
        let _ = self.live.send(event.clone());
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailFirstSink {
        kind: EventAppendErrorKind,
        sequences: Mutex<Vec<u64>>,
    }

    #[async_trait]
    impl EventSink for FailFirstSink {
        async fn append(&self, event: &EventRecord) -> Result<()> {
            let mut sequences = self.sequences.lock().unwrap();
            sequences.push(event.seq);
            if sequences.len() == 1 {
                anyhow::bail!("first append failed");
            }
            Ok(())
        }

        fn classify_append_error(&self, _error: &anyhow::Error) -> EventAppendErrorKind {
            self.kind
        }
    }

    fn publisher(sink: Arc<dyn EventSink>) -> RunEventPublisher {
        let (live, _) = broadcast::channel(4);
        RunEventPublisher::new("run".into(), "attempt".into(), "test", sink, live)
    }

    #[tokio::test]
    async fn rejected_append_reuses_sequence() {
        let sink = Arc::new(FailFirstSink {
            kind: EventAppendErrorKind::Rejected,
            sequences: Mutex::new(Vec::new()),
        });
        let events = publisher(sink.clone());
        assert!(events.publish("first", "test", Value::Null).await.is_err());
        let accepted = events.publish("retry", "test", Value::Null).await.unwrap();
        assert_eq!(accepted.seq, 0);
        assert_eq!(*sink.sequences.lock().unwrap(), vec![0, 0]);
    }

    #[tokio::test]
    async fn ambiguous_append_consumes_sequence() {
        let sink = Arc::new(FailFirstSink {
            kind: EventAppendErrorKind::Unknown,
            sequences: Mutex::new(Vec::new()),
        });
        let events = publisher(sink.clone());
        assert!(events.publish("first", "test", Value::Null).await.is_err());
        let accepted = events.publish("retry", "test", Value::Null).await.unwrap();
        assert_eq!(accepted.seq, 1);
        assert_eq!(*sink.sequences.lock().unwrap(), vec![0, 1]);
    }
}

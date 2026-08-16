//! Storage-independent runtime event contracts shared by Persisting components.
//!
//! Producers such as pVisor and Gateway emit these records. Consumers such as
//! pChronicle decide how to persist, query, and project them.
//! The optional `control` feature also carries the lightweight, versioned
//! sidecar protocol so callers do not need another protocol-only package.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(feature = "control")]
mod control;
#[cfg(feature = "control")]
pub use control::*;

/// Runtime identity shared by lifecycle and trajectory events.
///
/// The fields are flattened into [`EventRecord`] on the wire. A storage
/// adapter may fill missing routing identities, but must reject conflicts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storyline_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
}

/// Canonical storage-independent Agent runtime event.
///
/// Ordering within one Attempt is defined by `seq`; wall-clock timestamps are
/// evidence for correlation and display, not the source of ordering truth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    #[serde(flatten)]
    pub identity: EventIdentity,
    pub seq: u64,
    pub source: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_call_id: Option<String>,
    pub payload: Value,
}

impl EventRecord {
    /// Validate the stable event envelope independently of any storage engine.
    pub fn validate(&self) -> Result<(), EventValidationError> {
        if self.source.trim().is_empty() {
            return Err(EventValidationError::MissingSource);
        }
        if self.kind.trim().is_empty() {
            return Err(EventValidationError::MissingKind);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EventValidationError {
    #[error("event source is required")]
    MissingSource,
    #[error("event kind is required")]
    MissingKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_flattened_and_wire_compatible() {
        let record = EventRecord {
            identity: EventIdentity {
                event_id: Some("event-1".into()),
                run_id: Some("run-1".into()),
                ..EventIdentity::default()
            },
            seq: 0,
            source: "runtime".into(),
            kind: "run.created".into(),
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
            payload: Value::Object(Default::default()),
        };
        let encoded = serde_json::to_value(record).unwrap();
        assert_eq!(encoded["event_id"], "event-1");
        assert_eq!(encoded["run_id"], "run-1");
        assert!(encoded.get("identity").is_none());
        assert!(encoded.get("schema_version").is_none());
    }

    #[test]
    fn validates_required_routing_fields() {
        let mut record: EventRecord = serde_json::from_value(serde_json::json!({
            "seq": 0,
            "source": "runtime",
            "kind": "run.created",
            "payload": {}
        }))
        .unwrap();
        record.validate().unwrap();
        record.kind.clear();
        assert_eq!(record.validate(), Err(EventValidationError::MissingKind));
    }
}

//! Property tests for the storage-independent event contract.
//!
//! Keep generators here focused on wire-level invariants. Product-specific
//! behavior belongs in the producer crate; this suite verifies that any
//! producer can serialize, validate, and round-trip the shared envelope.

use persisting_events::{EventIdentity, EventRecord, EventValidationError};
#[cfg(feature = "control")]
use persisting_events::{TrajectoryAppendRequest, TrajectoryFormat};
use proptest::prelude::*;
use serde_json::Value;

fn non_empty_string() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9_.-]{0,24}"
}

fn optional_string() -> impl Strategy<Value = Option<String>> {
    prop::option::of("[a-zA-Z0-9_.-]{0,24}")
}

fn payload() -> impl Strategy<Value = Value> {
    prop::collection::btree_map(
        "[a-z][a-z0-9_]{0,8}",
        prop_oneof![
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(Value::from),
            "[a-zA-Z0-9 _.-]{0,32}".prop_map(Value::String),
        ],
        0..8,
    )
    .prop_map(|fields| Value::Object(fields.into_iter().collect()))
}

prop_compose! {
    fn event_record_strategy()(
        event_id in optional_string(),
        run_id in optional_string(),
        attempt_id in optional_string(),
        storyline_id in optional_string(),
        turn_id in optional_string(),
        timestamp_unix_ms in prop::option::of(any::<u64>()),
        producer in optional_string(),
        seq in any::<u64>(),
        source in non_empty_string(),
        kind in non_empty_string(),
        timestamp in optional_string(),
        session_id in optional_string(),
        agent_id in optional_string(),
        parent_uuid in optional_string(),
        trace_id in optional_string(),
        call_id in optional_string(),
        subagent_id in optional_string(),
        parent_agent_id in optional_string(),
        branch in optional_string(),
        parent_call_id in optional_string(),
        payload in payload(),
    ) -> EventRecord {
        EventRecord {
            identity: EventIdentity {
                event_id,
                run_id,
                attempt_id,
                storyline_id,
                turn_id,
                timestamp_unix_ms,
                producer,
            },
            seq,
            source,
            kind,
            timestamp,
            session_id,
            agent_id,
            parent_uuid,
            trace_id,
            call_id,
            subagent_id,
            parent_agent_id,
            branch,
            parent_call_id,
            payload,
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, .. ProptestConfig::default() })]

    #[test]
    fn event_record_json_roundtrip_preserves_the_flattened_contract(
        record in event_record_strategy()
    ) {
        let encoded = serde_json::to_value(&record).expect("serialize EventRecord");
        prop_assert!(encoded.get("identity").is_none());

        let decoded: EventRecord =
            serde_json::from_value(encoded).expect("deserialize EventRecord");
        prop_assert_eq!(decoded, record);
    }

    #[test]
    fn generated_records_satisfy_required_routing_fields(record in event_record_strategy()) {
        prop_assert_eq!(record.validate(), Ok(()));
    }

    #[test]
    fn validation_rejects_each_missing_routing_field(record in event_record_strategy()) {
        let mut missing_source = record.clone();
        missing_source.source.clear();
        prop_assert_eq!(
            missing_source.validate(),
            Err(EventValidationError::MissingSource)
        );

        let mut missing_kind = record;
        missing_kind.kind.clear();
        prop_assert_eq!(missing_kind.validate(), Err(EventValidationError::MissingKind));
    }
}

#[cfg(feature = "control")]
prop_compose! {
    fn append_request_strategy()(
        storage in non_empty_string(),
        agent_id in non_empty_string(),
        session_id in non_empty_string(),
        format in prop_oneof![Just(TrajectoryFormat::Json), Just(TrajectoryFormat::Lance)],
        root_session_id in optional_string(),
        records in prop::collection::vec(event_record_strategy(), 0..4),
    ) -> TrajectoryAppendRequest {
        TrajectoryAppendRequest {
            storage,
            agent_id,
            session_id,
            format,
            root_session_id,
            records,
        }
    }
}

#[cfg(feature = "control")]
proptest! {
    #![proptest_config(ProptestConfig { cases: 128, .. ProptestConfig::default() })]

    #[test]
    fn trajectory_append_json_roundtrip_preserves_format_and_records(
        request in append_request_strategy()
    ) {
        let encoded = serde_json::to_value(&request).expect("serialize append request");
        let decoded: TrajectoryAppendRequest =
            serde_json::from_value(encoded).expect("deserialize append request");
        prop_assert_eq!(decoded.format, request.format);
        prop_assert_eq!(decoded.records, request.records);
        prop_assert_eq!(decoded.storage, request.storage);
        prop_assert_eq!(decoded.agent_id, request.agent_id);
        prop_assert_eq!(decoded.session_id, request.session_id);
        prop_assert_eq!(decoded.root_session_id, request.root_session_id);
    }
}

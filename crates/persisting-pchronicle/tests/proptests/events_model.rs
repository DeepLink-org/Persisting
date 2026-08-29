use persisting_pchronicle::model::{
    ChronicleEventRecordExt, EventIdentity, EventRecord, EventsDocument,
};
use proptest::prelude::*;
use serde_json::{Map, Value};

fn id_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9._-]{1,32}").unwrap()
}

fn event_strategy() -> impl Strategy<Value = EventRecord> {
    (
        any::<u64>(),
        id_strategy(),
        id_strategy(),
        prop::option::of(id_strategy()),
        prop::option::of(id_strategy()),
    )
        .prop_map(|(seq, source, kind, session_id, agent_id)| EventRecord {
            identity: EventIdentity::default(),
            seq,
            source,
            kind,
            timestamp: None,
            session_id,
            agent_id,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: Value::Object(Map::new()),
        })
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_events_document_new_preserves_events_and_latest_routing_ids(
        events in proptest::collection::vec(event_strategy(), 0..32),
    ) {
        let expected_session = events.iter().rev().find_map(|event| event.session_id.clone());
        let expected_agent = events.iter().rev().find_map(|event| event.agent_id.clone());
        let document = EventsDocument::new(events.clone());
        prop_assert_eq!(document.format, EventsDocument::FORMAT_NAME);
        prop_assert_eq!(document.session_id, expected_session);
        prop_assert_eq!(document.agent_id, expected_agent);
        prop_assert_eq!(document.events, events);
    }

    #[test]
    fn public_events_document_json_roundtrip_preserves_the_envelope(
        events in proptest::collection::vec(event_strategy(), 0..16),
    ) {
        let document = EventsDocument::new(events);
        let encoded = serde_json::to_value(&document).unwrap();
        let decoded: EventsDocument = serde_json::from_value(encoded).unwrap();
        prop_assert_eq!(decoded, document);
    }

    #[test]
    fn public_missing_typed_llm_payloads_are_explicitly_absent(event in event_strategy()) {
        prop_assert!(event.llm_request_payload().unwrap().is_none());
        prop_assert!(event.llm_response_payload().unwrap().is_none());
    }

    #[test]
    fn public_event_validation_rejects_only_empty_source_or_kind(
        source in any::<String>(),
        kind in any::<String>(),
    ) {
        let event = EventRecord {
            identity: EventIdentity::default(),
            seq: 0,
            source: source.clone(),
            kind: kind.clone(),
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
            payload: Value::Null,
        };
        prop_assert_eq!(
            event.validate().is_ok(),
            !source.trim().is_empty() && !kind.trim().is_empty(),
        );
    }

    #[test]
    fn public_newest_nonempty_routing_id_wins(
        events in proptest::collection::vec(event_strategy(), 1..16),
        session in id_strategy(),
    ) {
        let mut events = events;
        events.push(EventRecord {
            identity: EventIdentity::default(),
            seq: 999,
            source: "capture".into(),
            kind: "event".into(),
            timestamp: None,
            session_id: Some(session.clone()),
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: Value::Null,
        });
        prop_assert_eq!(EventsDocument::new(events).session_id, Some(session));
    }

    #[test]
    fn public_events_document_roundtrip_preserves_runtime_identity_fields(
        event_id in id_strategy(),
        run_id in id_strategy(),
        storyline_id in id_strategy(),
        timestamp_unix_ms in any::<u64>(),
    ) {
        let event = EventRecord {
            identity: EventIdentity {
                event_id: Some(event_id),
                run_id: Some(run_id),
                storyline_id: Some(storyline_id),
                timestamp_unix_ms: Some(timestamp_unix_ms),
                ..EventIdentity::default()
            },
            seq: 0,
            source: "capture".into(),
            kind: "note".into(),
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
            payload: Value::Null,
        };
        let document = EventsDocument::new(vec![event]);
        let encoded = serde_json::to_value(&document).unwrap();
        let decoded: EventsDocument = serde_json::from_value(encoded).unwrap();
        prop_assert_eq!(decoded, document);
    }
}

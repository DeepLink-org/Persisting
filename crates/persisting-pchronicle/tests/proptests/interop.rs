use persisting_pchronicle::document::{events_to_har, events_to_otlp_json, otlp_json_to_events};
use persisting_pchronicle::model::{EventIdentity, EventRecord};
use proptest::prelude::*;
use serde_json::json;

fn token_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Za-z0-9._-]{1,32}").unwrap()
}

fn event_strategy() -> impl Strategy<Value = EventRecord> {
    (
        any::<u64>(),
        token_strategy(),
        token_strategy(),
        prop::option::of(token_strategy()),
        prop::option::of(token_strategy()),
        prop::option::of(token_strategy()),
    )
        .prop_map(
            |(seq, source, kind, session_id, trace_id, call_id)| EventRecord {
                identity: EventIdentity::default(),
                seq,
                source,
                kind,
                timestamp: None,
                session_id,
                agent_id: None,
                parent_uuid: None,
                trace_id,
                call_id,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: json!({"value": seq}),
            },
        )
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_otlp_interop_preserves_correlating_ids(
        records in proptest::collection::vec(event_strategy(), 0..24),
    ) {
        let imported = otlp_json_to_events(&events_to_otlp_json(&records));
        prop_assert_eq!(imported.len(), records.len());
        for (index, (original, restored)) in records.iter().zip(imported.iter()).enumerate() {
            prop_assert_eq!(restored.seq, index as u64);
            prop_assert_eq!(&restored.trace_id, &original.trace_id);
            prop_assert_eq!(&restored.call_id, &original.call_id);
            prop_assert_eq!(&restored.session_id, &original.session_id);
            prop_assert_eq!(&restored.kind, "otel.span");
            prop_assert_eq!(restored.payload["degraded"].as_bool(), Some(true));
            prop_assert_eq!(restored.payload["replayable"].as_bool(), Some(false));
        }
    }

    #[test]
    fn public_har_export_keeps_only_request_anchored_calls(
        call_id in token_strategy(),
        trace_id in token_strategy(),
        duration_ms in 0u64..60_000,
    ) {
        let request = EventRecord {
            identity: EventIdentity::default(),
            seq: 0,
            source: "gateway".into(),
            kind: "http.request".into(),
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: Some(trace_id.clone()),
            call_id: Some(call_id.clone()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({"http":{"method":"POST","url":"https://example.test","body":"hi"}}),
        };
        let response = EventRecord {
            kind: "http.response".into(),
            payload: json!({"http":{"status":200,"duration_ms":duration_ms,"body":"ok"}}),
            ..request.clone()
        };
        let log = events_to_har(&[response, request]);
        let entries = log["log"]["entries"].as_array().unwrap();
        prop_assert_eq!(entries.len(), 1);
        prop_assert_eq!(entries[0]["_pchronicle"]["call_id"].as_str(), Some(call_id.as_str()));
        prop_assert_eq!(entries[0]["_pchronicle"]["trace_id"].as_str(), Some(trace_id.as_str()));
        prop_assert_eq!(entries[0]["time"].as_f64(), Some(duration_ms as f64));
        prop_assert_eq!(entries[0]["response"]["status"].as_u64(), Some(200));
    }
}

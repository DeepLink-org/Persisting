use persisting_pchronicle::document::events_to_har;
use persisting_pchronicle::model::{EventIdentity, EventRecord};
use proptest::prelude::*;
use serde_json::json;

fn token_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Za-z0-9._-]{1,32}").unwrap()
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

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

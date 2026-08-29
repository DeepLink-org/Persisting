use persisting_pchronicle::document::project_event_records;
use persisting_pchronicle::model::{EventIdentity, EventRecord};
use proptest::prelude::*;
use serde_json::json;

fn id_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9._-]{1,24}").unwrap()
}

fn response_event(
    session: &str,
    agent: &str,
    call_id: &str,
    seq: u64,
    content: &str,
) -> EventRecord {
    EventRecord {
        identity: EventIdentity::default(),
        seq,
        source: "test".into(),
        kind: "llm.response".into(),
        timestamp: None,
        session_id: Some(session.into()),
        agent_id: Some(agent.into()),
        parent_uuid: None,
        trace_id: None,
        call_id: Some(call_id.into()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: serde_json::json!({"content": content}),
    }
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_projection_uses_the_last_nonempty_routing_identity(
        first_session in id_strategy(),
        last_session in id_strategy(),
        first_agent in id_strategy(),
        last_agent in id_strategy(),
        first_text in proptest::string::string_regex("[A-Za-z0-9 .,!?]{0,48}").unwrap(),
        last_text in proptest::string::string_regex("[A-Za-z0-9 .,!?]{0,48}").unwrap(),
    ) {
        let records = vec![
            response_event(&first_session, &first_agent, "call-1", 100, &first_text),
            response_event(&last_session, &last_agent, "call-2", 1, &last_text),
        ];
        let story = project_event_records(&records).expect("generated events are valid");
        prop_assert_eq!(story.session_id, last_session);
        prop_assert_eq!(story.agent.id, last_agent);
        prop_assert_eq!(story.turns.len(), 2);
        prop_assert_eq!(&story.turns[0].message, &serde_json::json!(first_text));
        prop_assert_eq!(&story.turns[1].message, &serde_json::json!(last_text));
    }

    #[test]
    fn public_projection_derives_latency_and_dialogue_from_request_response(
        session in id_strategy(),
        agent in id_strategy(),
        user_text in proptest::string::string_regex("[A-Za-z0-9 .,!?]{1,48}").unwrap(),
        assistant_text in proptest::string::string_regex("[A-Za-z0-9 .,!?]{1,48}").unwrap(),
    ) {
        let request = EventRecord {
            identity: EventIdentity::default(),
            seq: 10,
            source: "gateway".into(),
            kind: "llm.request".into(),
            timestamp: Some("2026-01-01T00:00:00.000Z".into()),
            session_id: Some(session.clone()),
            agent_id: Some(agent.clone()),
            parent_uuid: None,
            trace_id: None,
            call_id: Some("call-1".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"messages":[{"role":"user","content":user_text}]}),
        };
        let response = EventRecord {
            seq: 11,
            kind: "llm.response".into(),
            timestamp: Some("2026-01-01T00:00:01.250Z".into()),
            payload: serde_json::json!({"content":assistant_text}),
            ..request.clone()
        };
        let story = project_event_records(&[request, response]).expect("valid request/response pair");
        prop_assert_eq!(story.turns.len(), 2);
        prop_assert_eq!(&story.turns[0].source, "user");
        prop_assert_eq!(&story.turns[0].message, &serde_json::json!(user_text));
        prop_assert_eq!(&story.turns[1].source, "agent");
        prop_assert_eq!(&story.turns[1].message, &serde_json::json!(assistant_text));
        prop_assert_eq!(story.turns[1].latency_ms, Some(1250));
    }

    #[test]
    fn public_projection_materializes_session_notes_as_system_turns(
        session in id_strategy(),
        text in proptest::string::string_regex("[A-Za-z0-9 .,!?]{0,48}").unwrap(),
    ) {
        let event = EventRecord {
            identity: EventIdentity::default(),
            seq: 7,
            source: "gateway".into(),
            kind: "note".into(),
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
            payload: serde_json::json!({"text": text.clone()}),
        };
        let story = project_event_records(&[event]).expect("valid note event");
        prop_assert_eq!(story.session_id, session);
        prop_assert_eq!(story.turns.len(), 1);
        prop_assert_eq!(&story.turns[0].source, "system");
        prop_assert_eq!(&story.turns[0].message["kind"], "note");
        prop_assert_eq!(&story.turns[0].message["payload"]["text"], &json!(text));
    }
}

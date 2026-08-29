use persisting_pchronicle::document::project_event_records;
use persisting_pchronicle::model::{EventIdentity, EventRecord};
use proptest::prelude::*;

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
}

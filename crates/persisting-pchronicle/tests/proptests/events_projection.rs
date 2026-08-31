use persisting_pchronicle::document::{events_to_storyline, storyline_to_events};
use persisting_pchronicle::model::StorylineDocument;
use proptest::prelude::*;
use serde_json::json;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_event_projection_recovers_generated_dialogue_pairs(
        session_id in "[A-Za-z0-9._-]{1,24}",
        agent_id in "[A-Za-z0-9._-]{1,24}",
        user_message in "[A-Za-z0-9 .,!?_-]{0,96}",
        agent_message in "[A-Za-z0-9 .,!?_-]{0,96}",
    ) {
        let story = serde_json::from_value::<StorylineDocument>(json!({
            "schema_version": "storyline/v1",
            "session": session_id,
            "agent": {"id": agent_id},
            "turns": [
                {"id": 1, "src": "user", "msg": user_message},
                {"id": 2, "src": "agent", "msg": agent_message},
            ],
        })).unwrap();
        let events = storyline_to_events(&story).unwrap();
        prop_assert_eq!(events.events.len(), 2);
        prop_assert_eq!(&events.events[0].call_id, &events.events[1].call_id);
        let recovered = events_to_storyline(&events).unwrap();
        prop_assert_eq!(&recovered.session_id, &story.session_id);
        prop_assert_eq!(&recovered.agent.id, &story.agent.id);
        prop_assert_eq!(&recovered.turns[0].message, &story.turns[0].message);
        prop_assert_eq!(&recovered.turns[1].message, &story.turns[1].message);
    }

    #[test]
    fn public_event_projection_emits_monotonic_sequences_for_generated_turns(
        session_id in "[A-Za-z0-9._-]{1,24}",
        agent_id in "[A-Za-z0-9._-]{1,24}",
        messages in proptest::collection::vec("[A-Za-z0-9 .,!?_-]{0,64}", 1..8),
    ) {
        let turns = messages.into_iter().enumerate().map(|(index, message)| {
            json!({
                "id": (index + 1),
                "src": if index % 2 == 0 { "user" } else { "agent" },
                "msg": message,
            })
        }).collect::<Vec<_>>();
        let story = serde_json::from_value::<StorylineDocument>(json!({
            "schema_version": "storyline/v1",
            "session": session_id,
            "agent": {"id": agent_id},
            "turns": turns,
        })).unwrap();
        let events = storyline_to_events(&story).unwrap();
        let seqs = events.events.iter().map(|event| event.seq).collect::<Vec<_>>();
        prop_assert_eq!(seqs, (0..story.turns.len()).map(|index| index as u64).collect::<Vec<_>>());
        prop_assert!(events.events.iter().all(|event| event.call_id.is_some()));
    }
}

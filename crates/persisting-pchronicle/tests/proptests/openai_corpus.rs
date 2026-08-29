use persisting_pchronicle::document::{
    DocumentFormat, decode_json_storylines, encode_json_storylines,
};
use proptest::prelude::*;
use serde_json::json;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_openai_codec_roundtrips_generated_dialogue_rows(
        session_id in "[A-Za-z0-9_-]{1,24}",
        step_id in 1i64..1_000_000,
        user_message in "[A-Za-z0-9 .,!?_-]{0,96}",
        agent_message in "[A-Za-z0-9 .,!?_-]{1,96}",
    ) {
        let input = json!({"session_steps": [{
            "session_id": session_id,
            "step_id": step_id,
            "messages": [{"role": "user", "content": user_message}],
            "response": {"role": "assistant", "content": agent_message}
        }]});
        let stories = decode_json_storylines(
            DocumentFormat::OpenaiMsg,
            &input.to_string(),
            "generated.json",
        ).unwrap();
        prop_assert_eq!(stories.len(), 1);
        prop_assert_eq!(stories[0].turns.len(), 2);
        prop_assert_eq!(stories[0].turns[0].id, step_id * 2 - 1);
        prop_assert_eq!(stories[0].turns[1].id, step_id * 2);

        let encoded = encode_json_storylines(DocumentFormat::OpenaiMsg, &stories).unwrap();
        let reparsed = decode_json_storylines(
            DocumentFormat::OpenaiMsg,
            &encoded.to_string(),
            "generated.json",
        ).unwrap();
        prop_assert_eq!(reparsed, stories);
    }
}

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

    #[test]
    fn public_openai_codec_preserves_generated_step_order(
        session_id in "[A-Za-z0-9_-]{1,24}",
        row_count in 1usize..8,
    ) {
        let rows = (0..row_count)
            .map(|index| {
                json!({
                    "session_id": session_id,
                    "step_id": index as i64 + 1,
                    "messages": [{"role": "user", "content": format!("user-{index}")}],
                    "response": {"role": "assistant", "content": format!("agent-{index}")}
                })
            })
            .collect::<Vec<_>>();
        let input = json!({"session_steps": rows});
        let stories = decode_json_storylines(
            DocumentFormat::OpenaiMsg,
            &input.to_string(),
            "generated.json",
        )
        .unwrap();
        prop_assert_eq!(stories.len(), 1);
        prop_assert_eq!(stories[0].turns.len(), row_count * 2);
        for index in 0..row_count {
            let user = &stories[0].turns[index * 2];
            let agent = &stories[0].turns[index * 2 + 1];
            prop_assert_eq!(user.id, index as i64 * 2 + 1);
            prop_assert_eq!(agent.id, index as i64 * 2 + 2);
            prop_assert_eq!(&user.message, &json!(format!("user-{index}")));
            prop_assert_eq!(&agent.message, &json!(format!("agent-{index}")));
        }
    }

    #[test]
    fn public_openai_codec_attaches_historical_tool_results_to_the_call(
        session_id in "[A-Za-z0-9_-]{1,24}",
        call_id in "[A-Za-z0-9_-]{1,24}",
        result in "[A-Za-z0-9 .,!?_-]{1,96}",
    ) {
        let input = json!({"session_steps": [
            {
                "session_id": session_id,
                "step_id": 1,
                "messages": [{"role": "user", "content": "run"}],
                "response": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": call_id,
                        "type": "function",
                        "function": {"name": "inspect", "arguments": "{}"}
                    }]
                }
            },
            {
                "session_id": session_id,
                "step_id": 2,
                "messages": [
                    {"role": "user", "content": "continue"},
                    {"role": "tool", "tool_call_id": call_id, "content": result}
                ],
                "response": {"role": "assistant", "content": "done"}
            }
        ]});
        let stories = decode_json_storylines(DocumentFormat::OpenaiMsg, &input.to_string(), "generated.json").unwrap();
        prop_assert_eq!(stories.len(), 1);
        prop_assert_eq!(stories[0].turns.len(), 4);
        let observation = stories[0].turns[1].observation.as_ref().unwrap();
        prop_assert_eq!(&observation["results"][0]["source_call_id"], &json!(input["session_steps"][0]["response"]["tool_calls"][0]["id"]));
        prop_assert_eq!(&observation["results"][0]["content"], &json!(result));
    }
}

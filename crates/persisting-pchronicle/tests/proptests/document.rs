use persisting_pchronicle::document::{
    DocumentFormat, InputIssueKind, decode_json_storylines, encode_json_storylines,
};
use persisting_pchronicle::model::{StorylineDocument, StorylineTurn};
use proptest::prelude::*;

fn token_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9._-]{1,32}").unwrap()
}

fn text_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..96)
        .prop_map(|characters| characters.into_iter().collect())
}

fn story_strategy() -> impl Strategy<Value = StorylineDocument> {
    (
        token_strategy(),
        token_strategy(),
        prop::option::of(token_strategy()),
        proptest::collection::vec(
            (
                prop::sample::select(vec!["user", "agent", "system"]),
                text_strategy(),
            ),
            0..8,
        ),
    )
        .prop_map(|(session_id, agent_id, trajectory_id, turns)| {
            let mut story = StorylineDocument::new(session_id, agent_id);
            story.trajectory_id = trajectory_id;
            story.turns = turns
                .into_iter()
                .enumerate()
                .map(|(id, (source, message))| StorylineTurn {
                    id: id as i64,
                    kind: None,
                    timestamp: None,
                    source: source.into(),
                    message: serde_json::Value::String(message),
                    reasoning_content: None,
                    reasoning_effort: None,
                    tool_calls: None,
                    observation: None,
                    metrics: None,
                    model_name: None,
                    llm_call_count: None,
                    is_copied_context: None,
                    latency_ms: None,
                    ttft_ms: None,
                    extra: None,
                    env: None,
                    prompt: None,
                    finished_at: None,
                })
                .collect();
            story
        })
}

fn atif_fixture_value() -> serde_json::Value {
    serde_json::json!({
        "schema_version": "ATIF-v1.7",
        "trajectory_id": "one",
        "agent": {"name": "agent", "version": "1"},
        "steps": []
    })
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_storyline_codec_preserves_generated_documents_and_order(
        stories in prop::collection::vec(story_strategy(), 1..7),
    ) {
        let encoded = encode_json_storylines(DocumentFormat::Storyline, &stories).unwrap();
        let decoded = decode_json_storylines(
            DocumentFormat::Storyline,
            &encoded.to_string(),
            "generated.storyline.json",
        ).unwrap();
        prop_assert_eq!(decoded, stories);
    }

    #[test]
    fn public_storyline_codec_rejects_empty_collections(
        stories in prop::collection::vec(story_strategy(), 0..1),
    ) {
        let encoded = encode_json_storylines(DocumentFormat::Storyline, &stories).unwrap();
        let error = decode_json_storylines(
            DocumentFormat::Storyline,
            &encoded.to_string(),
            "generated.storyline.json",
        ).unwrap_err();
        prop_assert_eq!(error.kind(), InputIssueKind::Unsupported);
        prop_assert!(error.message().contains("cannot be empty"));
    }

    #[test]
    fn public_atif_object_and_singleton_array_have_identical_canonical_encoding(
        trajectory_id in proptest::string::string_regex("[a-zA-Z0-9._-]{1,32}").unwrap(),
    ) {
        let mut object = atif_fixture_value();
        object["trajectory_id"] = serde_json::json!(trajectory_id);
        let object_input = object.to_string();
        let array_input = serde_json::json!([object]).to_string();

        let from_object = decode_json_storylines(
            DocumentFormat::Atif,
            &object_input,
            "generated.atif.json",
        ).unwrap();
        let from_array = decode_json_storylines(
            DocumentFormat::Atif,
            &array_input,
            "generated.atif.json",
        ).unwrap();

        prop_assert_eq!(
            encode_json_storylines(DocumentFormat::Atif, &from_object).unwrap(),
            encode_json_storylines(DocumentFormat::Atif, &from_array).unwrap(),
        );
    }

    #[test]
    fn public_atif_jsonl_decodes_every_nonempty_generated_record(
        sessions in proptest::collection::vec(
            proptest::string::string_regex("[a-zA-Z0-9._-]{1,24}").unwrap(),
            1..7,
        ),
    ) {
        let input = sessions
            .iter()
            .enumerate()
            .map(|(index, session)| {
                serde_json::json!({
                    "schema_version": "ATIF-v1.7",
                    "trajectory_id": format!("{session}-{index}"),
                    "agent": {"name": "agent", "version": "1"},
                    "steps": []
                }).to_string()
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let decoded = decode_json_storylines(DocumentFormat::Atif, &input, "generated.jsonl").unwrap();
        prop_assert_eq!(decoded.len(), sessions.len());
        for (index, session) in sessions.iter().enumerate() {
            prop_assert_eq!(&decoded[index].session_id, &format!("{session}-{index}"));
        }
    }
}

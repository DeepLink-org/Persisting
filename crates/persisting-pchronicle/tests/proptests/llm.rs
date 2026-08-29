use persisting_pchronicle::model::{
    LlmContentPart, LlmGenerationParams, LlmImageSource, LlmMessage, LlmProtocol, LlmRequest,
    LlmRequestEventPayload, LlmRole, LlmStreamEvent, LlmToolDefinition, LlmUsage,
};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn token_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9 _:/-]{0,24}").unwrap()
}

fn role_strategy() -> impl Strategy<Value = LlmRole> {
    prop::sample::select(vec![
        LlmRole::System,
        LlmRole::Developer,
        LlmRole::User,
        LlmRole::Assistant,
        LlmRole::Tool,
    ])
}

fn content_part_strategy() -> impl Strategy<Value = LlmContentPart> {
    prop_oneof![
        token_strategy().prop_map(LlmContentPart::text),
        token_strategy().prop_map(|url| LlmContentPart::Image {
            source: LlmImageSource::Url { url },
            media_type: None,
            detail: None,
        }),
        token_strategy().prop_map(|text| LlmContentPart::Reasoning {
            text: Some(text),
            signature: None,
        }),
        token_strategy().prop_map(|id| LlmContentPart::ToolCall {
            id,
            name: "tool".into(),
            arguments: serde_json::json!({"ok": true}),
            signature: None,
        }),
    ]
}

fn message_strategy() -> impl Strategy<Value = LlmMessage> {
    (
        role_strategy(),
        proptest::collection::vec(content_part_strategy(), 0..4),
        token_strategy(),
    )
        .prop_map(|(role, parts, name)| LlmMessage {
            role,
            parts,
            name: (!name.is_empty()).then_some(name),
            extensions: BTreeMap::new(),
        })
}

fn request_strategy() -> impl Strategy<Value = LlmRequest> {
    (
        prop::option::of(token_strategy()),
        proptest::collection::vec(message_strategy(), 0..12),
        proptest::collection::vec(token_strategy(), 0..4),
        any::<bool>(),
    )
        .prop_map(|(model, messages, tool_names, stream)| LlmRequest {
            model,
            system: Vec::new(),
            messages,
            tools: tool_names
                .into_iter()
                .map(|name| LlmToolDefinition {
                    kind: "function".into(),
                    name,
                    description: None,
                    input_schema: serde_json::json!({"type": "object"}),
                    strict: None,
                    extensions: BTreeMap::new(),
                })
                .collect(),
            tool_choice: None,
            generation: LlmGenerationParams::default(),
            response_format: None,
            stream,
            metadata: None,
            extensions: BTreeMap::new(),
        })
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_request_payloads_roundtrip_losslessly(
        protocol in prop::sample::select(vec![
            LlmProtocol::ChatCompletions,
            LlmProtocol::Messages,
            LlmProtocol::Responses,
            LlmProtocol::Gemini,
            LlmProtocol::Unknown,
        ]),
        request in request_strategy(),
    ) {
        let payload = LlmRequestEventPayload { input_format: protocol, request };
        let encoded = serde_json::to_value(&payload).unwrap();
        prop_assert_eq!(serde_json::from_value::<LlmRequestEventPayload>(encoded).unwrap(), payload);
    }

    #[test]
    fn public_request_summaries_match_message_content(request in request_strategy()) {
        let expected_visible = request.messages.iter().filter(|message| {
            message.role == LlmRole::User && message.parts.iter().any(|part| {
                matches!(part, LlmContentPart::Text { text, .. } if !text.trim().is_empty())
                    || matches!(part, LlmContentPart::Image { .. })
            })
        }).count();
        prop_assert_eq!(request.visible_user_turns(), expected_visible);

        let expected_latest = request.messages.iter().rev().find_map(|message| {
            (message.role == LlmRole::User).then(|| {
                message.parts.iter().filter_map(|part| match part {
                    LlmContentPart::Text { text, .. } if !text.trim().is_empty() => Some(text.as_str()),
                    _ => None,
                }).collect::<Vec<_>>().join("\n")
            }).filter(|text| !text.is_empty())
        });
        prop_assert_eq!(request.latest_user_text(), expected_latest);
        prop_assert_eq!(request.tool_names(), request.tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn public_stream_events_roundtrip_with_their_explicit_variant(
        candidate in 0usize..4,
        text in token_strategy(),
        id in token_strategy(),
        reason in prop::option::of(token_strategy()),
        usage in (0u64..1000, 0u64..1000, 0u64..2000).prop_map(|(input, output, total)| LlmUsage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: total,
            ..LlmUsage::default()
        }),
    ) {
        let variants = vec![
            LlmStreamEvent::TextDelta { candidate, text: text.clone() },
            LlmStreamEvent::ReasoningDelta { candidate, text: text.clone() },
            LlmStreamEvent::ToolCallStart { candidate, id: id.clone(), name: text.clone(), signature: reason.clone() },
            LlmStreamEvent::ToolArgumentsDelta { candidate, id: id.clone(), delta: text.clone() },
            LlmStreamEvent::Usage { usage: usage.clone() },
            LlmStreamEvent::Finish { candidate, reason: reason.clone() },
            LlmStreamEvent::Error { message: text.clone(), code: reason.clone() },
        ];
        for event in variants {
            let encoded = serde_json::to_value(&event).unwrap();
            prop_assert_eq!(serde_json::from_value::<LlmStreamEvent>(encoded).unwrap(), event);
        }
    }

    #[test]
    fn public_tool_names_preserve_order_and_duplicates(request in request_strategy()) {
        let expected = request.tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>();
        prop_assert_eq!(request.tool_names(), expected);
    }

    #[test]
    fn public_visible_user_turns_never_exceed_message_count(request in request_strategy()) {
        prop_assert!(request.visible_user_turns() <= request.messages.len());
    }

    #[test]
    fn public_content_parts_roundtrip_without_reordering(
        parts in proptest::collection::vec(content_part_strategy(), 0..16),
    ) {
        let encoded = serde_json::to_value(&parts).unwrap();
        let decoded = serde_json::from_value::<Vec<LlmContentPart>>(encoded).unwrap();
        prop_assert_eq!(decoded, parts);
    }

    #[test]
    fn public_llm_roles_use_stable_snake_case_wire_names(role in role_strategy()) {
        let expected = match role {
            LlmRole::System => "system",
            LlmRole::Developer => "developer",
            LlmRole::User => "user",
            LlmRole::Assistant => "assistant",
            LlmRole::Tool => "tool",
        };
        prop_assert_eq!(serde_json::to_value(&role).unwrap(), serde_json::Value::String(expected.into()));
    }

    #[test]
    fn public_usage_total_is_at_least_each_component(
        input_tokens in 0u64..1_000_000,
        output_tokens in 0u64..1_000_000,
    ) {
        let usage = LlmUsage {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens.saturating_add(output_tokens),
            ..LlmUsage::default()
        };
        prop_assert!(usage.total_tokens >= usage.input_tokens);
        prop_assert!(usage.total_tokens >= usage.output_tokens);
    }
}

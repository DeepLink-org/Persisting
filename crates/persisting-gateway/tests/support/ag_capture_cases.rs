//! Expected capture outcomes for agentgateway fixture files.

/// User-turn extraction expectations for a request fixture.
pub struct UserCaptureCase {
    pub path: &'static str,
    pub min_turns: usize,
    pub must_contain: &'static [&'static str],
}

pub const USER_CAPTURE_CASES: &[UserCaptureCase] = &[
    UserCaptureCase {
        path: "requests/completions/basic.json",
        min_turns: 1,
        must_contain: &[],
    },
    UserCaptureCase {
        path: "requests/completions/full.json",
        min_turns: 1,
        must_contain: &["What's in this image?", "[image: url:"],
    },
    UserCaptureCase {
        path: "requests/completions/tool-call.json",
        min_turns: 1,
        must_contain: &["Columbus"],
    },
    UserCaptureCase {
        path: "requests/completions/parallel-tool-call.json",
        min_turns: 1,
        must_contain: &["Columbus"],
    },
    UserCaptureCase {
        path: "requests/messages/basic.json",
        min_turns: 1,
        must_contain: &["Hello"],
    },
    UserCaptureCase {
        path: "requests/messages/tools.json",
        min_turns: 1,
        must_contain: &[],
    },
    UserCaptureCase {
        path: "requests/responses/basic.json",
        min_turns: 1,
        must_contain: &[],
    },
    UserCaptureCase {
        path: "requests/responses/assistant-history.json",
        min_turns: 1,
        must_contain: &["Follow up question"],
    },
    UserCaptureCase {
        path: "local/requests/responses/codex_basic.json",
        min_turns: 1,
        must_contain: &[],
    },
    UserCaptureCase {
        path: "local/requests/responses/codex_tool_roundtrip.json",
        min_turns: 1,
        must_contain: &["```tool_result:", "Cargo.toml"],
    },
];

/// Assistant text extraction from JSON response bodies.
pub struct AssistantJsonCase {
    pub path: &'static str,
    pub must_contain: &'static [&'static str],
}

pub const ASSISTANT_JSON_CASES: &[AssistantJsonCase] = &[
    AssistantJsonCase {
        path: "response/completions/basic.json",
        must_contain: &["Sorry"],
    },
    AssistantJsonCase {
        path: "local/response/completions/tool_call.json",
        must_contain: &["```tool:shell", "ls"],
    },
    AssistantJsonCase {
        path: "response/anthropic/basic.json",
        must_contain: &["Hi there"],
    },
    AssistantJsonCase {
        path: "response/anthropic/tool.json",
        must_contain: &["```tool:get_weather", "San Francisco"],
    },
    AssistantJsonCase {
        path: "response/responses/basic.json",
        must_contain: &["Hello"],
    },
];

/// Assistant extraction from SSE wire fixtures (OpenAI completions or Responses).
pub struct AssistantSseCase {
    pub path: &'static str,
    pub translate_completions_to_messages: bool,
    pub must_contain: &'static [&'static str],
}

pub const ASSISTANT_SSE_CASES: &[AssistantSseCase] = &[
    AssistantSseCase {
        path: "local/response/completions/stream_head.txt",
        translate_completions_to_messages: true,
        must_contain: &["Hi"],
    },
    AssistantSseCase {
        path: "local/response/completions/stream_tool_call.txt",
        translate_completions_to_messages: false,
        must_contain: &["```tool:shell", "ls"],
    },
    AssistantSseCase {
        path: "response/completions/stream.json",
        translate_completions_to_messages: true,
        must_contain: &["Hi", "help"],
    },
    AssistantSseCase {
        path: "response/responses/stream-image.json",
        translate_completions_to_messages: false,
        must_contain: &["[image_generated:", "gray tabby cat"],
    },
];

/// Fixtures that should yield non-zero token usage when parsed.
pub const USAGE_JSON_FIXTURES: &[&str] = &[
    "response/completions/basic.json",
    "response/completions/gemini_with_completion_tokens.json",
    "response/completions/gemini_zero_completion_tokens.json",
    "response/anthropic/basic.json",
    "response/anthropic/tool.json",
    "response/responses/basic.json",
];

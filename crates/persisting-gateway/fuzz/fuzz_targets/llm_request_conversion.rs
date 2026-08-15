#![no_main]

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use persisting_gateway::conversion::{
    completions_request_to_gemini, completions_response_to_messages,
    gemini_response_to_completions, messages_request_to_completions,
    responses_request_to_completions,
};
use persisting_gateway::protocol::ProtocolKind;
use persisting_gateway::understanding::understand_request;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, body)) = data.split_first() else {
        return;
    };
    let body = Bytes::copy_from_slice(body);
    match selector % 5 {
        0 => {
            let _ = understand_request(ProtocolKind::Messages, &body);
            let _ = messages_request_to_completions(&body, "gpt-5.6-terra");
        }
        1 => {
            let _ = understand_request(ProtocolKind::Responses, &body);
            let _ = responses_request_to_completions(&body, "deepseek-chat", None);
        }
        2 => {
            let _ = understand_request(ProtocolKind::ChatCompletions, &body);
            let _ = completions_response_to_messages(&body, "claude-client");
        }
        3 => {
            let _ = completions_request_to_gemini(&body, "gemini-2.5-pro");
        }
        _ => {
            let _ = gemini_response_to_completions(&body, "gemini-2.5-pro");
        }
    }
});

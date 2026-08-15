#![no_main]

use libfuzzer_sys::fuzz_target;
use persisting_gateway::conversion::{
    CompletionsStreamTranslator, CompletionsToResponsesStreamTranslator,
    GeminiNativeStreamTranslator,
};

fuzz_target!(|data: &[u8]| {
    let mut messages = CompletionsStreamTranslator::new("claude-client");
    let mut responses = CompletionsToResponsesStreamTranslator::new("gpt-client");
    let mut gemini = GeminiNativeStreamTranslator::new("gemini-2.5-pro");
    for chunk in data.chunks(37) {
        let _ = messages.push_chunk(chunk);
        let _ = responses.push_chunk(chunk);
        let _ = gemini.push_chunk(chunk);
    }
    let _ = messages.finish_stream();
    let _ = responses.finish_stream();
    let _ = gemini.finish_stream();
});

#![no_main]

use libfuzzer_sys::fuzz_target;
use persisting_gateway::conversion::{ProtocolBridge, StreamTranslator};
use persisting_gateway::protocol::ProtocolKind;

fuzz_target!(|data: &[u8]| {
    let bridges = [
        (ProtocolBridge::Passthrough, ProtocolKind::ChatCompletions),
        (
            ProtocolBridge::MessagesToCompletions,
            ProtocolKind::Messages,
        ),
        (
            ProtocolBridge::ResponsesToCompletions,
            ProtocolKind::Responses,
        ),
        (
            ProtocolBridge::CompletionsToGemini,
            ProtocolKind::ChatCompletions,
        ),
        (ProtocolBridge::MessagesToGemini, ProtocolKind::Messages),
        (ProtocolBridge::ResponsesToGemini, ProtocolKind::Responses),
    ];
    let chunk_size = data.first().map_or(1, |byte| usize::from(*byte).max(1));
    for (bridge, protocol) in bridges {
        let Some(mut translator) = StreamTranslator::new(bridge, protocol, "fuzz-model") else {
            continue;
        };
        let mut output = Vec::new();
        let mut succeeded = true;
        for chunk in data.chunks(chunk_size) {
            match translator.push_chunk(chunk) {
                Ok(rendered) => output.extend_from_slice(&rendered),
                Err(_) => succeeded = false,
            }
        }
        match translator.finish_stream() {
            Ok(rendered) => output.extend_from_slice(&rendered),
            Err(_) => succeeded = false,
        }
        let _ = translator.semantic_response();
        if bridge == ProtocolBridge::Passthrough {
            assert!(succeeded);
            assert_eq!(output, data);
        }
    }
});

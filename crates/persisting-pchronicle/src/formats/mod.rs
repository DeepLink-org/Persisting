//! Codecs for each [`crate::ChronicleFormat`].

pub mod actf;
pub mod detect;
pub mod events;
pub mod llm;
pub mod openai_corpus;
pub mod openai_msg;
pub mod storyline;

pub use actf::{
    parse_actf_document, ActfAssistantContent, ActfAttempt, ActfDocument, ActfMetric,
    ActfObservation, ActfStep, ActfToolCall, ActfTrajectory, ACTF_SCHEMA_VERSION,
};
pub use detect::detect_format;
pub use events::{
    events_lance_only_error, events_lance_only_message, export_events_json_pretty,
    export_events_jsonl, ChronicleEventRecordExt, EventIdentity, EventRecord, EventsDocument,
};
pub use llm::{
    LlmCandidate, LlmContentPart, LlmExtensions, LlmGenerationParams, LlmImageSource, LlmMessage,
    LlmProtocol, LlmRequest, LlmRequestEventPayload, LlmResponse, LlmResponseEventPayload,
    LlmResponseFormat, LlmRole, LlmStreamEvent, LlmToolChoice, LlmToolChoiceMode,
    LlmToolDefinition, LlmUsage,
};
pub use openai_corpus::{
    is_lossless_openai_storyline, parse_openai_msg_corpus_value, recover_openai_msg_files,
    OpenaiMsgCorpusReader, RecoveredOpenaiMsgFile,
};
pub use openai_msg::{parse_openai_msg_document, OpenaiMsgDocument, OpenaiMsgStep};
pub use storyline::{
    parse_storyline_document, FieldPresence, StoryLink, StorylineAgent, StorylineDocument,
    StorylineToolCall, StorylineTurn,
};

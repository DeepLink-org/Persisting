//! Codecs for each [`crate::ChronicleFormat`].

pub mod actf;
pub mod detect;
pub mod events;
pub mod llm;
pub mod openai_corpus;
pub mod openai_msg;
pub mod storyline;

pub use crate::agenticmd::{
    agenticmd_body_byte_offset, encode_agenticmd_block, encode_agenticmd_document,
    encode_agenticmd_preamble, parse_agenticmd_blocks_with_spans, parse_agenticmd_document,
    AgenticmdBlock, AgenticmdBlockSpan, AgenticmdDocument, AgenticmdHeader, AGENTICMD_BLOCK_LAYOUT,
    AGENTICMD_FORMAT_NAME, AGENTICMD_FRONTMATTER_FORMAT, BLOCK_MARKER,
};
pub use crate::agenticmd::{
    append_subagent_refs_footer, is_subagent_footer_line, strip_subagent_footer_from_body,
};
pub use crate::agenticmd::{
    block_speaker, validate_agenticmd_block, validate_speaker, validate_type_name,
};
pub use crate::agenticmd::{
    encode_agenticmd_session_frontmatter, AgenticmdClientMeta, AgenticmdSessionFrontmatter,
};
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

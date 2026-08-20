//! pChronicle 的权威领域模型。

pub use crate::formats::events::{
    ChronicleEventRecordExt, EventIdentity, EventRecord, EventsDocument,
};
pub use crate::formats::llm::{
    LlmCandidate, LlmContentPart, LlmExtensions, LlmGenerationParams, LlmImageSource, LlmMessage,
    LlmProtocol, LlmRequest, LlmRequestEventPayload, LlmResponse, LlmResponseEventPayload,
    LlmResponseFormat, LlmRole, LlmStreamEvent, LlmToolChoice, LlmToolChoiceMode,
    LlmToolDefinition, LlmUsage,
};
pub use crate::formats::storyline::{
    StoryLink, StorylineAgent, StorylineDocument, StorylineToolCall, StorylineTurn,
};
pub use crate::formats::unknown_fields::{
    compute_unknown_key_counts, validate_json_pointer, validate_unknown_fields,
    validate_unknown_fields_with, SourceUnknownFields, StorylineUnknownFields, UnknownFieldCounts,
    UnknownFieldLimits, UnknownKeyCounts, DEFAULT_MAX_UNKNOWN_BYTES, DEFAULT_MAX_UNKNOWN_FIELDS,
};

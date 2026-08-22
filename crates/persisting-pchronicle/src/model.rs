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
    StoryLink, StorylineAgent, StorylineDocument, StorylineEnv, StorylineOrigin, StorylinePrompt,
    StorylineTask, StorylineTaskLlm, StorylineTaskResult, StorylineToolCall, StorylineToolResponse,
    StorylineTurn, STORYLINE_SCHEMA_VERSION,
};
pub use crate::formats::timestamp::StorylineTimestamp;
pub use crate::formats::unknown_fields::{
    compute_unknown_key_counts, validate_json_pointer, validate_unknown_fields,
    validate_unknown_fields_with, SourceUnknownFields, StorylineUnknownFields, UnknownFieldCounts,
    UnknownFieldImportWarnings, UnknownFieldLimits, UnknownKeyCounts, DEFAULT_MAX_UNKNOWN_BYTES,
    DEFAULT_MAX_UNKNOWN_FIELDS,
};

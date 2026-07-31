//! Codecs for each [`crate::ChronicleFormat`].

pub mod agenticmd;
pub mod agenticmd_body;
pub mod agenticmd_validate;
pub mod detect;
pub mod events;
pub mod openai_msg;
pub mod storyline;

pub use agenticmd::{
    agenticmd_body_byte_offset, encode_agenticmd_block, encode_agenticmd_document,
    encode_agenticmd_preamble, parse_agenticmd_blocks_with_spans, parse_agenticmd_document,
    parse_agenticmd_document_with, AgenticmdBlock, AgenticmdBlockSpan, AgenticmdDocument,
    AgenticmdHeader, AgenticmdParseMode, AGENTICMD_BLOCK_LAYOUT, AGENTICMD_FORMAT_NAME,
    AGENTICMD_FRONTMATTER_FORMAT, BLOCK_MARKER,
};
pub use agenticmd_body::{
    append_subagent_refs_footer, is_subagent_footer_line, strip_subagent_footer_from_body,
    BLOCK_FORMAT_BLOCK, BLOCK_FORMAT_VERSION,
};
pub use agenticmd_validate::{
    block_speaker, validate_agenticmd_block, validate_speaker, validate_type_name,
};
pub use detect::detect_format;
pub use events::{
    events_lance_only_error, events_lance_only_message, export_events_json_pretty,
    export_events_jsonl, EventRecord, EventsDocument,
};
pub use openai_msg::{
    parse_openai_msg_document, OpenaiMsgDocument, OpenaiMsgStep, OPENAI_MSG_FORMAT_VERSION,
};
pub use storyline::{
    parse_storyline_document, StoryLink, StorylineAgent, StorylineDocument, StorylineToolCall,
    StorylineTurn, STORYLINE_SCHEMA_VERSION,
};

//! Codecs for each [`crate::ChronicleFormat`].

pub mod agenticmd;
pub mod detect;
pub mod events;
pub mod openai_msg;
pub mod storyline;

pub use agenticmd::{
    encode_agenticmd_document, parse_agenticmd_document, AgenticmdBlock, AgenticmdDocument,
    AgenticmdHeader,
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

//! Codecs for each [`crate::DocumentFormat`].

pub mod actf;
pub mod detect;
pub mod events;
pub mod llm;
pub mod openai_corpus;
pub mod openai_msg;
pub mod storyline;

pub use actf::parse_actf_document;
pub use detect::detect_format;
pub use events::{
    events_lance_only_message, export_events_json_pretty, export_events_jsonl, EventIdentity,
    EventRecord,
};
pub use openai_corpus::{
    is_lossless_openai_storyline, parse_openai_msg_corpus_value, recover_openai_msg_files,
};
pub use openai_msg::parse_openai_msg_document;
pub use storyline::{
    parse_storyline_document, FieldPresence, StoryLink, StorylineAgent, StorylineDocument,
    StorylineToolCall, StorylineTurn,
};

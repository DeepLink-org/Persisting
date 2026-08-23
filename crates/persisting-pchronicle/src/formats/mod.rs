//! Codecs for each [`crate::document::DocumentFormat`].

pub mod actf;
pub(crate) mod atif;
pub(crate) mod claude_code;
pub(crate) mod codec;
pub(crate) mod codex;
pub(crate) mod common;
pub mod detect;
pub mod events;
pub mod llm;
pub mod openai_corpus;
pub(crate) mod registry;
pub mod storyline;
pub mod timestamp;
pub mod unknown_fields;

pub use detect::detect_format;
pub use events::{EventIdentity, EventRecord};
pub use openai_corpus::parse_openai_msg_corpus_value;
pub use storyline::StorylineDocument;

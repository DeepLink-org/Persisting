//! Codecs for each [`crate::document::DocumentFormat`].

pub mod actf;
pub mod detect;
pub mod events;
pub mod llm;
pub mod openai_corpus;
pub mod storyline;
pub mod timestamp;
pub mod unknown_fields;

pub use detect::detect_format;
pub use events::{EventIdentity, EventRecord};
pub(crate) use openai_corpus::has_openai_provenance;
pub use openai_corpus::{parse_openai_msg_corpus_value, recover_openai_msg_files};
pub use storyline::StorylineDocument;

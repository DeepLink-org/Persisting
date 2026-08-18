//! Codecs for each [`crate::DocumentFormat`].

pub mod actf;
pub mod detect;
pub mod events;
pub mod llm;
pub mod openai_corpus;
pub mod storyline;

pub use detect::detect_format;
pub use events::{EventIdentity, EventRecord};
pub(crate) use openai_corpus::has_openai_provenance;
pub use openai_corpus::{
    parse_openai_msg_corpus_value, recover_openai_msg_files, synthesize_openai_msg_corpus,
};
pub use storyline::{StorylineCollectionShape, StorylineDocument};

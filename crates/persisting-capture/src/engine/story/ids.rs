//! Strong ids for the run → story → turn → call narrative model.

use serde::{Deserialize, Serialize};

/// One capture workspace (run directory / `root_session`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub String);

impl RunId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One agent narrative line (≈ former `seq_key` / one trajectory file).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StoryId(pub String);

impl StoryId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn from_seq_key(seq_key: impl Into<String>) -> Self {
        Self(seq_key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One dialogue turn within a story (user → assistant round).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TurnId(pub String);

impl TurnId {
    pub fn new(story_id: &StoryId, index: u32) -> Self {
        Self(format!("{}#turn-{index}", story_id.as_str()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One LLM HTTP round-trip (`call_id`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CallId(pub String);

impl CallId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for CallId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for CallId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

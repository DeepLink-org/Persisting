//! Block types and on-disk layout constants.

use std::collections::BTreeMap;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// Legacy numbered session file (pre `{session_id}.md` layout).
pub const SESSION_MARKDOWN_FILENAME: &str = "0001.md";

/// Max filename stem length for `{session_id}.md` on disk.
pub(crate) const SESSION_FILENAME_MAX_LEN: usize = 128;

/// Legacy filename; still recognized for read / import.
pub const LEGACY_TRAJECTORY_MARKDOWN_FILENAME: &str = "trajectory.tlv.md";

pub const BLOCK_MARKER: &str = "<!-- persisting:block";
pub(crate) const BLOCK_LAYOUT: &str =
    "<!-- persisting:block:{speaker} {json} -->\n\nmessage body\n\n";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    #[serde(rename = "type")]
    pub type_name: String,
    pub length: usize,
    #[serde(flatten)]
    pub fields: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct MarkdownBlock {
    pub header: BlockHeader,
    pub body: Vec<u8>,
}

impl MarkdownBlock {
    pub fn type_name(&self) -> &str {
        &self.header.type_name
    }

    pub fn value_utf8(&self) -> anyhow::Result<&str> {
        std::str::from_utf8(&self.body).context("block body must be UTF-8")
    }

    pub fn role(&self) -> Option<&str> {
        self.header.fields.get("role").and_then(|v| v.as_str())
    }

    pub fn kind(&self) -> Option<&str> {
        self.header.fields.get("kind").and_then(|v| v.as_str())
    }

    pub fn turn(&self) -> Option<u64> {
        self.header.fields.get("turn").and_then(|v| v.as_u64())
    }
}

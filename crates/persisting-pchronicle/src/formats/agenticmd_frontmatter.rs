//! Typed metadata contract for AgenticMD trajectory frontmatter.

use serde::{Deserialize, Serialize};

use super::{encode_agenticmd_preamble, AGENTICMD_BLOCK_LAYOUT, AGENTICMD_FRONTMATTER_FORMAT};
use crate::Result;

/// Producer/client provenance embedded in an AgenticMD document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgenticmdClientMeta {
    pub peer: String,
    pub peer_port: u16,
    pub pid: u32,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_fp: Option<String>,
}

/// Best-effort session rollup using Storyline-compatible field names.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgenticmdSessionFrontmatter {
    #[serde(rename = "session_id")]
    pub session: String,
    #[serde(rename = "agent_id")]
    pub agent: String,
    #[serde(rename = "model_name", skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(rename = "started_at", skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    #[serde(rename = "turn_count", default, skip_serializing_if = "is_zero")]
    pub turns: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    #[serde(
        rename = "child_session_ids",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub subagents: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<AgenticmdClientMeta>,
}

#[derive(Serialize)]
struct FrontmatterDocument<'a> {
    format: &'static str,
    block: &'static str,
    #[serde(flatten)]
    summary: &'a AgenticmdSessionFrontmatter,
}

pub fn encode_agenticmd_session_frontmatter(
    summary: &AgenticmdSessionFrontmatter,
) -> Result<String> {
    encode_agenticmd_preamble(&FrontmatterDocument {
        format: AGENTICMD_FRONTMATTER_FORMAT,
        block: AGENTICMD_BLOCK_LAYOUT,
        summary,
    })
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_frontmatter_uses_canonical_agenticmd_contract() {
        let encoded = encode_agenticmd_session_frontmatter(&AgenticmdSessionFrontmatter {
            session: "s1".into(),
            agent: "a1".into(),
            turns: 2,
            client: Some(AgenticmdClientMeta {
                peer: "127.0.0.1:1234".into(),
                peer_port: 1234,
                pid: 42,
                command: "agent".into(),
                machine_fp: None,
            }),
            ..Default::default()
        })
        .unwrap();
        assert!(encoded.contains("format: persisting"));
        assert!(encoded.contains("session_id: s1"));
        assert!(encoded.contains("turn_count: 2"));
        assert!(encoded.contains("client:"));
        assert!(!encoded.contains("total_tokens:"));
    }

    #[test]
    fn legacy_short_frontmatter_names_are_rejected() {
        let legacy = serde_json::json!({
            "session": "s1",
            "agent": "a1"
        });
        assert!(serde_json::from_value::<AgenticmdSessionFrontmatter>(legacy).is_err());
    }
}

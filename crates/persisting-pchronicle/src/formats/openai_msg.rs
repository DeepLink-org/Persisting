//! `openai_msg` format — dlcapt session_steps with OpenAI Chat Completions messages.
//!
//! Matches the authoritative `session_steps.json` envelope produced by
//! `persisting-dlcapt`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

/// One dlcapt / TransferQueue-compatible step with embedded OpenAI messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenaiMsgStep {
    pub id: String,
    pub session_id: String,
    pub step_id: i64,
    #[serde(default)]
    pub job_id: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub group_id: String,
    #[serde(default)]
    pub env_name: String,
    #[serde(default)]
    pub llm_model: String,
    #[serde(default)]
    pub step_reward: f64,
    #[serde(default)]
    pub reward: f64,
    #[serde(default)]
    pub is_terminal: bool,
    #[serde(default)]
    pub is_truncated: bool,
    #[serde(default)]
    pub is_session_completed: bool,
    #[serde(default)]
    pub is_trainable: bool,
    #[serde(default)]
    pub created_at: String,
    /// OpenAI Chat Completions `messages` array (preferred decoded form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Value>,
    /// Assistant `response` message object (preferred decoded form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    /// Raw string form used by Lance / StepRecord (`messages_json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_state_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_json: Option<String>,
    #[serde(default)]
    pub run_bucket: String,
    #[serde(default)]
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_export_id: Option<i64>,
}

impl OpenaiMsgStep {
    /// Resolve the OpenAI `messages` array from either decoded or string fields.
    pub fn messages_value(&self) -> Result<Value> {
        if let Some(v) = &self.messages {
            return Ok(v.clone());
        }
        if let Some(s) = &self.messages_json {
            return Ok(serde_json::from_str(s)?);
        }
        Err(Error::Other(
            "openai_msg step missing messages / messages_json".into(),
        ))
    }

    pub fn response_value(&self) -> Result<Option<Value>> {
        if let Some(v) = &self.response {
            return Ok(Some(v.clone()));
        }
        if let Some(s) = &self.response_json {
            if s.is_empty() {
                return Ok(None);
            }
            return Ok(Some(serde_json::from_str(s)?));
        }
        Ok(None)
    }
}

/// dlcapt `session_steps.json` envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenaiMsgDocument {
    pub session_id: String,
    #[serde(default)]
    pub session_dir: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub run_bucket: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default = "default_authoritative")]
    pub authoritative: String,
    #[serde(default)]
    pub session_metadata: serde_json::Map<String, Value>,
    #[serde(default)]
    pub session_steps: Vec<OpenaiMsgStep>,
}

fn default_source() -> String {
    "dlcapt-proxy".into()
}

fn default_authoritative() -> String {
    "json_file".into()
}

impl OpenaiMsgDocument {
    pub const FORMAT_NAME: &'static str = "openai_msg";

    pub fn new(session_id: impl Into<String>, steps: Vec<OpenaiMsgStep>) -> Self {
        let session_id = session_id.into();
        let agent_id = steps
            .first()
            .map(|s| s.agent_id.clone())
            .unwrap_or_default();
        let run_bucket = steps
            .first()
            .map(|s| s.run_bucket.clone())
            .unwrap_or_default();
        Self {
            session_id: session_id.clone(),
            session_dir: session_id,
            agent_id,
            run_bucket,
            source: default_source(),
            authoritative: default_authoritative(),
            session_metadata: Default::default(),
            session_steps: steps,
        }
    }
}

pub fn parse_openai_msg_document(input: &str) -> Result<OpenaiMsgDocument> {
    let mut doc: OpenaiMsgDocument = serde_json::from_str(input)?;
    // Normalize: if producers only wrote messages_json, keep both forms available.
    for step in &mut doc.session_steps {
        if step.messages.is_none() {
            if let Some(s) = &step.messages_json {
                step.messages = Some(serde_json::from_str(s)?);
            }
        }
        if step.response.is_none() {
            if let Some(s) = &step.response_json {
                if !s.is_empty() {
                    step.response = Some(serde_json::from_str(s)?);
                }
            }
        }
    }
    Ok(doc)
}

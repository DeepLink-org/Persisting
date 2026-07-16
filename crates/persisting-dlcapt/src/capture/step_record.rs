use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepRecord {
    pub id: String,
    pub session_id: String,
    pub step_id: i64,
    pub job_id: String,
    pub agent_id: String,
    pub group_id: String,
    pub env_name: String,
    pub llm_model: String,
    pub step_reward: f64,
    pub reward: f64,
    pub is_terminal: bool,
    pub is_truncated: bool,
    pub is_session_completed: bool,
    pub is_trainable: bool,
    pub created_at: String,
    pub messages_json: String,
    pub response_json: String,
    pub env_state_json: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_json: Option<String>,
    pub run_bucket: String,
    pub call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_export_id: Option<i64>,
}

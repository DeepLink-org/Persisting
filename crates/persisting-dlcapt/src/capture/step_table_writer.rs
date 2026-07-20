use crate::capture::step_record::StepRecord;
use crate::config::LanceStorageConfig;
use anyhow::{Result, bail};
use async_trait::async_trait;
use std::sync::Arc;

pub use crate::capture::writers::lance_crate::LanceCrateWriter;

/// 单表 session_steps 追加写；一步一行。
#[async_trait]
pub trait StepTableWriter: Send + Sync {
    async fn append(&self, record: &StepRecord) -> Result<()>;
}

pub fn build_step_table_writer(cfg: &LanceStorageConfig) -> Result<Arc<dyn StepTableWriter>> {
    match cfg.backend.as_str() {
        "lance" => Ok(Arc::new(LanceCrateWriter::new(cfg)?)),
        other => bail!("unsupported lance backend: {other}"),
    }
}

/// Lance 行映射（与 tests/lance/session_steps_import.py `step_to_lance_row` 对齐）。
#[derive(Debug, Clone, PartialEq)]
pub struct LanceStepRow {
    pub id: String,
    pub session_id: String,
    pub step_id: i64,
    pub job_id: String,
    pub group_id: String,
    pub env_name: String,
    pub llm_model: String,
    pub messages_json: String,
    pub response_json: String,
    pub step_reward: f64,
    pub reward: f64,
    pub env_state_json: String,
    pub is_terminal: bool,
    pub is_truncated: bool,
    pub is_session_completed: bool,
    pub is_trainable: bool,
    pub created_at: String,
    pub agent_id: String,
    pub root_session: String,
    pub extensions_json: Option<String>,
    pub capture_json: Option<String>,
    pub call_id: String,
    pub source_export_id: Option<i64>,
}

pub fn step_record_to_lance_row(record: &StepRecord) -> LanceStepRow {
    LanceStepRow {
        id: record.id.clone(),
        session_id: record.session_id.clone(),
        step_id: record.step_id,
        job_id: record.job_id.clone(),
        group_id: record.group_id.clone(),
        env_name: record.env_name.clone(),
        llm_model: record.llm_model.clone(),
        messages_json: record.messages_json.clone(),
        response_json: record.response_json.clone(),
        step_reward: record.step_reward,
        reward: record.reward,
        env_state_json: record.env_state_json.clone(),
        is_terminal: record.is_terminal,
        is_truncated: record.is_truncated,
        is_session_completed: record.is_session_completed,
        is_trainable: record.is_trainable,
        created_at: record.created_at.clone(),
        agent_id: record.agent_id.clone(),
        root_session: record.run_bucket.clone(),
        extensions_json: record.extensions_json.clone(),
        capture_json: record.capture_json.clone(),
        call_id: record.call_id.clone(),
        source_export_id: record.source_export_id,
    }
}

pub fn lance_dataset_uri(db_uri: &str, table_name: &str) -> String {
    crate::capture::writers::lance_storage::lance_dataset_uri(db_uri, table_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::step_record::StepRecord;

    fn fixture_record() -> StepRecord {
        StepRecord {
            id: "dlcapt:sess-import-test:1".to_string(),
            session_id: "sess-import-test".to_string(),
            step_id: 1,
            job_id: "dlcapt".to_string(),
            agent_id: "openclaw".to_string(),
            group_id: String::new(),
            env_name: "openclaw".to_string(),
            llm_model: "kimi-k2.5".to_string(),
            step_reward: 0.0,
            reward: 0.0,
            is_terminal: false,
            is_truncated: false,
            is_session_completed: false,
            is_trainable: true,
            created_at: "2026-06-16 09:57:27.641681+00:00".to_string(),
            messages_json: r#"[{"role":"user","content":"ping"}]"#.to_string(),
            response_json: r#"{"role":"assistant","content":"pong"}"#.to_string(),
            env_state_json: "{}".to_string(),
            extensions_json: Some(r#"{"source":"dlcapt-proxy"}"#.to_string()),
            capture_json: Some(
                r#"{"call_id":"call-import-test-1","finish_reason":"stop"}"#.to_string(),
            ),
            run_bucket: "2026-06-16".to_string(),
            call_id: "call-import-test-1".to_string(),
            source_export_id: None,
        }
    }

    #[test]
    fn step_record_to_lance_row_maps_root_session_and_agent_id() {
        let row = step_record_to_lance_row(&fixture_record());
        assert_eq!(row.id, "dlcapt:sess-import-test:1");
        assert_eq!(row.root_session, "2026-06-16");
        assert_eq!(row.agent_id, "openclaw");
        assert_eq!(row.call_id, "call-import-test-1");
        assert_eq!(
            row.extensions_json.as_deref(),
            Some(r#"{"source":"dlcapt-proxy"}"#)
        );
        assert!(row.source_export_id.is_none());
    }
}

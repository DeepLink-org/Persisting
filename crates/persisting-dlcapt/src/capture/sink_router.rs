use crate::capture::event::CaptureEvent;
use crate::capture::field_registry::{FieldRegistry, materialize_session_step};
use crate::capture::session_dir::resolve_session_layout_with_bucket;
use crate::capture::sink::lance::LanceSink;
use crate::capture::step_record::StepRecord;
use crate::config::ProxyConfig;
use crate::tlv::{MdSinkInput, TlvWriter};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionStepsEnvelope {
    format_version: u32,
    session_id: String,
    session_dir: String,
    agent_id: String,
    run_bucket: String,
    source: String,
    authoritative: String,
    #[serde(default)]
    session_metadata: serde_json::Map<String, Value>,
    #[serde(default)]
    session_steps: Vec<Value>,
}

pub struct CaptureSinkRouter {
    store_root: PathBuf,
    config: Arc<ProxyConfig>,
    registry: FieldRegistry,
    tlv: TlvWriter,
    write_lock: Arc<Mutex<()>>,
    lance_sink: Option<LanceSink>,
}

impl CaptureSinkRouter {
    pub fn new(
        config: Arc<ProxyConfig>,
        tlv: TlvWriter,
        write_lock: Arc<Mutex<()>>,
    ) -> Result<Self> {
        let registry = FieldRegistry::from_export(&config.export);
        let lance_sink = if config.storage.lance_enabled() {
            Some(LanceSink::from_config(&config)?)
        } else {
            None
        };
        Ok(Self {
            store_root: PathBuf::from(&config.store_dir),
            config,
            registry,
            tlv,
            write_lock,
            lance_sink,
        })
    }

    pub async fn dispatch(&self, event: CaptureEvent) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let now = Utc::now();
        let json_path = self.session_steps_path(&event.session_id, now).await?;
        let existing_bucket = self.read_envelope_run_bucket(&json_path).await?;
        let layout = resolve_session_layout_with_bucket(
            &event.session_id,
            &self.config.default_session_id,
            now,
            existing_bucket.as_deref(),
        );

        let session_metadata = self.config.export.session_metadata.clone();
        let record = materialize_session_step(
            &event,
            &self.registry,
            &session_metadata,
            &layout.run_bucket,
        );

        if self.should_write_json_authoritative() {
            self.append_json_file(&event.session_id, &layout, &record, "json_file")
                .await?;
        } else if self.should_write_json_cache() {
            self.append_json_file(&event.session_id, &layout, &record, "lance")
                .await?;
        }

        if let Some(lance_sink) = &self.lance_sink {
            lance_sink.append(&record).await?;
        }

        if self.should_write_md() {
            self.append_md(&event).await?;
        }

        Ok(())
    }

    fn should_write_json_authoritative(&self) -> bool {
        self.config.storage.authoritative == "json_file"
    }

    fn should_write_json_cache(&self) -> bool {
        self.config.storage.json_cache_enabled()
    }

    fn should_write_md(&self) -> bool {
        self.config.storage.also.contains(&"md".to_string())
            || self.config.storage.authoritative == "md"
    }

    async fn session_steps_path(
        &self,
        session_id: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<PathBuf> {
        let layout = resolve_session_layout_with_bucket(
            session_id,
            &self.config.default_session_id,
            now,
            None,
        );
        Ok(self
            .store_root
            .join(&layout.session_dir)
            .join("session_steps.json"))
    }

    async fn read_envelope_run_bucket(&self, path: &Path) -> Result<Option<String>> {
        if !path.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let envelope: SessionStepsEnvelope =
            serde_json::from_str(&text).context("parse session_steps.json envelope")?;
        Ok(Some(envelope.run_bucket))
    }

    async fn append_json_file(
        &self,
        session_id: &str,
        layout: &crate::capture::session_dir::SessionLayout,
        record: &StepRecord,
        authoritative: &str,
    ) -> Result<()> {
        let dir = self.store_root.join(&layout.session_dir);
        fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("create {}", dir.display()))?;
        let path = dir.join("session_steps.json");

        let mut envelope = if path.exists() {
            let text = fs::read_to_string(&path).await?;
            serde_json::from_str::<SessionStepsEnvelope>(&text)
                .unwrap_or_else(|_| self.fresh_envelope(session_id, layout, authoritative))
        } else {
            self.fresh_envelope(session_id, layout, authoritative)
        };

        envelope
            .session_steps
            .push(step_record_to_json_element(record)?);
        let content = serde_json::to_string_pretty(&envelope).context("serialize envelope")?;
        fs::write(&path, content)
            .await
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    fn fresh_envelope(
        &self,
        session_id: &str,
        layout: &crate::capture::session_dir::SessionLayout,
        authoritative: &str,
    ) -> SessionStepsEnvelope {
        SessionStepsEnvelope {
            format_version: 1,
            session_id: session_id.to_string(),
            session_dir: layout.session_dir.clone(),
            agent_id: self.config.agent_id.clone(),
            run_bucket: layout.run_bucket.clone(),
            source: "dlcapt-proxy".to_string(),
            authoritative: authoritative.to_string(),
            session_metadata: self.config.export.session_metadata.clone(),
            session_steps: Vec::new(),
        }
    }

    async fn append_md(&self, event: &CaptureEvent) -> Result<()> {
        let input = MdSinkInput::from_capture_event(event);
        if let Some(record) = input.to_tlv_record() {
            self.tlv
                .append_turn_internal(record)
                .await
                .context("md sink tlv append")?;
        }
        Ok(())
    }
}

fn step_record_to_json_element(record: &StepRecord) -> Result<Value> {
    let extensions = record
        .extensions_json
        .as_ref()
        .map(|s| serde_json::from_str::<Value>(s))
        .transpose()?
        .unwrap_or(json!({}));
    let capture = record
        .capture_json
        .as_ref()
        .map(|s| serde_json::from_str::<Value>(s))
        .transpose()?
        .unwrap_or(json!({}));
    let messages: Value = serde_json::from_str(&record.messages_json).unwrap_or(json!([]));
    let response: Value = serde_json::from_str(&record.response_json).unwrap_or(json!({}));
    let env_state: Value = serde_json::from_str(&record.env_state_json).unwrap_or(json!({}));

    Ok(json!({
        "id": record.id,
        "source_export_id": record.source_export_id,
        "session_id": record.session_id,
        "step_id": record.step_id,
        "job_id": record.job_id,
        "group_id": record.group_id,
        "env_name": record.env_name,
        "llm_model": record.llm_model,
        "messages": messages,
        "response": response,
        "step_reward": record.step_reward,
        "reward": record.reward,
        "env_state": env_state,
        "is_terminal": record.is_terminal,
        "is_truncated": record.is_truncated,
        "is_session_completed": record.is_session_completed,
        "is_trainable": record.is_trainable,
        "created_at": record.created_at,
        "extensions": extensions,
        "_capture": capture,
    }))
}

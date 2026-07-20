use crate::capture::step_record::StepRecord;
use crate::capture::step_table_writer::{StepTableWriter, build_step_table_writer};
use crate::config::{LanceStorageConfig, ProxyConfig};
use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::json;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

pub struct LanceSink {
    writer: Arc<dyn StepTableWriter>,
    fail_open: bool,
    dead_letter_path: PathBuf,
}

impl LanceSink {
    pub fn from_config(config: &ProxyConfig) -> Result<Self> {
        let lance_cfg = &config.storage.lance;
        let writer = build_step_table_writer(lance_cfg)?;
        let dead_letter_path = if lance_cfg.dead_letter_path.is_empty() {
            PathBuf::from(&config.store_dir).join(".capture/lance_dead_letter.jsonl")
        } else if PathBuf::from(&lance_cfg.dead_letter_path).is_absolute() {
            PathBuf::from(&lance_cfg.dead_letter_path)
        } else {
            PathBuf::from(&config.store_dir).join(&lance_cfg.dead_letter_path)
        };

        Ok(Self {
            writer,
            fail_open: lance_cfg.fail_open,
            dead_letter_path,
        })
    }

    pub fn from_parts(
        lance_cfg: &LanceStorageConfig,
        store_dir: &str,
        writer: Arc<dyn StepTableWriter>,
    ) -> Result<Self> {
        let dead_letter_path = if lance_cfg.dead_letter_path.is_empty() {
            PathBuf::from(store_dir).join(".capture/lance_dead_letter.jsonl")
        } else if PathBuf::from(&lance_cfg.dead_letter_path).is_absolute() {
            PathBuf::from(&lance_cfg.dead_letter_path)
        } else {
            PathBuf::from(store_dir).join(&lance_cfg.dead_letter_path)
        };

        Ok(Self {
            writer,
            fail_open: lance_cfg.fail_open,
            dead_letter_path,
        })
    }

    pub async fn append(&self, record: &StepRecord) -> Result<()> {
        match self.writer.append(record).await {
            Ok(()) => Ok(()),
            Err(err) if self.fail_open => {
                self.write_dead_letter(record, &err)?;
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn write_dead_letter(&self, record: &StepRecord, error: &anyhow::Error) -> Result<()> {
        if let Some(parent) = self.dead_letter_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dead letter dir {}", parent.display()))?;
        }

        let line = json!({
            "ts": Utc::now().to_rfc3339(),
            "error": error.to_string(),
            "backend": "lance",
            "record": record,
        });
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.dead_letter_path)
            .with_context(|| format!("open dead letter {}", self.dead_letter_path.display()))?;
        serde_json::to_writer(&mut file, &line).context("serialize dead letter entry")?;
        file.write_all(b"\n").context("write dead letter newline")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::step_record::StepRecord;
    use crate::capture::step_table_writer::StepTableWriter;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FailingWriter;

    #[async_trait]
    impl StepTableWriter for FailingWriter {
        async fn append(&self, _record: &StepRecord) -> Result<()> {
            anyhow::bail!("simulated lance failure")
        }
    }

    struct CountingWriter {
        count: AtomicUsize,
    }

    #[async_trait]
    impl StepTableWriter for CountingWriter {
        async fn append(&self, _record: &StepRecord) -> Result<()> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn test_record() -> StepRecord {
        StepRecord {
            id: "dlcapt:sess:1".to_string(),
            session_id: "sess".to_string(),
            step_id: 1,
            job_id: "dlcapt".to_string(),
            agent_id: "openclaw".to_string(),
            group_id: String::new(),
            env_name: "openclaw".to_string(),
            llm_model: "m".to_string(),
            step_reward: 0.0,
            reward: 0.0,
            is_terminal: false,
            is_truncated: false,
            is_session_completed: false,
            is_trainable: true,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            messages_json: "[]".to_string(),
            response_json: "{}".to_string(),
            env_state_json: "{}".to_string(),
            extensions_json: None,
            capture_json: None,
            run_bucket: "2026-06-16".to_string(),
            call_id: "call-1".to_string(),
            source_export_id: None,
        }
    }

    #[tokio::test]
    async fn fail_open_writes_dead_letter_and_returns_ok() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cfg = LanceStorageConfig {
            fail_open: true,
            dead_letter_path: ".capture/lance_dead_letter.jsonl".to_string(),
            ..LanceStorageConfig::default()
        };
        let sink = LanceSink::from_parts(
            &cfg,
            temp.path().to_string_lossy().as_ref(),
            Arc::new(FailingWriter),
        )
        .expect("sink");

        sink.append(&test_record()).await.expect("fail_open append");

        let dead_letter = temp.path().join(".capture/lance_dead_letter.jsonl");
        assert!(dead_letter.exists());
        let text = std::fs::read_to_string(dead_letter).expect("read dead letter");
        assert!(text.contains("simulated lance failure"));
        assert!(text.contains("dlcapt:sess:1"));
    }

    #[tokio::test]
    async fn fail_closed_propagates_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cfg = LanceStorageConfig {
            fail_open: false,
            ..LanceStorageConfig::default()
        };
        let sink = LanceSink::from_parts(
            &cfg,
            temp.path().to_string_lossy().as_ref(),
            Arc::new(FailingWriter),
        )
        .expect("sink");

        let err = sink.append(&test_record()).await.unwrap_err();
        assert!(err.to_string().contains("simulated lance failure"));
    }

    #[tokio::test]
    async fn successful_append_delegates_to_writer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let counter = Arc::new(CountingWriter {
            count: AtomicUsize::new(0),
        });
        let cfg = LanceStorageConfig::default();
        let sink = LanceSink::from_parts(
            &cfg,
            temp.path().to_string_lossy().as_ref(),
            Arc::clone(&counter) as Arc<dyn StepTableWriter>,
        )
        .expect("sink");

        sink.append(&test_record()).await.expect("append");
        assert_eq!(counter.count.load(Ordering::SeqCst), 1);
    }
}

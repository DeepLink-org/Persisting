use crate::capture::step_record::StepRecord;
use crate::capture::step_table_writer::{LanceStepRow, StepTableWriter, step_record_to_lance_row};
use crate::capture::writers::lance_storage::{
    build_object_store_params, lance_dataset_uri, open_dataset, write_params_with_store,
};
use crate::config::LanceStorageConfig;
use anyhow::{Context, Result};
use arrow::array::{BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatchIterator;
use async_trait::async_trait;
use lance::Dataset;
use lance::dataset::{WriteMode, WriteParams};
use lance::io::ObjectStoreParams;
use std::sync::Arc;

pub struct LanceCrateWriter {
    dataset_uri: String,
    schema: Arc<Schema>,
    store_params: Option<ObjectStoreParams>,
}

impl LanceCrateWriter {
    pub fn new(cfg: &LanceStorageConfig) -> Result<Self> {
        Ok(Self {
            dataset_uri: lance_dataset_uri(&cfg.db_uri, &cfg.table_name),
            schema: Arc::new(session_steps_schema()),
            store_params: build_object_store_params(cfg),
        })
    }

    fn write_params(&self, mode: WriteMode) -> WriteParams {
        WriteParams {
            mode,
            ..write_params_with_store(self.store_params.clone())
        }
    }

    async fn append_async(&self, row: &LanceStepRow) -> Result<()> {
        if !self.dataset_uri.starts_with("s3://")
            && let Some(parent) = std::path::Path::new(&self.dataset_uri).parent()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create lance db dir {}", parent.display()))?;
        }

        let batch = lance_row_to_batch(row, self.schema.clone())?;
        let batches = RecordBatchIterator::new(std::iter::once(Ok(batch)), self.schema.clone());

        if let Some(mut dataset) = open_dataset(&self.dataset_uri, &self.store_params)
            .await
            .context("probe lance dataset")?
        {
            dataset
                .append(batches, Some(self.write_params(WriteMode::Append)))
                .await
                .context("append to lance dataset")?;
        } else {
            Dataset::write(
                batches,
                self.dataset_uri.as_str(),
                Some(self.write_params(WriteMode::Create)),
            )
            .await
            .context("create lance dataset")?;
        }

        Ok(())
    }
}

#[async_trait]
impl StepTableWriter for LanceCrateWriter {
    async fn append(&self, record: &StepRecord) -> Result<()> {
        let row = step_record_to_lance_row(record);
        self.append_async(&row).await
    }
}

pub fn session_steps_schema() -> Schema {
    Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("session_id", DataType::Utf8, false),
        Field::new("step_id", DataType::Int64, false),
        Field::new("job_id", DataType::Utf8, false),
        Field::new("group_id", DataType::Utf8, false),
        Field::new("env_name", DataType::Utf8, false),
        Field::new("llm_model", DataType::Utf8, false),
        Field::new("messages_json", DataType::Utf8, false),
        Field::new("response_json", DataType::Utf8, false),
        Field::new("step_reward", DataType::Float64, false),
        Field::new("reward", DataType::Float64, false),
        Field::new("env_state_json", DataType::Utf8, false),
        Field::new("is_terminal", DataType::Boolean, false),
        Field::new("is_truncated", DataType::Boolean, false),
        Field::new("is_session_completed", DataType::Boolean, false),
        Field::new("is_trainable", DataType::Boolean, false),
        Field::new("created_at", DataType::Utf8, false),
        Field::new("agent_id", DataType::Utf8, false),
        Field::new("root_session", DataType::Utf8, false),
        Field::new("extensions_json", DataType::Utf8, true),
        Field::new("capture_json", DataType::Utf8, true),
        Field::new("call_id", DataType::Utf8, false),
        Field::new("source_export_id", DataType::Int64, true),
    ])
}

fn lance_row_to_batch(row: &LanceStepRow, schema: Arc<Schema>) -> Result<RecordBatch> {
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec![row.id.as_str()])),
            Arc::new(StringArray::from(vec![row.session_id.as_str()])),
            Arc::new(Int64Array::from(vec![row.step_id])),
            Arc::new(StringArray::from(vec![row.job_id.as_str()])),
            Arc::new(StringArray::from(vec![row.group_id.as_str()])),
            Arc::new(StringArray::from(vec![row.env_name.as_str()])),
            Arc::new(StringArray::from(vec![row.llm_model.as_str()])),
            Arc::new(StringArray::from(vec![row.messages_json.as_str()])),
            Arc::new(StringArray::from(vec![row.response_json.as_str()])),
            Arc::new(Float64Array::from(vec![row.step_reward])),
            Arc::new(Float64Array::from(vec![row.reward])),
            Arc::new(StringArray::from(vec![row.env_state_json.as_str()])),
            Arc::new(BooleanArray::from(vec![row.is_terminal])),
            Arc::new(BooleanArray::from(vec![row.is_truncated])),
            Arc::new(BooleanArray::from(vec![row.is_session_completed])),
            Arc::new(BooleanArray::from(vec![row.is_trainable])),
            Arc::new(StringArray::from(vec![row.created_at.as_str()])),
            Arc::new(StringArray::from(vec![row.agent_id.as_str()])),
            Arc::new(StringArray::from(vec![row.root_session.as_str()])),
            Arc::new(nullable_string_array(&row.extensions_json)),
            Arc::new(nullable_string_array(&row.capture_json)),
            Arc::new(StringArray::from(vec![row.call_id.as_str()])),
            Arc::new(nullable_i64_array(row.source_export_id)),
        ],
    )
    .context("build lance record batch")?;
    Ok(batch)
}

fn nullable_string_array(value: &Option<String>) -> StringArray {
    match value {
        Some(text) => StringArray::from(vec![Some(text.as_str())]),
        None => StringArray::from(vec![None as Option<&str>]),
    }
}

fn nullable_i64_array(value: Option<i64>) -> Int64Array {
    Int64Array::from(vec![value])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::step_record::StepRecord;
    use crate::capture::writers::lance_storage::local_dataset_path;
    use tempfile::tempdir;

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

    #[tokio::test]
    async fn append_creates_and_appends_rows() {
        let dir = tempdir().expect("tempdir");
        let cfg = LanceStorageConfig {
            db_uri: dir.path().to_string_lossy().to_string(),
            table_name: "session_steps".to_string(),
            ..LanceStorageConfig::default()
        };
        let writer = LanceCrateWriter::new(&cfg).expect("writer");
        let record = fixture_record();
        writer.append(&record).await.expect("first append");
        writer
            .append(&StepRecord {
                step_id: 2,
                id: "dlcapt:sess-import-test:2".to_string(),
                ..record.clone()
            })
            .await
            .expect("second append");

        let dataset_uri = local_dataset_path(&cfg.db_uri, &cfg.table_name);
        assert!(dataset_uri.exists());

        let dataset = Dataset::open(dataset_uri.to_string_lossy().as_ref())
            .await
            .expect("open");
        let count = dataset.count_rows(None).await.expect("count");
        assert_eq!(count, 2);
    }

    #[test]
    fn s3_writer_builds_object_store_params() {
        let cfg = LanceStorageConfig {
            db_uri: "s3://my-bucket/capture-prod".to_string(),
            s3: Some(crate::config::LanceS3Config {
                region: "cn-north-1".to_string(),
                endpoint: None,
                allow_http: Some(true),
            }),
            ..LanceStorageConfig::default()
        };
        let writer = LanceCrateWriter::new(&cfg).expect("writer");
        assert_eq!(
            writer.dataset_uri,
            "s3://my-bucket/capture-prod/session_steps.lance"
        );
        assert!(writer.store_params.is_some());
    }
}

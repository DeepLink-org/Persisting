use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;
use std::path::PathBuf;
use tokio::fs;

#[derive(Debug, Clone)]
pub struct AuditWriter {
    store_root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct AuditRecord {
    pub session_id: String,
    pub model: String,
    pub stream: bool,
    pub status_code: u16,
    pub request_body: Value,
    pub response_summary: String,
    pub response_text: Option<String>,
    pub response_raw: Option<String>,
    pub finish_reason: Option<String>,
    pub usage: Option<Value>,
}

impl AuditWriter {
    pub fn new(store_root: PathBuf) -> Self {
        Self { store_root }
    }

    pub async fn write(&self, record: AuditRecord) -> Result<PathBuf> {
        let run_id = Utc::now().format("%Y%m%d-%H%M%S-%3f").to_string();
        let run_dir = self
            .store_root
            .join(sanitize_path_segment(&record.session_id))
            .join(format!("run-{run_id}"));
        fs::create_dir_all(&run_dir)
            .await
            .with_context(|| format!("failed creating audit dir: {}", run_dir.display()))?;

        let run_file = run_dir.join(format!("run-{run_id}.md"));
        let body_pretty = serde_json::to_string_pretty(&record.request_body)
            .with_context(|| "failed rendering request body".to_string())?;

        let usage_text = record
            .usage
            .as_ref()
            .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()))
            .unwrap_or_else(|| "null".to_string());

        let finish_reason = record
            .finish_reason
            .unwrap_or_else(|| "unknown".to_string());
        let response_text = record
            .response_text
            .as_deref()
            .unwrap_or("(empty response text)");
        let response_raw = record
            .response_raw
            .as_deref()
            .unwrap_or("(empty response raw)");
        let content = format!(
            "# run-{run_id}\n\n\
session_id: {session}\n\
model: {model}\n\
stream: {stream}\n\
status_code: {status_code}\n\
finish_reason: {finish_reason}\n\
usage: {usage_text}\n\
response_summary: {response_summary}\n\n\
## request\n\
```json\n\
{body_pretty}\n\
```\n\n\
## response_text\n\
{response_text}\n\n\
## response_raw\n\
```text\n\
{response_raw}\n\
```\n",
            session = record.session_id,
            model = record.model,
            stream = record.stream,
            status_code = record.status_code,
            response_summary = record.response_summary.replace('\n', " "),
        );

        fs::write(&run_file, content)
            .await
            .with_context(|| format!("failed writing audit file: {}", run_file.display()))?;
        Ok(run_file)
    }
}

fn sanitize_path_segment(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

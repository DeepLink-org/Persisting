use crate::capture::CaptureEvent;
use crate::capture::session_dir::resolve_session_layout;
use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::sync::Mutex;

const BLOCK_MARKER: &str = "<!-- persisting:block";
const BLOCK_LAYOUT: &str = "<!-- persisting:block:{speaker} {json} -->\n\nmessage body\n\n";
const BLOCK_FORMAT_VERSION: u64 = 1;

#[derive(Debug, Clone)]
pub struct TlvWriter {
    store_root: PathBuf,
    agent_id: String,
    default_session_id: String,
    write_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Clone)]
pub struct TlvTurnRecord {
    pub session_id: String,
    pub agent_id: String,
    pub model: String,
    pub stream: bool,
    pub status_code: u16,
    pub user_text: Option<String>,
    pub assistant_text: Option<String>,
    pub usage: Option<Value>,
    pub user_seq: u64,
    pub assistant_seq: u64,
    pub turn: u64,
    pub call_id: String,
    pub request_path: String,
}

#[derive(Debug, Clone)]
pub struct MdSinkInput {
    pub session_id: String,
    pub agent_id: String,
    pub model: String,
    pub stream: bool,
    pub status_code: u16,
    pub user_text: Option<String>,
    pub assistant_text: Option<String>,
    pub usage: Option<Value>,
    pub user_seq: u64,
    pub assistant_seq: u64,
    pub turn: u64,
    pub call_id: String,
    pub request_path: String,
}

impl MdSinkInput {
    pub fn from_capture_event(event: &CaptureEvent) -> Self {
        Self {
            session_id: event.session_id.clone(),
            agent_id: event.agent_id.clone(),
            model: event.model.clone(),
            stream: event.stream,
            status_code: event.status_code,
            user_text: crate::dialogue::extract_user_text(event.endpoint, &event.request),
            assistant_text: event.response_text.clone(),
            usage: event.capture_meta.usage.clone(),
            user_seq: event.user_seq,
            assistant_seq: event.assistant_seq,
            turn: event.turn,
            call_id: event.call_id.clone(),
            request_path: event.request_path.clone(),
        }
    }

    pub fn to_tlv_record(&self) -> Option<TlvTurnRecord> {
        let has_user = self
            .user_text
            .as_deref()
            .is_some_and(|text| !text.is_empty());
        let has_assistant = self
            .assistant_text
            .as_deref()
            .is_some_and(|text| !text.is_empty());
        if !has_user && !has_assistant {
            return None;
        }
        Some(TlvTurnRecord {
            session_id: self.session_id.clone(),
            agent_id: self.agent_id.clone(),
            model: self.model.clone(),
            stream: self.stream,
            status_code: self.status_code,
            user_text: self.user_text.clone(),
            assistant_text: self.assistant_text.clone(),
            usage: self.usage.clone(),
            user_seq: self.user_seq,
            assistant_seq: self.assistant_seq,
            turn: self.turn,
            call_id: self.call_id.clone(),
            request_path: self.request_path.clone(),
        })
    }
}

impl TlvWriter {
    pub fn new(
        store_root: PathBuf,
        agent_id: String,
        default_session_id: String,
        write_lock: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            store_root,
            agent_id,
            default_session_id,
            write_lock,
        }
    }

    pub fn write_lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.write_lock)
    }

    pub async fn append_turn(&self, record: TlvTurnRecord) -> Result<PathBuf> {
        let _guard = self.write_lock.lock().await;
        self.append_turn_internal(record).await
    }

    pub async fn append_turn_internal(&self, record: TlvTurnRecord) -> Result<PathBuf> {
        let now = Utc::now();
        let layout = resolve_session_layout(&record.session_id, &self.default_session_id, now);
        let md_path = self
            .store_root
            .join(&layout.session_dir)
            .join("trajectory.md");

        if let Some(parent) = md_path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed creating tlv dir: {}", parent.display()))?;
        }

        let mut blocks = Vec::new();
        if let Some(user_text) = record.user_text.as_deref().filter(|text| !text.is_empty()) {
            blocks.push(encode_block(
                "user",
                user_text,
                &record,
                record.user_seq,
                "llm.request",
            )?);
        }
        if let Some(assistant_text) = record
            .assistant_text
            .as_deref()
            .filter(|text| !text.is_empty())
        {
            let kind = if record.stream {
                "llm.response.stream"
            } else {
                "llm.response"
            };
            blocks.push(encode_block(
                "assistant",
                assistant_text,
                &record,
                record.assistant_seq,
                kind,
            )?);
        }

        if blocks.is_empty() {
            anyhow::bail!("tlv turn has no visible user or assistant content");
        }

        let turns = record.turn;
        if !md_path.exists() {
            let preamble = format_document_preamble(&record.session_id, &self.agent_id, turns)?;
            let content = format!("{preamble}{}", blocks.join(""));
            fs::write(&md_path, content)
                .await
                .with_context(|| format!("failed writing tlv file: {}", md_path.display()))?;
        } else {
            let existing = fs::read_to_string(&md_path)
                .await
                .with_context(|| format!("failed reading tlv file: {}", md_path.display()))?;
            let updated = refresh_frontmatter_turns(&existing, turns)?;
            let content = format!("{updated}{}", blocks.join(""));
            fs::write(&md_path, content)
                .await
                .with_context(|| format!("failed appending tlv file: {}", md_path.display()))?;
        }

        Ok(md_path)
    }
}

pub fn new_call_id() -> String {
    let now = Utc::now();
    format!(
        "call-{}-{}",
        now.format("%Y%m%d%H%M%S"),
        now.timestamp_subsec_micros()
    )
}

fn encode_block(
    speaker: &str,
    body: &str,
    record: &TlvTurnRecord,
    seq: u64,
    kind: &str,
) -> Result<String> {
    let timestamp = Utc::now().to_rfc3339();
    let mut fields = BTreeMap::new();
    fields.insert("agent_id".to_string(), json!(record.agent_id));
    fields.insert("call_id".to_string(), json!(record.call_id));
    fields.insert("kind".to_string(), json!(kind));
    fields.insert("model".to_string(), json!(record.model));
    fields.insert("path".to_string(), json!(record.request_path));
    fields.insert("role".to_string(), json!(speaker));
    fields.insert("seq".to_string(), json!(seq));
    fields.insert("session_id".to_string(), json!(record.session_id));
    fields.insert("source".to_string(), json!("dlcapt-proxy"));
    fields.insert("timestamp".to_string(), json!(timestamp));
    fields.insert("trace_id".to_string(), json!(record.call_id));
    fields.insert("turn".to_string(), json!(record.turn));
    fields.insert("v".to_string(), json!(BLOCK_FORMAT_VERSION));

    if speaker == "assistant" {
        fields.insert("status".to_string(), json!(record.status_code));
        if let Some(usage) = record.usage.as_ref() {
            if let Some(prompt_tokens) = usage.get("prompt_tokens") {
                fields.insert("prompt_tokens".to_string(), prompt_tokens.clone());
                fields.insert("input_tokens".to_string(), prompt_tokens.clone());
            }
            if let Some(completion_tokens) = usage.get("completion_tokens") {
                fields.insert("completion_tokens".to_string(), completion_tokens.clone());
                fields.insert("output_tokens".to_string(), completion_tokens.clone());
            }
            if let Some(total_tokens) = usage.get("total_tokens") {
                fields.insert("total_tokens".to_string(), total_tokens.clone());
            }
        }
    }

    let header = json!({
        "type": "markdown",
        "length": body.len(),
        "fields": fields,
    });
    let flat = flatten_block_header(&header)?;
    let json_text = serde_json::to_string(&flat).context("serialize tlv block header")?;
    Ok(format!(
        "{BLOCK_MARKER}:{speaker} {json_text} -->\n\n{body}\n\n"
    ))
}

fn flatten_block_header(header: &Value) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    if let Some(type_name) = header.get("type") {
        out.insert("type".to_string(), type_name.clone());
    }
    if let Some(length) = header.get("length") {
        out.insert("length".to_string(), length.clone());
    }
    if let Some(fields) = header.get("fields").and_then(Value::as_object).cloned() {
        for (key, value) in fields {
            out.insert(key, value);
        }
    }
    Ok(out)
}

fn format_document_preamble(session_id: &str, agent_id: &str, turns: u64) -> Result<String> {
    Ok(format!(
        "---\n\
format: persisting:1.0\n\
block: |+\n\
  {BLOCK_LAYOUT}\n\
session: {session_id}\n\
agent: {agent_id}\n\
turns: {turns}\n\
client:\n\
  peer: ''\n\
  peer_port: 0\n\
  pid: 0\n\
  command: openclaw\n\
  machine_fp: ''\n\
---\n\n"
    ))
}

fn refresh_frontmatter_turns(content: &str, turns: u64) -> Result<String> {
    let Some(end) = content.find("\n---\n\n") else {
        anyhow::bail!("tlv file missing frontmatter terminator");
    };
    let preamble = &content[..end];
    let body = &content[end + "\n---\n\n".len()..];
    let updated_preamble = if preamble.contains("turns:") {
        preamble
            .lines()
            .map(|line| {
                if line.starts_with("turns:") {
                    format!("turns: {turns}")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        format!("{preamble}\nturns: {turns}")
    };
    Ok(format!("{updated_preamble}\n---\n\n{body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_frontmatter_turns_should_update_turn_count() {
        let input = "---\nformat: persisting:1.0\nsession: abc\nturns: 1\n---\n\nbody\n";
        let updated = refresh_frontmatter_turns(input, 3).expect("refresh turns");
        assert!(updated.contains("turns: 3"));
        assert!(updated.ends_with("body\n"));
    }
}

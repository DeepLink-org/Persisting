//! Versioned, fail-closed Claude Code resume-transport cleanup.
//!
//! Claude Code needs a local wake-up input when `--resume --print` starts from
//! a native session ending in `tool_result`. Claude Code 2.1.220 wraps that
//! input in a deterministic three-message envelope. This module validates the
//! complete envelope and the reconstructed native prefix before removing the
//! transport-only messages. It deliberately performs no network I/O and writes
//! no audit or trajectory files.

use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

pub const TRANSPORT_SCHEMA_VERSION: &str = "sandbox-playback.claude-resume-transport/v1";
pub const PROFILE_ID: &str = "claude-code/2.1.220/native-resume-v1";
pub const CLAUDE_CODE_VERSION: &str = "2.1.220";
pub const CONTINUE_TEXT: &str = "Continue from where you left off.";
pub const NO_RESPONSE_TEXT: &str = "No response requested.";

const SKILLS_SYSTEM_PREFIX: &str =
    "The following skills are available for use with the Skill tool:\n\n- ";
const AGENT_TYPES_SYSTEM_PREFIX: &str = "Available agent types for the Agent tool:\n";
const TASK_TOOL_REMINDER: &str = "The task tools haven't been used recently. If you're working on tasks that would benefit from tracking progress, consider using TaskCreate to add new tasks and TaskUpdate to update task status (set to in_progress when starting, completed when done). Also consider cleaning up the task list if it has become stale. Only use these if relevant to the current work. This is just a gentle reminder - ignore if not applicable.";
const TASKS_SUFFIX_PREFIX: &str = "\n\n\nHere are the existing tasks:\n\n";
const CURRENT_DATE_PREFIX: &str = "<system-reminder>\nAs you answer the user's questions, you can use the following context:\n# currentDate\nToday's date is ";
const CURRENT_DATE_SUFFIX: &str = ".\n\n      IMPORTANT: this context may or may not be relevant to your tasks. You should not respond to this context unless it is highly relevant to your task.\n</system-reminder>";

/// Run-scoped integrity contract shared by session reconstruction and the
/// local Claude bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeTransportManifest {
    pub schema_version: String,
    pub profile_id: String,
    pub claude_code_version: String,
    pub nonce: String,
    pub session_id: String,
    pub boundary_tool_use_ids: Vec<String>,
    pub boundary_observation_sha256: Vec<String>,
    pub canonical_message_count: usize,
    pub canonical_prefix_sha256: String,
    #[serde(default)]
    pub canonical_message_sha256: Vec<String>,
}

impl ResumeTransportManifest {
    pub fn create(
        session_id: &str,
        boundary_tool_use_ids: Vec<String>,
        canonical_messages: Vec<Value>,
        nonce: String,
    ) -> Result<Self> {
        ensure!(
            canonical_messages.len() >= 2,
            "canonical messages must end in assistant tool_use and user tool_result"
        );
        ensure!(
            canonical_messages.iter().all(Value::is_object),
            "canonical messages must be JSON objects"
        );
        let assistant_ids = tool_use_ids(
            &canonical_messages[canonical_messages.len() - 2],
            "canonical boundary assistant",
        )?;
        let boundary_message = canonical_messages.last().expect("length checked");
        ensure!(
            boundary_message.get("role").and_then(Value::as_str) == Some("user"),
            "canonical boundary result must have role=user"
        );
        let content = boundary_message
            .get("content")
            .and_then(Value::as_array)
            .context("canonical boundary result must contain only tool_result blocks")?;
        ensure!(
            !content.is_empty()
                && content.iter().all(|block| {
                    block.is_object()
                        && block.get("type").and_then(Value::as_str) == Some("tool_result")
                }),
            "canonical boundary result must contain only tool_result blocks"
        );
        let result_ids = content
            .iter()
            .map(|block| required_string(block.get("tool_use_id"), "boundary tool_use_id"))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            assistant_ids == boundary_tool_use_ids && result_ids == boundary_tool_use_ids,
            "canonical boundary tool IDs do not match boundary_tool_use_ids"
        );

        let manifest = Self {
            schema_version: TRANSPORT_SCHEMA_VERSION.to_owned(),
            profile_id: PROFILE_ID.to_owned(),
            claude_code_version: CLAUDE_CODE_VERSION.to_owned(),
            nonce,
            session_id: session_id.to_owned(),
            boundary_tool_use_ids,
            boundary_observation_sha256: content
                .iter()
                .map(canonical_observation_sha256)
                .collect::<Result<Vec<_>>>()?,
            canonical_message_count: canonical_messages.len(),
            canonical_prefix_sha256: canonical_messages_sha256(&canonical_messages)?,
            canonical_message_sha256: canonical_messages
                .iter()
                .map(|message| canonical_messages_sha256(std::slice::from_ref(message)))
                .collect::<Result<Vec<_>>>()?,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == TRANSPORT_SCHEMA_VERSION,
            "unsupported transport schema {:?}",
            self.schema_version
        );
        ensure!(
            self.profile_id == PROFILE_ID,
            "unknown Claude resume transport profile {:?}",
            self.profile_id
        );
        ensure!(
            self.claude_code_version == CLAUDE_CODE_VERSION,
            "profile {:?} requires Claude Code {}, got {}",
            self.profile_id,
            CLAUDE_CODE_VERSION,
            self.claude_code_version
        );
        ensure!(!self.session_id.is_empty(), "session_id must not be empty");
        ensure!(
            self.nonce.chars().count() >= 16,
            "nonce must contain at least 16 characters"
        );
        ensure!(
            !self.boundary_tool_use_ids.is_empty(),
            "boundary_tool_use_ids must not be empty"
        );
        ensure!(
            self.boundary_tool_use_ids
                .iter()
                .all(|call_id| !call_id.is_empty()),
            "boundary_tool_use_ids must contain non-empty strings"
        );
        ensure!(
            all_unique(&self.boundary_tool_use_ids),
            "boundary_tool_use_ids must be unique"
        );
        ensure!(
            self.boundary_observation_sha256.len() == self.boundary_tool_use_ids.len()
                && self
                    .boundary_observation_sha256
                    .iter()
                    .all(|digest| valid_sha256(digest)),
            "boundary_observation_sha256 must align with boundary_tool_use_ids"
        );
        ensure!(
            self.canonical_message_count >= 2,
            "canonical_message_count must be at least two"
        );
        ensure!(
            valid_sha256(&self.canonical_prefix_sha256),
            "canonical_prefix_sha256 must be a lowercase SHA-256 hex digest"
        );
        ensure!(
            self.canonical_message_sha256.is_empty()
                || (self.canonical_message_sha256.len() == self.canonical_message_count
                    && self
                        .canonical_message_sha256
                        .iter()
                        .all(|digest| valid_sha256(digest))),
            "canonical_message_sha256 must align with canonical_message_count"
        );
        Ok(())
    }
}

/// Non-secret validation facts returned to the in-memory bridge state machine.
/// The caller may use these for fail-closed checks; this module never persists
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeTransportValidation {
    pub request_sequence: usize,
    pub profile_id: String,
    pub nonce_message_index: usize,
    pub sentinel_removed: usize,
    pub closure_messages_removed: usize,
    pub boundary_tool_use_ids: Vec<String>,
    pub boundary_observation_sha256: Vec<String>,
    pub clean_message_count: usize,
    pub clean_prefix_sha256: String,
    pub first_request_boundary_ok: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CleanedResumeRequest {
    pub payload: Value,
    pub validation: ResumeTransportValidation,
}

pub fn canonical_messages_sha256(messages: &[Value]) -> Result<String> {
    ensure!(
        messages.iter().all(Value::is_object),
        "messages must be a list of JSON objects"
    );
    Ok(sha256_hex(
        canonical_json(&Value::Array(messages.to_vec())).as_bytes(),
    ))
}

pub fn canonical_observation_sha256(tool_result_block: &Value) -> Result<String> {
    ensure!(
        tool_result_block.get("type").and_then(Value::as_str) == Some("tool_result"),
        "observation hash input must be a tool_result block"
    );
    let value = json!({
        "content": tool_result_block.get("content").cloned().unwrap_or(Value::Null),
        "is_error": tool_result_block
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    });
    Ok(sha256_hex(canonical_json(&value).as_bytes()))
}

/// Validate and remove exactly one Claude Code resume envelope.
///
/// The first model request discards every message after the nonce because all
/// such messages are resume-time additions. Later requests retain their suffix,
/// which contains the newly generated continuation.
pub fn clean_resume_transport_envelope(
    payload: &Value,
    manifest: &ResumeTransportManifest,
    request_sequence: usize,
) -> Result<CleanedResumeRequest> {
    ensure!(
        request_sequence >= 1,
        "request_sequence must be a positive integer"
    );
    manifest.validate()?;
    let payload_object = payload
        .as_object()
        .context("resume payload must be a JSON object")?;
    let raw_messages = payload_object
        .get("messages")
        .and_then(Value::as_array)
        .context("payload.messages must be a list of JSON objects")?;
    ensure!(
        raw_messages.iter().all(Value::is_object),
        "payload.messages must be a list of JSON objects"
    );
    let messages = raw_messages.clone();

    let mut nonce_indexes = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if message.get("role").and_then(Value::as_str) == Some("user")
            && sole_text(message, &format!("messages[{index}]"))? == Some(manifest.nonce.as_str())
        {
            nonce_indexes.push(index);
        }
    }
    ensure!(
        nonce_indexes.len() == 1,
        "expected exactly one sole-content resume nonce message, found {}",
        nonce_indexes.len()
    );
    let nonce_index = nonce_indexes[0];
    ensure!(
        nonce_index >= 3,
        "resume nonce has no complete preceding envelope"
    );

    let assistant_closure_index = nonce_index - 1;
    let boundary_result_index = nonce_index - 2;
    let boundary_assistant_index = nonce_index - 3;
    let assistant_closure = &messages[assistant_closure_index];
    ensure!(
        assistant_closure.get("role").and_then(Value::as_str) == Some("assistant")
            && sole_text(
                assistant_closure,
                &format!("messages[{assistant_closure_index}]")
            )? == Some(NO_RESPONSE_TEXT),
        "resume nonce is not preceded by the exact assistant closure"
    );

    let (result_ids, clean_result_blocks) =
        boundary_result_and_closure(&messages[boundary_result_index])?;
    let assistant = &messages[boundary_assistant_index];
    ensure!(
        assistant.get("role").and_then(Value::as_str) == Some("assistant"),
        "boundary result envelope is not preceded by an assistant message"
    );
    let assistant_ids = tool_use_ids(assistant, &format!("messages[{boundary_assistant_index}]"))?;
    ensure!(
        assistant_ids == manifest.boundary_tool_use_ids,
        "boundary assistant tool_use IDs do not match manifest"
    );
    ensure!(
        result_ids == manifest.boundary_tool_use_ids,
        "boundary tool_result IDs do not match manifest"
    );
    let observation_hashes = clean_result_blocks
        .iter()
        .map(canonical_observation_sha256)
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        observation_hashes == manifest.boundary_observation_sha256,
        "boundary O' observation hash does not match resume manifest"
    );

    let continuation_messages = if request_sequence == 1 {
        Vec::new()
    } else {
        messages[nonce_index + 1..].to_vec()
    };
    let mut cleaned_boundary = messages[boundary_result_index].clone();
    cleaned_boundary["content"] = Value::Array(clean_result_blocks);
    let mut cleaned_messages = messages[..boundary_result_index].to_vec();
    cleaned_messages.push(cleaned_boundary);
    cleaned_messages.extend(continuation_messages);

    ensure!(
        !contains_exact_transport_text(&cleaned_messages),
        "resume transport text remains after structured cleanup"
    );
    let canonical_messages = canonical_cli_message_projection(&cleaned_messages);
    let count = manifest.canonical_message_count;
    ensure!(
        canonical_messages.len() >= count,
        "clean request has {} canonical messages, fewer than canonical prefix length {}",
        canonical_messages.len(),
        count
    );
    let clean_prefix = &canonical_messages[..count];
    let prefix_digest = canonical_messages_sha256(clean_prefix)?;
    ensure!(
        prefix_digest == manifest.canonical_prefix_sha256,
        "clean request canonical prefix hash does not match resume manifest"
    );
    let first_request_ok = canonical_messages.len() == count;
    if request_sequence == 1 {
        ensure!(
            first_request_ok,
            "first resumed model request contains messages after the canonical boundary"
        );
    }

    let mut cleaned_payload = payload.clone();
    cleaned_payload["messages"] = Value::Array(cleaned_messages);
    ensure!(
        !contains_nonce(&cleaned_payload, &manifest.nonce),
        "resume nonce remains anywhere in the cleaned model payload"
    );

    Ok(CleanedResumeRequest {
        payload: cleaned_payload,
        validation: ResumeTransportValidation {
            request_sequence,
            profile_id: manifest.profile_id.clone(),
            nonce_message_index: nonce_index,
            sentinel_removed: 1,
            closure_messages_removed: 2,
            boundary_tool_use_ids: manifest.boundary_tool_use_ids.clone(),
            boundary_observation_sha256: observation_hashes,
            clean_message_count: canonical_messages.len(),
            clean_prefix_sha256: prefix_digest,
            first_request_boundary_ok: first_request_ok,
        },
    })
}

fn required_string(value: Option<&Value>, context: &str) -> Result<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .with_context(|| format!("{context} must be a non-empty string"))
}

fn sole_text<'a>(message: &'a Value, context: &str) -> Result<Option<&'a str>> {
    let content = message
        .get("content")
        .with_context(|| format!("{context} has no content"))?;
    if let Some(text) = content.as_str() {
        return Ok(Some(text));
    }
    let Some(blocks) = content.as_array() else {
        return Ok(None);
    };
    if blocks.len() == 1
        && blocks[0].get("type").and_then(Value::as_str) == Some("text")
        && blocks[0].get("text").and_then(Value::as_str).is_some()
    {
        return Ok(blocks[0].get("text").and_then(Value::as_str));
    }
    Ok(None)
}

fn tool_use_ids(message: &Value, context: &str) -> Result<Vec<String>> {
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .with_context(|| format!("{context} content must be a block list"))?;
    ensure!(
        content.iter().all(Value::is_object),
        "{context} contains a non-object content block"
    );
    let ids = content
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|block| required_string(block.get("id"), &format!("{context} tool_use ID")))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        all_unique(&ids),
        "{context} contains duplicate tool_use IDs"
    );
    Ok(ids)
}

fn boundary_result_and_closure(message: &Value) -> Result<(Vec<String>, Vec<Value>)> {
    ensure!(
        message.get("role").and_then(Value::as_str) == Some("user"),
        "boundary result envelope message must have role=user"
    );
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .context("boundary result envelope must contain tool_result block(s) and closure text")?;
    ensure!(
        content.len() >= 2,
        "boundary result envelope must contain tool_result block(s) and closure text"
    );
    ensure!(
        content.iter().all(Value::is_object),
        "boundary result envelope contains a non-object block"
    );
    let closure = content.last().expect("length checked");
    ensure!(
        closure.get("type").and_then(Value::as_str) == Some("text")
            && closure.get("text").and_then(Value::as_str) == Some(CONTINUE_TEXT),
        "boundary result envelope does not end with the exact continue closure"
    );
    let result_blocks = content[..content.len() - 1].to_vec();
    ensure!(
        result_blocks
            .iter()
            .all(|block| { block.get("type").and_then(Value::as_str) == Some("tool_result") }),
        "boundary result envelope may contain only tool_result block(s) before closure"
    );
    let ids = result_blocks
        .iter()
        .map(|block| required_string(block.get("tool_use_id"), "boundary tool_result ID"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        all_unique(&ids),
        "boundary envelope has duplicate tool_result IDs"
    );
    Ok((ids, result_blocks))
}

fn contains_exact_transport_text(messages: &[Value]) -> bool {
    messages.iter().any(|message| {
        let Some(content) = message.get("content") else {
            return false;
        };
        if let Some(text) = content.as_str() {
            return is_transport_text(text);
        }
        content.as_array().is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("type").and_then(Value::as_str) == Some("text")
                    && block
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(is_transport_text)
            })
        })
    })
}

fn is_transport_text(text: &str) -> bool {
    text == CONTINUE_TEXT || text == NO_RESPONSE_TEXT
}

/// Scan every string in the model-bound payload, including object keys. This
/// keeps newly introduced extension fields from becoming nonce-leak bypasses.
fn contains_nonce(value: &Value, nonce: &str) -> bool {
    match value {
        Value::String(text) => text.contains(nonce),
        Value::Array(values) => values.iter().any(|value| contains_nonce(value, nonce)),
        Value::Object(values) => values
            .iter()
            .any(|(key, value)| key.contains(nonce) || contains_nonce(value, nonce)),
        _ => false,
    }
}

fn canonical_cli_message_projection(messages: &[Value]) -> Vec<Value> {
    let mut projected = messages
        .iter()
        .filter(|message| !runtime_only_system_message(message))
        .cloned()
        .collect::<Vec<_>>();
    let Some(first) = projected.first_mut() else {
        return projected;
    };
    if first.get("role").and_then(Value::as_str) != Some("user") {
        return projected;
    }
    let Some(content) = first.get("content").and_then(Value::as_array) else {
        return projected;
    };
    if !content.iter().all(Value::is_object) {
        return projected;
    }
    let kept = content
        .iter()
        .filter(|block| {
            !(block.get("type").and_then(Value::as_str) == Some("text")
                && block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(is_current_date_reminder))
        })
        .cloned()
        .collect::<Vec<_>>();
    if kept.len() == 1
        && kept[0].get("type").and_then(Value::as_str) == Some("text")
        && kept[0].get("text").and_then(Value::as_str).is_some()
    {
        first["content"] = kept[0]["text"].clone();
    } else {
        first["content"] = Value::Array(kept);
    }
    projected
}

fn runtime_only_system_message(message: &Value) -> bool {
    if message.get("role").and_then(Value::as_str) != Some("system") {
        return false;
    }
    let Some(content) = message.get("content").and_then(Value::as_str) else {
        return false;
    };
    content.starts_with(SKILLS_SYSTEM_PREFIX)
        || content.starts_with(AGENT_TYPES_SYSTEM_PREFIX)
        || content.trim_end_matches('\n') == TASK_TOOL_REMINDER
        || is_task_reminder_with_tasks(content)
}

fn is_task_reminder_with_tasks(content: &str) -> bool {
    let Some(tasks) = content.strip_prefix(TASK_TOOL_REMINDER) else {
        return false;
    };
    let Some(tasks) = tasks.strip_prefix(TASKS_SUFFIX_PREFIX) else {
        return false;
    };
    !tasks.is_empty() && tasks.split('\n').all(valid_task_summary_line)
}

fn valid_task_summary_line(line: &str) -> bool {
    let Some(after_hash) = line.strip_prefix('#') else {
        return false;
    };
    let digit_count = after_hash.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count == 0 {
        return false;
    }
    let Some(summary) = after_hash[digit_count..].strip_prefix(". [") else {
        return false;
    };
    let Some((status, title)) = summary.split_once("] ") else {
        return false;
    };
    !status.is_empty() && !status.contains(']') && !title.is_empty()
}

fn is_current_date_reminder(text: &str) -> bool {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix(CURRENT_DATE_PREFIX) else {
        return false;
    };
    let Some(date) = rest.strip_suffix(CURRENT_DATE_SUFFIX) else {
        return false;
    };
    let bytes = date.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

fn all_unique(values: &[String]) -> bool {
    values.iter().collect::<BTreeSet<_>>().len() == values.len()
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_hex(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Render JSON with recursively sorted object keys, compact separators and
/// UTF-8 text, matching Python's `json.dumps(sort_keys=True,separators=(",",":"))`.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).expect("JSON string serialization"),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(values) => canonical_object(values),
    }
}

fn canonical_object(values: &Map<String, Value>) -> String {
    let mut keys = values.keys().collect::<Vec<_>>();
    keys.sort_unstable();
    let entries = keys
        .into_iter()
        .map(|key| {
            format!(
                "{}:{}",
                serde_json::to_string(key).expect("JSON key serialization"),
                canonical_json(&values[key])
            )
        })
        .collect::<Vec<_>>();
    format!("{{{}}}", entries.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_messages() -> Vec<Value> {
        vec![
            json!({"role": "user", "content": "fix it"}),
            json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "inspect"},
                    {
                        "type": "tool_use",
                        "id": "call-1",
                        "name": "Read",
                        "input": {"file_path": "/app/a.py"},
                    },
                    {
                        "type": "tool_use",
                        "id": "call-2",
                        "name": "Grep",
                        "input": {"pattern": "bug"},
                    },
                ],
            }),
            json!({
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "call-1", "content": "new read"},
                    {"type": "tool_result", "tool_use_id": "call-2", "content": "new grep"},
                ],
            }),
        ]
    }

    fn manifest_and_payload() -> (ResumeTransportManifest, Value) {
        let canonical = canonical_messages();
        let manifest = ResumeTransportManifest::create(
            "native-session",
            vec!["call-1".into(), "call-2".into()],
            canonical.clone(),
            "__SANDBOX_PLAYBACK_TEST_NONCE_0123456789__".into(),
        )
        .unwrap();
        let mut messages = canonical;
        messages.last_mut().unwrap()["content"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type": "text", "text": CONTINUE_TEXT}));
        messages.extend([
            json!({"role": "assistant", "content": NO_RESPONSE_TEXT}),
            json!({"role": "user", "content": manifest.nonce}),
        ]);
        (
            manifest,
            json!({"model": "test", "messages": messages, "stream": true}),
        )
    }

    #[test]
    fn manifest_and_first_request_cleanup_match_python_contract() {
        let (manifest, payload) = manifest_and_payload();
        let original = payload.clone();
        assert_eq!(
            manifest.canonical_prefix_sha256,
            "34fbd2a13b5788fbe6f11abaf47bf664316f75f28ae383a4637f5c42f9792837"
        );
        assert_eq!(
            manifest.boundary_observation_sha256[0],
            "581c12494899d692e1b8456047397b13c4140bfbed9e522b6fdb02ef325fa954"
        );
        let cleaned = clean_resume_transport_envelope(&payload, &manifest, 1).unwrap();
        assert_eq!(payload, original);
        assert_eq!(
            cleaned.payload["messages"],
            Value::Array(canonical_messages())
        );
        assert_eq!(cleaned.validation.sentinel_removed, 1);
        assert_eq!(cleaned.validation.closure_messages_removed, 2);
        assert_eq!(
            cleaned.validation.boundary_tool_use_ids,
            ["call-1", "call-2"]
        );
        assert_eq!(
            cleaned.validation.boundary_observation_sha256,
            manifest.boundary_observation_sha256
        );
        assert!(cleaned.validation.first_request_boundary_ok);
    }

    #[test]
    fn later_request_keeps_real_continuation_suffix() {
        let (manifest, mut payload) = manifest_and_payload();
        payload["messages"].as_array_mut().unwrap().extend([
            json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "call-3", "name": "Edit", "input": {}}],
            }),
            json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "call-3", "content": "done"}],
            }),
        ]);
        let cleaned = clean_resume_transport_envelope(&payload, &manifest, 2).unwrap();
        assert_eq!(
            &cleaned.payload["messages"].as_array().unwrap()[..3],
            canonical_messages().as_slice()
        );
        assert_eq!(cleaned.payload["messages"].as_array().unwrap().len(), 5);
        assert!(!cleaned.validation.first_request_boundary_ok);
    }

    #[test]
    fn first_request_drops_every_suffix_after_nonce() {
        let (manifest, mut payload) = manifest_and_payload();
        payload["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role": "user", "content": "unexpected suffix"}));
        let cleaned = clean_resume_transport_envelope(&payload, &manifest, 1).unwrap();
        assert_eq!(
            cleaned.payload["messages"],
            Value::Array(canonical_messages())
        );
    }

    #[test]
    fn audited_cli_reminders_are_projected_but_preserved_for_forwarding() {
        let (manifest, mut payload) = manifest_and_payload();
        let current_date = format!("{CURRENT_DATE_PREFIX}2026-07-31{CURRENT_DATE_SUFFIX}");
        payload["messages"][0]["content"] = json!([
            {"type": "text", "text": current_date},
            {"type": "text", "text": "fix it"},
        ]);
        payload["messages"].as_array_mut().unwrap().insert(
            1,
            json!({
                "role": "system",
                "content": format!("{SKILLS_SYSTEM_PREFIX}example: test skill"),
            }),
        );
        payload["messages"].as_array_mut().unwrap().insert(
            2,
            json!({
                "role": "system",
                "content": format!("{AGENT_TYPES_SYSTEM_PREFIX}- Explore: read-only"),
            }),
        );
        let envelope_start = payload["messages"].as_array().unwrap().len() - 4;
        payload["messages"].as_array_mut().unwrap().insert(
            envelope_start,
            json!({"role": "system", "content": TASK_TOOL_REMINDER}),
        );
        let cleaned = clean_resume_transport_envelope(&payload, &manifest, 1).unwrap();
        assert_eq!(
            cleaned.validation.clean_message_count,
            canonical_messages().len()
        );
        assert_eq!(
            cleaned.validation.clean_prefix_sha256,
            manifest.canonical_prefix_sha256
        );
        assert!(
            cleaned.payload["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["role"] == "system")
        );
    }

    #[test]
    fn reminder_with_existing_tasks_is_projected() {
        let (manifest, mut payload) = manifest_and_payload();
        let envelope_start = payload["messages"].as_array().unwrap().len() - 4;
        payload["messages"].as_array_mut().unwrap().insert(
            envelope_start,
            json!({
                "role": "system",
                "content": format!(
                    "{TASK_TOOL_REMINDER}{TASKS_SUFFIX_PREFIX}#1. [in_progress] Inspect code\n#2. [pending] Fix it"
                ),
            }),
        );
        let cleaned = clean_resume_transport_envelope(&payload, &manifest, 1).unwrap();
        assert_eq!(
            cleaned.validation.clean_message_count,
            canonical_messages().len()
        );
    }

    #[test]
    fn unknown_system_message_fails_prefix_integrity() {
        let (manifest, mut payload) = manifest_and_payload();
        payload["messages"]
            .as_array_mut()
            .unwrap()
            .insert(1, json!({"role": "system", "content": "unknown injection"}));
        let error = clean_resume_transport_envelope(&payload, &manifest, 1).unwrap_err();
        assert!(error.to_string().contains("canonical prefix hash"));
    }

    #[test]
    fn nonce_in_any_payload_value_or_key_is_rejected() {
        let (manifest, mut payload) = manifest_and_payload();
        payload["system"] = json!([{
            "type": "text",
            "text": format!("echo {}", manifest.nonce),
        }]);
        assert!(
            clean_resume_transport_envelope(&payload, &manifest, 1)
                .unwrap_err()
                .to_string()
                .contains("nonce remains anywhere")
        );

        let (manifest, mut payload) = manifest_and_payload();
        payload
            .as_object_mut()
            .unwrap()
            .insert(format!("extension-{}", manifest.nonce), Value::Bool(true));
        assert!(
            clean_resume_transport_envelope(&payload, &manifest, 1)
                .unwrap_err()
                .to_string()
                .contains("nonce remains anywhere")
        );
    }

    #[test]
    fn stale_observation_and_malformed_envelope_fail_closed() {
        let (manifest, mut stale) = manifest_and_payload();
        let last = stale["messages"].as_array().unwrap().len();
        stale["messages"][last - 3]["content"][0]["content"] = json!("stale");
        assert!(
            clean_resume_transport_envelope(&stale, &manifest, 1)
                .unwrap_err()
                .to_string()
                .contains("observation hash")
        );

        let (manifest, mut closure) = manifest_and_payload();
        let last = closure["messages"].as_array().unwrap().len();
        closure["messages"][last - 2]["content"] = json!("changed");
        assert!(
            clean_resume_transport_envelope(&closure, &manifest, 1)
                .unwrap_err()
                .to_string()
                .contains("assistant closure")
        );

        let (manifest, mut duplicate) = manifest_and_payload();
        duplicate["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role": "user", "content": manifest.nonce}));
        assert!(
            clean_resume_transport_envelope(&duplicate, &manifest, 1)
                .unwrap_err()
                .to_string()
                .contains("exactly one")
        );
    }

    #[test]
    fn canonical_hash_sorts_nested_object_keys() {
        let left = vec![json!({"role":"user", "content":{"b":2,"a":{"d":4,"c":3}}})];
        let right = vec![json!({"content":{"a":{"c":3,"d":4},"b":2}, "role":"user"})];
        assert_eq!(
            canonical_messages_sha256(&left).unwrap(),
            canonical_messages_sha256(&right).unwrap()
        );
    }

    #[test]
    fn manifest_rejects_profile_or_version_drift_after_deserialization() {
        let (manifest, _) = manifest_and_payload();
        let mut wrong_profile = manifest.clone();
        wrong_profile.profile_id = "claude-code/unknown/native-resume-v1".into();
        assert!(
            wrong_profile
                .validate()
                .unwrap_err()
                .to_string()
                .contains("unknown")
        );
        let mut wrong_version = manifest;
        wrong_version.claude_code_version = "2.1.71".into();
        assert!(
            wrong_version
                .validate()
                .unwrap_err()
                .to_string()
                .contains("requires")
        );
    }
}

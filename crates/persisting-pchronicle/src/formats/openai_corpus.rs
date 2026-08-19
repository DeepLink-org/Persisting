//! Lossless JSON-model import/export for OpenAI-message trajectory corpora.
//!
//! The reader accepts either a top-level row array or a `session_steps`
//! envelope containing rows from many sessions. Unmapped container and row
//! fields are retained as controlled residuals so strict recovery can rebuild
//! the original source document.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use chrono::{SecondsFormat, TimeZone, Utc};
use serde_json::{json, Map, Value};

use crate::formats::storyline::{
    StorylineAgent, StorylineDocument, StorylineToolCall, StorylineTurn,
};
use crate::{InputIssue, InputResult, Result};

const OPENAI_EXTENSION_KEY: &str = "persisting.dev/openai-msg/v1";
const ROW_METRIC_FIELDS: &[&str] = &[
    "reward",
    "step_reward",
    "is_terminal",
    "is_truncated",
    "is_session_completed",
    "is_trainable",
];

/// One source JSON file reconstructed from lossless OpenAI import metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveredOpenaiMsgFile {
    pub relative_path: PathBuf,
    pub document: Value,
}

/// Parse one OpenAI corpus JSON value into one Storyline per session.
pub fn parse_openai_msg_corpus_value(
    document: &Value,
    relative_path: impl AsRef<Path>,
) -> InputResult<Vec<StorylineDocument>> {
    let relative_path = validate_input_relative_path(relative_path.as_ref())?
        .to_string_lossy()
        .into_owned();
    let (kind, envelope, records) = match document {
        Value::Array(records) => ("array", None, records.clone()),
        Value::Object(root) => {
            let records = root
                .get("session_steps")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    InputIssue::invalid("OpenAI corpus object requires a session_steps array")
                })?
                .clone();
            let mut metadata = root.clone();
            metadata.remove("session_steps");
            ("envelope", Some(Value::Object(metadata)), records)
        }
        _ => {
            return Err(InputIssue::invalid(
                "OpenAI corpus must be a JSON array or session_steps object",
            ));
        }
    };

    let file_metadata = json!({
        "relative_path": relative_path,
        "document_kind": kind,
        "envelope": envelope,
    });
    let mut groups: Vec<(String, Vec<(usize, Value)>)> = Vec::new();
    let mut group_indexes = HashMap::<String, usize>::new();
    for (ordinal, record) in records.into_iter().enumerate() {
        let object = record.as_object().ok_or_else(|| {
            InputIssue::invalid("OpenAI corpus row must be an object")
                .at(format!("rows[{ordinal}]"))
        })?;
        let session_id = required_string(object, "session_id")
            .map_err(|error| error.at(format!("rows[{ordinal}].session_id")))?;
        let index = if let Some(index) = group_indexes.get(&session_id) {
            *index
        } else {
            let index = groups.len();
            group_indexes.insert(session_id.clone(), index);
            groups.push((session_id, Vec::new()));
            index
        };
        groups[index].1.push((ordinal, record));
    }

    if groups.is_empty() {
        return Err(InputIssue::unsupported("OpenAI corpus cannot be empty"));
    }

    groups
        .into_iter()
        .map(|(session_id, records)| {
            rows_to_storyline(&session_id, records, &relative_path, &file_metadata)
        })
        .collect()
}

/// Recover original OpenAI files from Storylines produced by the corpus reader.
///
/// This is intentionally strict: Storylines without complete lossless metadata
/// are rejected instead of being silently synthesized from normalized fields.
pub fn recover_openai_msg_files(
    stories: &[StorylineDocument],
) -> Result<Vec<RecoveredOpenaiMsgFile>> {
    #[derive(Clone)]
    struct FileGroup {
        kind: String,
        envelope: Option<Value>,
        records: Vec<(u64, Value)>,
    }

    let mut files = HashMap::<PathBuf, FileGroup>::new();
    for story in stories {
        let file = story
            .extra
            .as_ref()
            .and_then(|extra| extra.get(OPENAI_EXTENSION_KEY))
            .and_then(Value::as_object)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Storyline '{}' has no lossless OpenAI file metadata",
                    story.session_id
                )
            })?;
        let relative_path = file
            .get("relative_path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("OpenAI file metadata missing relative_path"))?;
        let relative_path = validate_relative_path(Path::new(relative_path))?;
        let kind = file
            .get("document_kind")
            .and_then(Value::as_str)
            .filter(|kind| matches!(*kind, "array" | "envelope"))
            .ok_or_else(|| anyhow::anyhow!("invalid OpenAI document_kind"))?
            .to_string();
        let envelope = file.get("envelope").filter(|v| !v.is_null()).cloned();

        let group = files
            .entry(relative_path.clone())
            .or_insert_with(|| FileGroup {
                kind: kind.clone(),
                envelope: envelope.clone(),
                records: Vec::new(),
            });
        if group.kind != kind || group.envelope != envelope {
            anyhow::bail!(
                "conflicting OpenAI file metadata for {}",
                relative_path.display()
            );
        }

        for turn in &story.turns {
            if turn.source == "user" {
                continue;
            }
            let extra = turn.extra.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "Storyline '{}' step {} has no OpenAI provenance",
                    story.session_id,
                    turn.id
                )
            })?;
            let Some(record) = extra.get(OPENAI_EXTENSION_KEY).and_then(Value::as_object) else {
                anyhow::bail!(
                    "Storyline '{}' step {} has no OpenAI residual",
                    story.session_id,
                    turn.id
                );
            };
            let record_path = record
                .get("relative_path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("OpenAI record missing relative_path"))?;
            if validate_relative_path(Path::new(record_path))? != relative_path {
                anyhow::bail!(
                    "OpenAI record path conflicts with Storyline '{}' file metadata",
                    story.session_id
                );
            }
            let ordinal = record
                .get("ordinal")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow::anyhow!("OpenAI record missing ordinal"))?;
            let raw = recover_record(story, turn, record)?;
            group.records.push((ordinal, raw));
        }
    }

    let mut output = Vec::with_capacity(files.len());
    for (relative_path, mut group) in files {
        group.records.sort_by_key(|(ordinal, _)| *ordinal);
        for pair in group.records.windows(2) {
            if pair[0].0 == pair[1].0 {
                anyhow::bail!(
                    "duplicate OpenAI row ordinal {} in {}",
                    pair[0].0,
                    relative_path.display()
                );
            }
        }
        // Ordinals are ordering keys, not a completeness proof. Callers may
        // intentionally export a filtered set of complete trajectories from
        // one source file, so gaps are valid while duplicates are not.
        let records = group
            .records
            .into_iter()
            .map(|(_, record)| record)
            .collect::<Vec<_>>();
        let document = match group.kind.as_str() {
            "array" => Value::Array(records),
            "envelope" => {
                let mut envelope = group
                    .envelope
                    .and_then(|value| value.as_object().cloned())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "OpenAI envelope metadata missing for {}",
                            relative_path.display()
                        )
                    })?;
                envelope.insert("session_steps".into(), Value::Array(records));
                Value::Object(envelope)
            }
            kind => {
                anyhow::bail!(
                    "invalid OpenAI document kind '{}' while recovering {}",
                    kind,
                    relative_path.display()
                )
            }
        };
        output.push(RecoveredOpenaiMsgFile {
            relative_path,
            document,
        });
    }
    output.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(output)
}

pub(crate) fn has_openai_provenance(story: &StorylineDocument) -> bool {
    story
        .extra
        .as_ref()
        .and_then(|extra| extra.get(OPENAI_EXTENSION_KEY))
        .and_then(Value::as_object)
        .is_some()
}

/// Explicitly synthesize an OpenAI message row array from Storyline semantics.
///
/// This is a cross-format projection, not a lossless recovery operation. Use
/// [`recover_openai_msg_files`] when the Storylines originated from an OpenAI
/// corpus and exact JSON-model recovery is required.
pub fn synthesize_openai_msg_corpus(stories: &[StorylineDocument]) -> Result<Value> {
    let mut records = Vec::new();
    for story in stories {
        story.validate()?;
        let mut index = 0usize;
        while index < story.turns.len() {
            let turn = &story.turns[index];
            let (user, agent) = if turn.source == "user" {
                let agent = story
                    .turns
                    .get(index + 1)
                    .filter(|turn| turn.source == "agent");
                index += if agent.is_some() { 2 } else { 1 };
                (Some(turn), agent)
            } else if turn.source == "agent" {
                index += 1;
                (None, Some(turn))
            } else {
                anyhow::bail!(
                    "OpenAI synthesis cannot represent Storyline turn {} source '{}'",
                    turn.id,
                    turn.source
                );
            };
            if user.is_some() && agent.is_none() {
                anyhow::bail!(
                    "OpenAI synthesis requires an agent response after user turn {}",
                    turn.id
                );
            }
            let output = agent
                .or(user)
                .ok_or_else(|| anyhow::anyhow!("cannot synthesize an empty OpenAI message step"))?;
            let mut messages = agent
                .and_then(|turn| turn.extra.as_ref())
                .and_then(|extra| extra.get("request_messages"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if messages.is_empty() {
                if let Some(user) = user {
                    messages.push(json!({"role": "user", "content": user.message}));
                }
            }
            let response = agent.map(|turn| {
                json!({
                    "role": "assistant",
                    "content": crate::convert::message_text(&turn.message)
                        .map(Value::String)
                        .unwrap_or_else(|| turn.message.clone()),
                })
            });
            let call_id = agent
                .and_then(|turn| turn.extra.as_ref())
                .and_then(|extra| extra.get("call_id"))
                .and_then(Value::as_str)
                .unwrap_or("");
            records.push(json!({
                "id": format!("step-{}", output.id),
                "session_id": story.session_id,
                "step_id": output.id,
                "job_id": "",
                "agent_id": story.agent.id,
                "group_id": "",
                "env_name": "",
                "llm_model": agent.and_then(|turn| turn.model_name.clone()).unwrap_or_default(),
                "step_reward": 0.0,
                "reward": 0.0,
                "is_terminal": index >= story.turns.len(),
                "is_truncated": false,
                "is_session_completed": index >= story.turns.len(),
                "is_trainable": true,
                "created_at": output.timestamp.clone().unwrap_or_default(),
                "messages": messages,
                "response": response,
                "run_bucket": story.run_id.clone().unwrap_or_default(),
                "call_id": call_id,
            }));
        }
    }
    Ok(Value::Array(records))
}

fn rows_to_storyline(
    session_id: &str,
    mut records: Vec<(usize, Value)>,
    relative_path: &str,
    file_metadata: &Value,
) -> InputResult<StorylineDocument> {
    records.sort_by_key(|(_, row)| row.get("step_id").and_then(Value::as_i64));
    let mut seen_steps = HashSet::new();
    let mut turns = Vec::with_capacity(records.len().saturating_mul(2));
    let mut agent_source = None;
    let mut first_model: Option<String> = None;
    let mut run_id: Option<String> = None;
    let mut next_turn_id = 1_i64;

    for (ordinal, raw) in records {
        let row = raw.as_object().ok_or_else(|| {
            InputIssue::invalid("OpenAI corpus row must be an object")
                .at(format!("rows[{ordinal}]"))
        })?;
        let step_id = row.get("step_id").and_then(Value::as_i64).ok_or_else(|| {
            InputIssue::invalid("OpenAI corpus row requires integer step_id")
                .at(format!("rows[{ordinal}].step_id"))
        })?;
        if !seen_steps.insert(step_id) {
            return Err(InputIssue::invalid(format!("duplicate step_id {step_id}"))
                .at(format!("rows[{ordinal}].step_id")));
        }
        let meta = parsed_meta(row);
        let env_state = parsed_env_state(meta.as_ref());
        let model = row
            .get("agent_model")
            .or_else(|| row.get("llm_model"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if first_model.is_none() {
            first_model = model.clone();
        }
        if agent_source.is_none() {
            agent_source = meta
                .as_ref()
                .and_then(|value| value.get("source"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
        if let Some(candidate) = ["run_id", "run_bucket", "job_id"]
            .into_iter()
            .find_map(|field| {
                row.get(field)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
            })
        {
            if let Some(existing) = &run_id {
                if existing != candidate {
                    return Err(InputIssue::invalid(
                        "OpenAI corpus session has conflicting run ids",
                    )
                    .at(format!("rows[{ordinal}]")));
                }
            } else {
                run_id = Some(candidate.to_string());
            }
        }

        let (output, output_location) = select_output_message(row).ok_or_else(|| {
            InputIssue::invalid("OpenAI corpus row has no assistant output")
                .at(format!("rows[{ordinal}]"))
        })?;
        let tool_calls = parse_tool_calls(output.get("tool_calls"))
            .or_else(|| parse_embedded_tool_call(output.get("content"), step_id));
        let message = output.get("content").cloned().unwrap_or(Value::Null);
        let metrics = normalized_metrics(row, env_state.as_ref());
        let timestamp = env_state
            .as_ref()
            .and_then(|state| state.get("created_at"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| row.get("created_at").and_then(normalize_timestamp));
        let latency_ms = env_state
            .as_ref()
            .and_then(|state| state.get("total_latency_ms"))
            .and_then(number_to_i64);
        let ttft_ms = env_state
            .as_ref()
            .and_then(|state| state.get("ttft_ms"))
            .and_then(number_to_i64);

        let call_id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("step-{step_id}"));
        let request_messages = row.get("messages").cloned();
        let user_message = last_user_message(request_messages.as_ref());
        let user_turn_id = user_message.as_ref().map(|_| next_turn_id);
        if let Some((_, message)) = user_message.as_ref() {
            turns.push(StorylineTurn {
                id: next_turn_id,
                kind: Some("llm.request".into()),
                timestamp: timestamp.clone(),
                source: "user".into(),
                message: message.clone(),
                reasoning_content: None,
                reasoning_effort: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                model_name: None,
                llm_call_count: None,
                is_copied_context: None,
                latency_ms: None,
                ttft_ms: None,
                extra: Some(json!({
                    "call_id": call_id,
                    OPENAI_EXTENSION_KEY: {
                        "kind": "request",
                        "openai_step_id": step_id,
                    }
                })),
            });
            next_turn_id += 1;
        }

        turns.push(StorylineTurn {
            id: next_turn_id,
            kind: Some(if tool_calls.is_some() {
                "autonomous".into()
            } else {
                "llm.response".into()
            }),
            timestamp,
            source: "agent".into(),
            message,
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls,
            observation: None,
            metrics,
            model_name: model,
            llm_call_count: Some(1),
            is_copied_context: None,
            latency_ms,
            ttft_ms,
            extra: Some(json!({
                "call_id": call_id,
                OPENAI_EXTENSION_KEY: record_residual(
                    row,
                    relative_path,
                    ordinal,
                    step_id,
                    user_message.as_ref().map(|(index, _)| *index),
                    user_turn_id,
                    output_location,
                    env_state.as_ref(),
                )
            })),
        });
        next_turn_id += 1;
    }

    let final_metrics = turns.last().and_then(|turn| turn.metrics.clone());
    let agent_id = agent_source
        .or_else(|| first_model.clone())
        .unwrap_or_else(|| "openai-import".into());
    Ok(StorylineDocument {
        schema_version: None,
        run_id,
        trajectory_id: None,
        attempt_id: None,
        session_id: session_id.to_string(),
        agent: StorylineAgent {
            id: agent_id.clone(),
            name: Some(agent_id),
            version: None,
            model_name: first_model,
            tool_definitions: None,
            extra: None,
        },
        parent: None,
        child_session_ids: None,
        notes: None,
        final_metrics,
        continued_trajectory_ref: None,
        extra: Some(json!({ OPENAI_EXTENSION_KEY: file_metadata })),
        presence: Default::default(),
        turns,
    })
}

fn last_user_message(messages: Option<&Value>) -> Option<(usize, Value)> {
    messages?
        .as_array()?
        .iter()
        .enumerate()
        .rev()
        .find(|(_, message)| message.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|(index, message)| message.get("content").cloned().map(|value| (index, value)))
}

#[allow(clippy::too_many_arguments)]
fn record_residual(
    row: &Map<String, Value>,
    relative_path: &str,
    ordinal: usize,
    step_id: i64,
    user_message_index: Option<usize>,
    user_turn_id: Option<i64>,
    output_location: OutputLocation,
    env_state: Option<&Value>,
) -> Value {
    let mut residual = row.clone();
    for key in ["session_id", "step_id", "messages", "response"] {
        residual.remove(key);
    }

    let id_original = residual.remove("id");
    let id_present = id_original.is_some();
    let id_normalized = row
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("step-{step_id}"));
    let model_key = ["agent_model", "llm_model"]
        .into_iter()
        .find(|key| row.get(*key).and_then(Value::as_str).is_some())
        .map(str::to_string);
    if let Some(key) = &model_key {
        residual.remove(key);
    }
    let run_key = ["run_id", "run_bucket", "job_id"]
        .into_iter()
        .find(|key| {
            row.get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        })
        .map(str::to_string);
    if let Some(key) = &run_key {
        residual.remove(key);
    }

    let metric_fields = ROW_METRIC_FIELDS
        .iter()
        .filter(|field| row.contains_key(**field))
        .map(|field| Value::String((*field).to_string()))
        .collect::<Vec<_>>();
    for field in ROW_METRIC_FIELDS {
        residual.remove(*field);
    }

    let timestamp_from_env = env_state
        .and_then(|value| value.get("created_at"))
        .and_then(Value::as_str)
        .is_some();
    let (created_at_kind, created_at_original, created_at_normalized) = if timestamp_from_env {
        (None, None, None)
    } else {
        row.get("created_at").map_or((None, None, None), |value| {
            residual.remove("created_at");
            let kind = match value {
                Value::String(_) => "string",
                Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
                Value::Number(_) => "float",
                _ => "other",
            };
            (
                Some(kind),
                Some(value.clone()),
                normalize_timestamp(value).map(Value::String),
            )
        })
    };

    let mut messages = row.get("messages").cloned();
    if let Some(values) = messages.as_mut().and_then(Value::as_array_mut) {
        if let Some(index) = user_message_index {
            if let Some(message) = values.get_mut(index).and_then(Value::as_object_mut) {
                message.remove("content");
            }
        }
        if let OutputLocation::Message(index) = output_location {
            if let Some(message) = values.get_mut(index).and_then(Value::as_object_mut) {
                message.remove("content");
                if parse_tool_calls(message.get("tool_calls")).is_some() {
                    message.remove("tool_calls");
                }
            }
        }
    }
    let mut response = row.get("response").cloned();
    if matches!(output_location, OutputLocation::Response) {
        if let Some(message) = response.as_mut().and_then(Value::as_object_mut) {
            message.remove("content");
            if parse_tool_calls(message.get("tool_calls")).is_some() {
                message.remove("tool_calls");
            }
        }
    }

    let (output_kind, output_index) = match output_location {
        OutputLocation::Response => ("response", None),
        OutputLocation::Message(index) => ("message", Some(index)),
    };
    json!({
        "relative_path": relative_path,
        "ordinal": ordinal,
        "step_id": step_id,
        "user_message_index": user_message_index,
        "user_turn_id": user_turn_id,
        "output_kind": output_kind,
        "output_index": output_index,
        "id_present": id_present,
        "id_original": id_original,
        "id_normalized": id_normalized,
        "model_key": model_key,
        "run_key": run_key,
        "metric_fields": metric_fields,
        "created_at_kind": created_at_kind,
        "created_at_original": created_at_original,
        "created_at_normalized": created_at_normalized,
        "messages": messages,
        "response": response,
        "residual": residual,
    })
}

fn recover_record(
    story: &StorylineDocument,
    agent_turn: &StorylineTurn,
    metadata: &Map<String, Value>,
) -> Result<Value> {
    let mut record = metadata
        .get("residual")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("OpenAI record residual must be an object"))?;
    let step_id = metadata
        .get("step_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("OpenAI record residual missing step_id"))?;
    insert_authoritative(
        &mut record,
        "session_id",
        Value::String(story.session_id.clone()),
        "record",
    );
    insert_authoritative(&mut record, "step_id", json!(step_id), "record");

    if metadata
        .get("id_present")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let call_id = agent_turn
            .extra
            .as_ref()
            .and_then(|value| value.get("call_id"))
            .and_then(Value::as_str);
        let normalized = metadata.get("id_normalized").and_then(Value::as_str);
        let value = if call_id == normalized {
            metadata.get("id_original").cloned().unwrap_or(Value::Null)
        } else {
            call_id.map_or(Value::Null, |value| Value::String(value.to_string()))
        };
        insert_authoritative(&mut record, "id", value, "record");
    }
    if let Some(key) = metadata.get("model_key").and_then(Value::as_str) {
        if let Some(model) = &agent_turn.model_name {
            insert_authoritative(&mut record, key, Value::String(model.clone()), "record");
        }
    }
    if let Some(key) = metadata.get("run_key").and_then(Value::as_str) {
        if let Some(run_id) = &story.run_id {
            insert_authoritative(&mut record, key, Value::String(run_id.clone()), "record");
        }
    }
    if let Some(fields) = metadata.get("metric_fields").and_then(Value::as_array) {
        for field in fields.iter().filter_map(Value::as_str) {
            if let Some(value) = agent_turn
                .metrics
                .as_ref()
                .and_then(|value| value.get(field))
            {
                insert_authoritative(&mut record, field, value.clone(), "record");
            }
        }
    }
    if let Some(kind) = metadata.get("created_at_kind").and_then(Value::as_str) {
        let encoded = match agent_turn.timestamp.as_deref() {
            Some(timestamp)
                if metadata
                    .get("created_at_normalized")
                    .and_then(Value::as_str)
                    == Some(timestamp) =>
            {
                metadata
                    .get("created_at_original")
                    .cloned()
                    .unwrap_or(encode_timestamp(timestamp, kind)?)
            }
            Some(timestamp) => encode_timestamp(timestamp, kind)?,
            None => metadata
                .get("created_at_original")
                .cloned()
                .unwrap_or(Value::Null),
        };
        insert_authoritative(&mut record, "created_at", encoded, "record");
    }

    let user_turn = metadata
        .get("user_turn_id")
        .and_then(Value::as_i64)
        .and_then(|id| story.turns.iter().find(|turn| turn.id == id));
    let output_kind = metadata
        .get("output_kind")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("OpenAI record residual missing output_kind"))?;
    let output_index = metadata.get("output_index").and_then(Value::as_u64);

    if let Some(mut messages) = metadata.get("messages").filter(|v| !v.is_null()).cloned() {
        let values = messages
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("OpenAI messages residual must be an array"))?;
        if let (Some(index), Some(user_turn)) = (
            metadata
                .get("user_message_index")
                .and_then(Value::as_u64)
                .map(|value| value as usize),
            user_turn,
        ) {
            let message = values
                .get_mut(index)
                .and_then(Value::as_object_mut)
                .ok_or_else(|| anyhow::anyhow!("OpenAI user message residual is invalid"))?;
            message.insert("content".into(), user_turn.message.clone());
        }
        if output_kind == "message" {
            let index = output_index
                .ok_or_else(|| anyhow::anyhow!("OpenAI output message index is missing"))?
                as usize;
            let message = values
                .get_mut(index)
                .and_then(Value::as_object_mut)
                .ok_or_else(|| anyhow::anyhow!("OpenAI output message residual is invalid"))?;
            apply_output(message, agent_turn)?;
        }
        insert_authoritative(&mut record, "messages", messages, "record");
    }

    if let Some(mut response) = metadata.get("response").filter(|v| !v.is_null()).cloned() {
        if output_kind == "response" {
            let message = response
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("OpenAI response residual must be an object"))?;
            apply_output(message, agent_turn)?;
        }
        insert_authoritative(&mut record, "response", response, "record");
    }
    Ok(Value::Object(record))
}

fn apply_output(message: &mut Map<String, Value>, turn: &StorylineTurn) -> Result<()> {
    message.insert("content".into(), turn.message.clone());
    if let Some(calls) = &turn.tool_calls {
        message.insert("tool_calls".into(), encode_tool_calls(calls)?);
    }
    Ok(())
}

fn insert_authoritative(target: &mut Map<String, Value>, key: &str, value: Value, scope: &str) {
    if target.contains_key(key) {
        tracing::warn!(
            source_format = "openai-msg",
            source_key = key,
            target_key = key,
            scope,
            "OpenAI residual conflicts with an authoritative Storyline field"
        );
    }
    target.insert(key.to_string(), value);
}

fn encode_timestamp(timestamp: &str, kind: &str) -> Result<Value> {
    if kind == "string" {
        return Ok(Value::String(timestamp.to_string()));
    }
    if kind == "other" {
        return Ok(Value::String(timestamp.to_string()));
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(timestamp)?;
    if kind == "integer" && parsed.timestamp_subsec_nanos() == 0 {
        Ok(json!(parsed.timestamp()))
    } else {
        Ok(json!(
            parsed.timestamp() as f64
                + f64::from(parsed.timestamp_subsec_nanos()) / 1_000_000_000.0
        ))
    }
}

fn validate_relative_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        anyhow::bail!(
            "OpenAI source path must be non-empty and relative: {}",
            path.display()
        );
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        anyhow::bail!(
            "OpenAI source path contains unsafe components: {}",
            path.display()
        );
    }
    Ok(path.to_path_buf())
}

fn validate_input_relative_path(path: &Path) -> InputResult<PathBuf> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(InputIssue::invalid("source path must be non-empty and relative").at("path"));
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(InputIssue::invalid("source path contains unsafe components").at("path"));
    }
    Ok(path.to_path_buf())
}

fn required_string(row: &Map<String, Value>, field: &str) -> InputResult<String> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| InputIssue::invalid(format!("OpenAI corpus row requires non-empty {field}")))
}

fn parsed_meta(row: &Map<String, Value>) -> Option<Value> {
    match row.get("meta_json")? {
        Value::String(value) => serde_json::from_str(value).ok(),
        value @ Value::Object(_) => Some(value.clone()),
        _ => None,
    }
}

fn parsed_env_state(meta: Option<&Value>) -> Option<Value> {
    match meta?.get("env_state")? {
        Value::String(value) => serde_json::from_str(value).ok(),
        value @ Value::Object(_) => Some(value.clone()),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
enum OutputLocation {
    Response,
    Message(usize),
}

fn select_output_message(
    row: &Map<String, Value>,
) -> Option<(&Map<String, Value>, OutputLocation)> {
    let response = row.get("response").and_then(Value::as_object);
    if response.is_some_and(message_has_output) {
        return response.map(|value| (value, OutputLocation::Response));
    }
    row.get("messages")?
        .as_array()?
        .iter()
        .enumerate()
        .rev()
        .filter_map(|(index, value)| value.as_object().map(|message| (index, message)))
        .find(|(_, message)| {
            message.get("role").and_then(Value::as_str) == Some("assistant")
                && message_has_output(message)
        })
        .map(|(index, message)| (message, OutputLocation::Message(index)))
}

fn message_has_output(message: &Map<String, Value>) -> bool {
    let has_tools = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty());
    has_tools || message.get("content").is_some_and(content_has_value)
}

fn content_has_value(content: &Value) -> bool {
    match content {
        Value::Null => false,
        Value::String(value) => !value.is_empty(),
        Value::Array(values) => values.iter().any(content_has_value),
        Value::Object(value) => value
            .get("text")
            .map_or(!value.is_empty(), content_has_value),
        _ => true,
    }
}

fn parse_tool_calls(value: Option<&Value>) -> Option<Vec<StorylineToolCall>> {
    let calls = value?.as_array()?;
    let parsed = calls
        .iter()
        .filter_map(|call| {
            let call = call.as_object()?;
            let function = call.get("function")?.as_object()?;
            let tool_call_id = call.get("id")?.as_str()?.to_string();
            let function_name = function.get("name")?.as_str()?.to_string();
            if tool_call_id.is_empty() || function_name.is_empty() {
                return None;
            }
            let arguments = function.get("arguments").cloned().unwrap_or(Value::Null);
            let arguments = match arguments {
                Value::String(ref text) => {
                    serde_json::from_str(text).unwrap_or_else(|_| arguments.clone())
                }
                _ => arguments,
            };
            let mut call_residual = call.clone();
            call_residual.remove("id");
            call_residual.remove("type");
            call_residual.remove("function");
            let mut function_residual = function.clone();
            function_residual.remove("name");
            let raw_arguments = function_residual.remove("arguments");
            Some(StorylineToolCall {
                tool_call_id,
                function_name,
                arguments,
                result: Default::default(),
                duration_ms: None,
                extra: Some(json!({
                    OPENAI_EXTENSION_KEY: {
                        "kind": "tool_call",
                        "type": call.get("type"),
                        "call": call_residual,
                        "function": function_residual,
                        "arguments_were_string": raw_arguments.is_some_and(|value| value.is_string()),
                    }
                })),
            })
        })
        .collect::<Vec<_>>();
    (!parsed.is_empty()).then_some(parsed)
}

fn encode_tool_calls(calls: &[StorylineToolCall]) -> Result<Value> {
    calls
        .iter()
        .map(|call| {
            let metadata = call
                .extra
                .as_ref()
                .and_then(|value| value.get(OPENAI_EXTENSION_KEY))
                .and_then(Value::as_object);
            let mut output = metadata
                .and_then(|value| value.get("call"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            output.insert("id".into(), Value::String(call.tool_call_id.clone()));
            if let Some(kind) = metadata.and_then(|value| value.get("type")) {
                output.insert("type".into(), kind.clone());
            }
            let mut function = metadata
                .and_then(|value| value.get("function"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            function.insert("name".into(), Value::String(call.function_name.clone()));
            let arguments = if metadata
                .and_then(|value| value.get("arguments_were_string"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                Value::String(serde_json::to_string(&call.arguments)?)
            } else {
                call.arguments.clone()
            };
            function.insert("arguments".into(), arguments);
            output.insert("function".into(), Value::Object(function));
            Ok(Value::Object(output))
        })
        .collect::<Result<Vec<_>>>()
        .map(Value::Array)
}

fn parse_embedded_tool_call(
    content: Option<&Value>,
    step_id: i64,
) -> Option<Vec<StorylineToolCall>> {
    let text = message_text(content?)?;
    let name = text
        .split_once("<tool_call>")
        .map(|(_, value)| value)
        .or_else(|| text.split_once("<function=").map(|(_, value)| value))?;
    let name = name.trim().split(['>', '\n', '<']).next()?.trim();
    if name.is_empty() {
        return None;
    }
    let mut arguments = serde_json::Map::new();
    let mut remaining = text.as_str();
    while let Some((_, after_marker)) = remaining.split_once("<parameter=") {
        let Some((key, after_opening)) = after_marker.split_once('>') else {
            break;
        };
        let key = key.trim();
        if key.is_empty() {
            remaining = after_opening;
            continue;
        }
        let (value, rest) = after_opening
            .split_once("</parameter>")
            .unwrap_or((after_opening, ""));
        arguments.insert(key.to_string(), Value::String(value.trim().to_string()));
        remaining = rest;
    }
    Some(vec![StorylineToolCall {
        tool_call_id: format!("embedded-{step_id}-{name}"),
        function_name: name.to_string(),
        arguments: Value::Object(arguments),
        result: Default::default(),
        duration_ms: None,
        extra: Some(json!({"encoding":"embedded_text"})),
    }])
}

fn message_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| {
                    part.as_str()
                        .or_else(|| part.get("text").and_then(Value::as_str))
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

fn normalized_metrics(row: &Map<String, Value>, env_state: Option<&Value>) -> Option<Value> {
    const ENV_FIELDS: &[&str] = &[
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "finish_reason",
        "status_code",
        "retry_count",
        "upstream_latency_ms",
        "gateway_overhead_ms",
        "total_latency_ms",
        "ttft_ms",
    ];
    let mut metrics = Map::new();
    for field in ROW_METRIC_FIELDS {
        if let Some(value) = row.get(*field) {
            metrics.insert((*field).to_string(), value.clone());
        }
    }
    if let Some(env_state) = env_state.and_then(Value::as_object) {
        for field in ENV_FIELDS {
            if let Some(value) = env_state.get(*field) {
                metrics.insert((*field).to_string(), value.clone());
            }
        }
    }
    (!metrics.is_empty()).then_some(Value::Object(metrics))
}

fn normalize_timestamp(value: &Value) -> Option<String> {
    if let Some(value) = value.as_str() {
        return Some(value.to_string());
    }
    let seconds = value.as_f64()?;
    let mut whole = seconds.floor() as i64;
    let mut nanos = ((seconds - whole as f64) * 1_000_000_000.0).round() as u32;
    if nanos == 1_000_000_000 {
        whole = whole.checked_add(1)?;
        nanos = 0;
    }
    Utc.timestamp_opt(whole, nanos)
        .single()
        .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::AutoSi, true))
}

fn number_to_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "lance-store")]
    use crate::store::StorylineLanceStore;

    fn corpus() -> Value {
        json!([
            {
                "id": "evt-2",
                "session_id": "s-1",
                "step_id": 2,
                "agent_model": "gpt-test",
                "created_at": 1_700_000_001,
                "messages": [
                    {"role":"user","content":[{"type":"text","text":"next"}]},
                    {"role":"assistant","content":[{"type":"text","text":"world"}]}
                ],
                "response": {"role":"assistant","content":[]},
                "reward": 1.0,
                "unknown": null
            },
            {
                "id": "evt-other",
                "session_id": "s-2",
                "step_id": 1,
                "agent_model": "gpt-test",
                "messages": [
                    {"role":"user","content":"tool"},
                    {"role":"assistant","content":null,"tool_calls":[{
                        "id":"call-1","type":"function",
                        "function":{"name":"lookup","arguments":"{\"q\":1}"}
                    }]}
                ],
                "response": {"role":"assistant","content":""}
            },
            {
                "id": "evt-1",
                "session_id": "s-1",
                "step_id": 1,
                "agent_model": "gpt-test",
                "created_at": 1_700_000_000,
                "messages": [
                    {"role":"system","content":"system"},
                    {"role":"user","content":"hello"},
                    {"role":"assistant","content":"answer"}
                ],
                "response": {"role":"assistant","content":""},
                "meta_json": "{\"source\":\"fixture\",\"env_state\":\"{\\\"created_at\\\":\\\"2026-01-01T00:00:00Z\\\",\\\"total_tokens\\\":3}\"}"
            }
        ])
    }

    #[test]
    fn corpus_roundtrip_is_json_semantically_lossless() {
        let input = corpus();
        let stories = parse_openai_msg_corpus_value(&input, "corpus.json").unwrap();
        assert_eq!(stories.len(), 2);
        assert_eq!(stories[0].turns.len(), 4);
        assert_eq!(stories[0].turns[0].id, 1);
        assert_eq!(stories[0].turns[1].id, 2);
        assert_eq!(stories[0].turns[0].source, "user");
        assert_eq!(stories[0].turns[0].message, json!("hello"));
        assert_eq!(stories[0].turns[1].source, "agent");
        assert_eq!(stories[0].turns[1].message, json!("answer"));
        assert_eq!(stories[1].turns[1].tool_calls.as_ref().unwrap().len(), 1);

        let recovered = recover_openai_msg_files(&stories).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].relative_path, PathBuf::from("corpus.json"));
        assert_eq!(recovered[0].document, input);
    }

    #[test]
    fn synthesis_rejects_user_turn_without_agent_response() {
        let mut stories = parse_openai_msg_corpus_value(&corpus(), "corpus.json").unwrap();
        stories[0].turns.truncate(1);

        let error = synthesize_openai_msg_corpus(&stories[..1]).unwrap_err();
        assert!(error
            .to_string()
            .contains("OpenAI synthesis requires an agent response after user turn 1"));

        stories[0].turns[0].source = "system".into();
        let error = synthesize_openai_msg_corpus(&stories[..1]).unwrap_err();
        assert!(error
            .to_string()
            .contains("OpenAI synthesis cannot represent Storyline turn 1 source 'system'"));
    }

    #[test]
    fn openai_residual_preserves_unknowns_but_storyline_content_is_authoritative() {
        let input = corpus();
        let mut stories = parse_openai_msg_corpus_value(&input, "corpus.json").unwrap();
        assert!(!serde_json::to_string(&stories)
            .unwrap()
            .contains(&["_pchron", "icle_"].concat()));

        stories[0].turns[0].message = json!("edited user");
        stories[0].turns[1].message = json!("edited assistant");
        let recovered = recover_openai_msg_files(&stories).unwrap();
        let rows = recovered[0].document.as_array().unwrap();
        let first_session_row = rows.iter().find(|row| row["id"] == "evt-1").unwrap();
        assert_eq!(first_session_row["messages"][1]["content"], "edited user");
        assert_eq!(
            first_session_row["messages"][2]["content"],
            "edited assistant"
        );
        assert_eq!(rows[0]["unknown"], Value::Null);
    }

    #[test]
    fn envelope_roundtrip_preserves_root_metadata() {
        let input = json!({
            "session_id": "s-1",
            "custom": null,
            "session_steps": [corpus()[0].clone()]
        });
        let stories = parse_openai_msg_corpus_value(&input, "session_steps.json").unwrap();
        let recovered = recover_openai_msg_files(&stories).unwrap();
        assert_eq!(recovered[0].document, input);
    }

    #[test]
    fn corpus_preserves_run_group_and_user_agent_turns() {
        let input = json!([{
            "id": "call-1",
            "session_id": "child-session",
            "job_id": "shared-run",
            "step_id": 7,
            "messages": [{"role":"user","content":"question"}],
            "response": {"role":"assistant","content":"answer"},
            "is_session_completed": true
        }]);
        let stories = parse_openai_msg_corpus_value(&input, "gateway.json").unwrap();
        assert_eq!(stories[0].run_id.as_deref(), Some("shared-run"));
        assert_eq!(stories[0].turns.len(), 2);
        assert_eq!(stories[0].turns[0].source, "user");
        assert_eq!(stories[0].turns[1].source, "agent");
        assert_eq!(
            recover_openai_msg_files(&stories).unwrap()[0].document,
            input
        );
    }

    #[test]
    fn semantic_encoder_does_not_silently_synthesize_mixed_provenance() {
        let mut stories = parse_openai_msg_corpus_value(&corpus(), "corpus.json").unwrap();
        let mut unrelated = stories[0].clone();
        unrelated.session_id = "unrelated".into();
        unrelated.trajectory_id = Some("unrelated".into());
        unrelated.extra = None;
        stories.push(unrelated);

        let error = crate::document::encode_json_storylines(
            crate::format::DocumentFormat::OpenaiMsg,
            &stories,
        )
        .unwrap_err();
        assert!(error.to_string().contains("OpenAI"), "{error}");
    }

    #[test]
    fn recovery_rejects_unsafe_paths() {
        let error = parse_openai_msg_corpus_value(&corpus(), "../escape.json").unwrap_err();
        assert!(error.to_string().contains("unsafe"));
        assert_eq!(error.kind(), crate::input::InputIssueKind::Invalid);
        assert_eq!(error.location(), Some("path"));
        assert!(!error.message().contains("../escape.json"));
    }

    #[test]
    fn embedded_text_tool_calls_are_normalized() {
        let calls = parse_embedded_tool_call(
            Some(&json!([{
                "type":"text",
                "text":"<tool_call>execute_ipython_cell\n<parameter=code>print('ok')</parameter>"
            }])),
            7,
        )
        .unwrap();
        assert_eq!(calls[0].function_name, "execute_ipython_cell");
        assert_eq!(calls[0].tool_call_id, "embedded-7-execute_ipython_cell");
        assert_eq!(calls[0].arguments["code"], "print('ok')");
    }

    #[cfg(feature = "lance-store")]
    #[tokio::test]
    async fn corpus_import_and_recovery_roundtrip_through_lance() {
        let input = corpus();
        let expected = parse_openai_msg_corpus_value(&input, "corpus.json").unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(temporary.path()).await.unwrap();
        store.replace_storylines(&expected).await.unwrap();

        let session_ids = expected
            .iter()
            .map(|story| story.session_id.clone())
            .collect::<Vec<_>>();
        let restored = store
            .get_storylines_full(&session_ids)
            .await
            .unwrap()
            .into_iter()
            .map(Option::unwrap)
            .collect::<Vec<_>>();
        let recovered = recover_openai_msg_files(&restored).unwrap();

        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].relative_path, PathBuf::from("corpus.json"));
        assert_eq!(recovered[0].document, input);
    }

    #[cfg(feature = "lance-store")]
    #[tokio::test]
    async fn fractional_created_at_is_lossless_through_lance() {
        let mut input = corpus();
        input[0]["created_at"] = json!(1_700_000_001.123_456_f64);
        let expected = parse_openai_msg_corpus_value(&input, "fractional.json").unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(temporary.path()).await.unwrap();
        store.replace_storylines(&expected).await.unwrap();

        let session_ids = expected
            .iter()
            .map(|story| story.session_id.clone())
            .collect::<Vec<_>>();
        let restored = store
            .get_storylines_full(&session_ids)
            .await
            .unwrap()
            .into_iter()
            .map(Option::unwrap)
            .collect::<Vec<_>>();

        assert_eq!(
            recover_openai_msg_files(&restored).unwrap()[0].document,
            input
        );
    }

    #[cfg(feature = "lance-store")]
    #[tokio::test]
    async fn explicit_null_id_and_created_at_are_lossless_through_lance() {
        let input = json!([{
            "id": null,
            "session_id": "s-null",
            "step_id": 1,
            "created_at": null,
            "messages": [{"role": "assistant", "content": "ok"}]
        }]);
        let expected = parse_openai_msg_corpus_value(&input, "nulls.json").unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(temporary.path()).await.unwrap();
        store.replace_storylines(&expected).await.unwrap();

        let document_ids = expected
            .iter()
            .map(|story| story.document_id().to_string())
            .collect::<Vec<_>>();
        let restored = store
            .get_storylines_by_document_ids(&document_ids)
            .await
            .unwrap()
            .into_iter()
            .map(Option::unwrap)
            .collect::<Vec<_>>();

        assert_eq!(
            recover_openai_msg_files(&restored).unwrap()[0].document,
            input
        );
    }
}

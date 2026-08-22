//! Logical import/export for OpenAI-message trajectory corpora.
//!
//! The reader accepts either a top-level row array or a `session_steps`
//! envelope containing rows from many sessions. Unmapped container and row
//! fields are retained as controlled unknown fields while mapped fields use the
//! canonical Storyline representation.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use anyhow::Context as _;
use serde_json::{json, Map, Value};

use crate::format::DocumentFormat;
use crate::formats::storyline::{
    StorylineAgent, StorylineDocument, StorylineEnv, StorylineOrigin, StorylineTask,
    StorylineToolCall, StorylineTurn, STORYLINE_SCHEMA_VERSION,
};
use crate::formats::timestamp::StorylineTimestamp;
use crate::formats::unknown_fields::{
    attach_carried_unknown_fields, decode_json_pointer, normalize_openai_pointer,
    restore_json_pointer, take_unknown_fields_envelope, validate_unknown_fields_with,
    write_foreign_unknown_fields_envelope, CarrierBinding, PointerWrite, UnknownFieldLimits,
};
use crate::{InputIssue, InputResult, Result};

/// One canonical OpenAI JSON file reconstructed from Storyline semantics.
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
    let mut document = document.clone();
    let carried_envelope = take_unknown_fields_envelope(&mut document)?;
    let (records, root_unknown) = match &document {
        Value::Array(records) => (records.clone(), Map::new()),
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
            (records, metadata)
        }
        _ => {
            return Err(InputIssue::invalid(
                "OpenAI corpus must be a JSON array or session_steps object",
            ));
        }
    };

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

    let mut stories = Vec::with_capacity(groups.len());
    let mut carriers = Vec::new();
    for (session_id, mut records) in groups {
        let story_index = stories.len();
        let mut story = rows_to_storyline(&session_id, &mut records, &relative_path)?;
        capture_openai_unknowns(&mut story, &relative_path, &root_unknown, &records)?;
        story.unknown_key_counts = validate_unknown_fields_with(
            &story.unknown_fields,
            UnknownFieldLimits::default(),
            normalize_openai_pointer,
        )?;
        for (ordinal, _) in records {
            carriers.push(CarrierBinding {
                story_index,
                pointer: format!("/session_steps/{ordinal}"),
            });
        }
        stories.push(story);
    }
    attach_carried_unknown_fields(
        DocumentFormat::OpenaiMsg,
        carried_envelope,
        &carriers,
        &mut stories,
        UnknownFieldLimits::default(),
    )?;
    Ok(stories)
}

fn capture_openai_unknowns(
    story: &mut StorylineDocument,
    source_document_id: &str,
    root_unknown: &Map<String, Value>,
    records: &[(usize, Value)],
) -> InputResult<()> {
    insert_openai_map(story, source_document_id, "", root_unknown)?;
    for (ordinal, record) in records {
        let row = record.as_object().ok_or_else(|| {
            InputIssue::invalid("OpenAI corpus row must be an object")
                .at(format!("rows[{ordinal}]"))
        })?;
        let row_prefix = format!("/session_steps/{ordinal}");
        for (key, value) in row {
            let prefix = pointer_join(&row_prefix, key);
            match (key.as_str(), value) {
                ("messages", Value::Array(messages)) => {
                    for (index, message) in messages.iter().enumerate() {
                        let Some(message) = message.as_object() else {
                            if !message.is_null() {
                                story.unknown_fields.insert(
                                    "openai-msg",
                                    source_document_id,
                                    pointer_join(&prefix, &index.to_string()),
                                    message.clone(),
                                )?;
                            }
                            continue;
                        };
                        capture_openai_message(
                            story,
                            source_document_id,
                            &pointer_join(&prefix, &index.to_string()),
                            message,
                        )?;
                    }
                }
                ("response", Value::Object(response)) => {
                    capture_openai_message(story, source_document_id, &prefix, response)?;
                }
                ("meta_json", Value::Object(meta)) => {
                    capture_openai_meta(story, source_document_id, &prefix, meta)?;
                }
                _ => {
                    story.unknown_fields.insert(
                        "openai-msg",
                        source_document_id,
                        prefix,
                        value.clone(),
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn capture_openai_message(
    story: &mut StorylineDocument,
    source_document_id: &str,
    prefix: &str,
    message: &Map<String, Value>,
) -> InputResult<()> {
    for (key, value) in message {
        let field_prefix = pointer_join(prefix, key);
        if key == "tool_calls" {
            if let Some(calls) = value.as_array() {
                for (index, call) in calls.iter().enumerate() {
                    let call_prefix = pointer_join(&field_prefix, &index.to_string());
                    let Some(call) = call.as_object() else {
                        if !call.is_null() {
                            story.unknown_fields.insert(
                                "openai-msg",
                                source_document_id,
                                call_prefix,
                                call.clone(),
                            )?;
                        }
                        continue;
                    };
                    for (call_key, call_value) in call {
                        let call_field_prefix = pointer_join(&call_prefix, call_key);
                        if call_key == "function" {
                            if let Some(function) = call_value.as_object() {
                                for (function_key, function_value) in function {
                                    story.unknown_fields.insert(
                                        "openai-msg",
                                        source_document_id,
                                        pointer_join(&call_field_prefix, function_key),
                                        function_value.clone(),
                                    )?;
                                }
                                continue;
                            }
                        }
                        story.unknown_fields.insert(
                            "openai-msg",
                            source_document_id,
                            call_field_prefix,
                            call_value.clone(),
                        )?;
                    }
                }
                continue;
            }
        }
        story.unknown_fields.insert(
            "openai-msg",
            source_document_id,
            field_prefix,
            value.clone(),
        )?;
    }
    Ok(())
}

fn capture_openai_meta(
    story: &mut StorylineDocument,
    source_document_id: &str,
    prefix: &str,
    meta: &Map<String, Value>,
) -> InputResult<()> {
    for (key, value) in meta {
        let field_prefix = pointer_join(prefix, key);
        if key == "env_state" {
            if let Some(env_state) = value.as_object() {
                for (env_key, env_value) in env_state {
                    story.unknown_fields.insert(
                        "openai-msg",
                        source_document_id,
                        pointer_join(&field_prefix, env_key),
                        env_value.clone(),
                    )?;
                }
                continue;
            }
        }
        story.unknown_fields.insert(
            "openai-msg",
            source_document_id,
            field_prefix,
            value.clone(),
        )?;
    }
    Ok(())
}

fn insert_openai_map(
    story: &mut StorylineDocument,
    source_document_id: &str,
    prefix: &str,
    fields: &Map<String, Value>,
) -> InputResult<()> {
    for (key, value) in fields {
        story.unknown_fields.insert(
            "openai-msg",
            source_document_id,
            pointer_join(prefix, key),
            value.clone(),
        )?;
    }
    Ok(())
}

fn pointer_join(parent: &str, token: &str) -> String {
    format!("{parent}/{}", token.replace('~', "~0").replace('/', "~1"))
}

const OPENAI_ROW_METRIC_FIELDS: &[&str] = &[
    "reward",
    "step_reward",
    "is_terminal",
    "is_truncated",
    "is_session_completed",
    "is_trainable",
];

const OPENAI_ENV_METRIC_FIELDS: &[&str] = &[
    "prompt_tokens",
    "completion_tokens",
    "total_tokens",
    "request_bytes",
    "response_bytes",
    "output_bytes",
    "output_chunk_count",
    "finish_reason",
    "status_code",
    "retry_count",
    "upstream_latency_ms",
    "gateway_overhead_ms",
    "total_latency_ms",
    "ttft_ms",
    "truncate_reason",
    "error_type",
    "error_text",
    "client_cancelled",
    "upstream_cancelled",
    "synthetic_stop",
    "is_truncated",
    "is_session_completed",
    "max_steps",
    "is_stream",
    "payload_sampled",
    "created_at",
    "completed_at",
];

fn is_known_optional_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(values) => values.is_empty(),
        Value::Object(values) => values.is_empty(),
        _ => false,
    }
}

fn discard_known_optional_empty(object: &mut Map<String, Value>, key: &str) {
    if object.get(key).is_some_and(is_known_optional_empty) {
        object.remove(key);
    }
}

fn into_openai_object(value: Value) -> std::result::Result<Map<String, Value>, Value> {
    match value {
        Value::Object(object) => Ok(object),
        Value::String(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(object)) => Ok(object),
            _ => Err(Value::String(text)),
        },
        value => Err(value),
    }
}

fn remove_matching_string(object: &mut Map<String, Value>, key: &str, expected: &str) {
    if object.get(key).and_then(Value::as_str) == Some(expected) {
        object.remove(key);
    }
}

fn consume_openai_meta(
    row: &mut Map<String, Value>,
    session_id: &str,
    step_id: i64,
    model: Option<&str>,
    mapped_agent_id: Option<&str>,
) {
    let Some(original_meta) = row.remove("meta_json") else {
        return;
    };
    if is_known_optional_empty(&original_meta) {
        return;
    }
    let mut meta = match into_openai_object(original_meta) {
        Ok(meta) => meta,
        Err(original_meta) => {
            row.insert("meta_json".into(), original_meta);
            return;
        }
    };

    if mapped_agent_id
        .is_some_and(|agent_id| meta.get("source").and_then(Value::as_str) == Some(agent_id))
    {
        meta.remove("source");
    }
    meta.remove("group_id");

    if let Some(original_env_state) = meta.remove("env_state") {
        if is_known_optional_empty(&original_env_state) {
            // A known empty env_state is equivalent to absence.
        } else {
            match into_openai_object(original_env_state) {
                Ok(mut env_state) => {
                    remove_matching_string(&mut env_state, "session_id", session_id);
                    if let Some(model) = model {
                        remove_matching_string(&mut env_state, "requested_model", model);
                    }
                    if env_state.get("llm_step_index").and_then(Value::as_i64) == Some(step_id) {
                        env_state.remove("llm_step_index");
                    }
                    for field in OPENAI_ENV_METRIC_FIELDS {
                        env_state.remove(*field);
                    }
                    for field in [
                        "endpoint",
                        "event_type",
                        "redaction_policy",
                        "request_id",
                        "upstream_base_url",
                        "weight_version",
                    ] {
                        env_state.remove(field);
                    }
                    if !env_state.is_empty() {
                        meta.insert("env_state".into(), Value::Object(env_state));
                    }
                }
                Err(original_env_state) => {
                    meta.insert("env_state".into(), original_env_state);
                }
            }
        }
    }

    if !meta.is_empty() {
        row.insert("meta_json".into(), Value::Object(meta));
    }
}

fn consume_tool_call_residuals(value: &mut Value) -> bool {
    let Some(calls) = value.as_array_mut() else {
        return false;
    };
    if calls.is_empty() {
        return true;
    }
    for call_value in calls.iter_mut() {
        let Some(call) = call_value.as_object_mut() else {
            continue;
        };
        let valid_id = call
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty());
        let valid_name = call
            .get("function")
            .and_then(Value::as_object)
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .is_some_and(|name| !name.is_empty());
        if !valid_id || !valid_name {
            continue;
        }

        call.remove("id");
        if call.get("type").and_then(Value::as_str) == Some("function") {
            call.remove("type");
        }
        if let Some(function) = call.get_mut("function").and_then(Value::as_object_mut) {
            function.remove("name");
            function.remove("arguments");
            if function.is_empty() {
                call.remove("function");
            }
        }
        if call.is_empty() {
            *call_value = Value::Null;
        }
    }
    calls.iter().all(Value::is_null)
}

fn consume_openai_message(message: &mut Map<String, Value>, force_assistant: bool) {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .map(str::to_string);
    let linked_tool = role.as_deref() == Some("tool")
        && message
            .get("tool_call_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty());
    let mapped_role = matches!(role.as_deref(), Some("system" | "user" | "assistant"))
        || linked_tool
        || force_assistant;
    let assistant = role.as_deref() == Some("assistant") || force_assistant;
    let refusal_is_output = assistant
        && !message.get("content").is_some_and(content_has_value)
        && message.get("refusal").is_some_and(content_has_value);
    if mapped_role {
        if matches!(role.as_deref(), Some("system" | "user" | "assistant")) || linked_tool {
            message.remove("role");
        }
        message.remove("content");
    }
    if linked_tool {
        message.remove("tool_call_id");
    }
    if assistant
        && message
            .get("reasoning_content")
            .is_some_and(|value| value.is_string() || value.is_null())
    {
        message.remove("reasoning_content");
    }
    if refusal_is_output {
        message.remove("refusal");
    }
    if assistant {
        if let Some(tool_calls) = message.get_mut("tool_calls") {
            if consume_tool_call_residuals(tool_calls) {
                message.remove("tool_calls");
            }
        }
    }
    for key in ["name", "refusal", "tool_call_id", "tool_calls"] {
        discard_known_optional_empty(message, key);
    }
}

fn consume_openai_messages(row: &mut Map<String, Value>) {
    if let Some(messages) = row.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages.iter_mut() {
            if let Some(message) = message.as_object_mut() {
                consume_openai_message(message, false);
            }
        }
        if messages
            .iter()
            .all(|message| message.as_object().is_some_and(Map::is_empty))
        {
            row.remove("messages");
        }
    }

    match row.get_mut("response") {
        Some(Value::Object(response)) => {
            consume_openai_message(response, true);
            if response.is_empty() {
                row.remove("response");
            }
        }
        Some(value) if is_known_optional_empty(value) => {
            row.remove("response");
        }
        _ => {}
    }
}

fn consume_openai_row(
    row: &mut Map<String, Value>,
    session_id: &str,
    step_id: i64,
    model: Option<&str>,
    run_id: Option<&str>,
    mapped_agent_id: Option<&str>,
) {
    consume_openai_meta(row, session_id, step_id, model, mapped_agent_id);

    row.remove("session_id");
    row.remove("step_id");
    row.remove("created_at");
    for key in ["env_name", "dataset_type", "dt", "id"] {
        if row
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
        {
            row.remove(key);
        }
    }
    for field in OPENAI_ROW_METRIC_FIELDS {
        row.remove(*field);
    }
    if let Some(model) = model {
        for field in ["agent_model", "llm_model"] {
            remove_matching_string(row, field, model);
        }
    }
    if let Some(run_id) = run_id {
        for field in ["run_id", "run_bucket", "job_id"] {
            remove_matching_string(row, field, run_id);
        }
    }
    if let Some(agent_id) = mapped_agent_id {
        remove_matching_string(row, "agent_id", agent_id);
    }
    remove_matching_string(row, "env_id", session_id);
    for key in [
        "blob_manifest",
        "chosen_response",
        "rejected_response",
        "ground_truth_answer",
        "reference_answer",
    ] {
        discard_known_optional_empty(row, key);
    }
    consume_openai_messages(row);
}

/// Rebuild canonical OpenAI files from Storylines produced by the corpus reader.
///
/// Storylines are grouped by their OpenAI source path. Unmapped source fields
/// are restored where their recorded JSON pointers do not collide with mapped
/// canonical values.
pub fn recover_openai_msg_files(
    stories: &[StorylineDocument],
) -> Result<Vec<RecoveredOpenaiMsgFile>> {
    let mut groups = HashMap::<String, Vec<StorylineDocument>>::new();
    for story in stories {
        let source_id = story
            .unknown_fields
            .sources
            .get("openai-msg")
            .map(|source| source.source_document_id.as_str())
            .or_else(|| {
                story
                    .origin
                    .as_ref()
                    .filter(|origin| origin.format == DocumentFormat::OpenaiMsg.as_str())
                    .and_then(|origin| origin.document_id.as_deref())
            })
            .ok_or_else(|| {
                anyhow::anyhow!("cannot mix OpenAI unknown fields and unrelated Storylines")
            })?;
        groups
            .entry(source_id.to_string())
            .or_default()
            .push(story.clone());
    }
    let mut output = groups
        .into_iter()
        .map(|(relative_path, group)| {
            Ok(RecoveredOpenaiMsgFile {
                relative_path: validate_relative_path(Path::new(&relative_path))?,
                document: storylines_to_openai_value(&group)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    output.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(output)
}

pub(crate) fn has_openai_provenance(story: &StorylineDocument) -> bool {
    story.unknown_fields.sources.contains_key("openai-msg")
        || story.origin.as_ref().is_some_and(|origin| {
            origin.format == DocumentFormat::OpenaiMsg.as_str() && origin.document_id.is_some()
        })
}

fn encode_agent_message(turn: &StorylineTurn) -> Result<Value> {
    let mut message = json!({
        "role": "assistant",
        "content": turn.message,
    });
    if let Some(reasoning) = &turn.reasoning_content {
        message["reasoning_content"] = Value::String(reasoning.clone());
    }
    if let Some(calls) = &turn.tool_calls {
        message["tool_calls"] = encode_tool_calls(calls)?;
    }
    Ok(message)
}

fn encode_context_turns(turns: &[StorylineTurn]) -> Result<Vec<Value>> {
    let mut messages = Vec::new();
    for turn in turns {
        let role = match turn.source.as_str() {
            "system" => "system",
            "user" => "user",
            "agent" => "assistant",
            source => anyhow::bail!(
                "OpenAI context cannot represent Storyline turn {} source '{source}'",
                turn.id
            ),
        };
        let mut message = json!({"role": role, "content": turn.message});
        if role == "assistant" {
            if let Some(reasoning) = &turn.reasoning_content {
                message["reasoning_content"] = Value::String(reasoning.clone());
            }
            if let Some(calls) = &turn.tool_calls {
                message["tool_calls"] = encode_tool_calls(calls)?;
            }
        }
        messages.push(message);
        messages.extend(encode_tool_results(turn.observation.as_ref()));
    }
    Ok(messages)
}

fn storyline_interactions(
    turns: &[StorylineTurn],
) -> Result<Vec<(Option<&StorylineTurn>, &StorylineTurn)>> {
    let mut interactions = Vec::new();
    let mut index = 0;
    while index < turns.len() {
        let turn = &turns[index];
        match turn.source.as_str() {
            "user" => {
                let agent = turns
                    .get(index + 1)
                    .filter(|turn| turn.source == "agent")
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "OpenAI synthesis requires an agent response after user turn {}",
                            turn.id
                        )
                    })?;
                interactions.push((Some(turn), agent));
                index += 2;
            }
            "agent" => {
                interactions.push((None, turn));
                index += 1;
            }
            source => anyhow::bail!(
                "OpenAI synthesis cannot represent Storyline turn {} source '{source}'",
                turn.id
            ),
        }
    }
    Ok(interactions)
}

fn encoded_openai_step_id(
    story: &StorylineDocument,
    context_count: i64,
    interaction_index: usize,
    user: Option<&StorylineTurn>,
    agent: &StorylineTurn,
) -> Result<i64> {
    if !has_openai_provenance(story) {
        return i64::try_from(interaction_index + 1)
            .context("OpenAI interaction index overflows step_id");
    }
    let delta = agent
        .id
        .checked_sub(context_count)
        .ok_or_else(|| anyhow::anyhow!("OpenAI agent turn id precedes copied context"))?;
    if delta <= 0 || delta % 2 != 0 {
        anyhow::bail!(
            "OpenAI agent turn {} does not encode a source step_id",
            agent.id
        );
    }
    let step_id = delta / 2;
    let expected_user_id = context_count
        .checked_add(step_id.checked_mul(2).context("OpenAI step_id overflow")?)
        .and_then(|value| value.checked_sub(1))
        .context("OpenAI step_id overflow")?;
    if user.is_some_and(|turn| turn.id != expected_user_id) {
        anyhow::bail!(
            "OpenAI user turn id does not match agent turn {} source step_id",
            agent.id
        );
    }
    Ok(step_id)
}

fn populate_openai_row_fields(
    row: &mut Map<String, Value>,
    story: &StorylineDocument,
    agent: &StorylineTurn,
) {
    row.insert("agent_id".into(), Value::String(story.agent.id.clone()));
    if let Some(run_id) = &story.run_id {
        row.insert("job_id".into(), Value::String(run_id.clone()));
    }
    if let Some(model) = agent
        .model_name
        .as_ref()
        .or(story.agent.model_name.as_ref())
    {
        row.insert("agent_model".into(), Value::String(model.clone()));
    }
    if let Some(timestamp) = &agent.timestamp {
        row.insert("created_at".into(), timestamp.source_value().clone());
    }

    if let Some(metrics) = agent.metrics.as_ref().and_then(Value::as_object) {
        for field in OPENAI_ROW_METRIC_FIELDS {
            if let Some(value) = metrics.get(*field) {
                row.insert((*field).to_string(), value.clone());
            }
        }
        let mut env_state = Map::new();
        for field in OPENAI_ENV_METRIC_FIELDS {
            if OPENAI_ROW_METRIC_FIELDS.contains(field) && row.contains_key(*field) {
                continue;
            }
            if let Some(value) = metrics.get(*field) {
                env_state.insert((*field).to_string(), value.clone());
            }
        }
        if !metrics.contains_key("total_latency_ms") {
            if let Some(latency_ms) = agent.latency_ms {
                env_state.insert("total_latency_ms".into(), json!(latency_ms));
            }
        }
        if !metrics.contains_key("ttft_ms") {
            if let Some(ttft_ms) = agent.ttft_ms {
                env_state.insert("ttft_ms".into(), json!(ttft_ms));
            }
        }
        if !env_state.is_empty() {
            row.insert(
                "meta_json".into(),
                json!({"env_state": Value::Object(env_state)}),
            );
        }
    }
    write_openai_env_fields(row, story, agent);
}

fn write_openai_env_fields(
    row: &mut Map<String, Value>,
    story: &StorylineDocument,
    agent: &StorylineTurn,
) {
    let merged = match (
        story.task.as_ref().and_then(|task| task.env.as_ref()),
        agent.env.as_ref(),
    ) {
        (Some(base), Some(overlay)) => Some(base.merge_overlay(overlay)),
        (Some(base), None) => Some(base.clone()),
        (None, Some(overlay)) => Some(overlay.clone()),
        (None, None) => None,
    };
    let Some(env) = merged else {
        return;
    };
    if let Some(name) = &env.name {
        row.insert("env_name".into(), Value::String(name.clone()));
    }
    if let Some(id) = &env.id {
        row.insert("id".into(), Value::String(id.clone()));
    }
    if let Some(state) = &env.state {
        if let Some(dataset_type) = state.get("dataset_type") {
            row.insert("dataset_type".into(), dataset_type.clone());
        }
        if let Some(dt) = state.get("dt") {
            row.insert("dt".into(), dt.clone());
        }
    }
    let mut meta = row
        .remove("meta_json")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    if let Some(group_id) = env.state.as_ref().and_then(|state| state.get("group_id")) {
        meta.insert("group_id".into(), group_id.clone());
    }
    let mut env_state = meta
        .remove("env_state")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    if let Some(endpoint) = &env.endpoint {
        env_state.insert("endpoint".into(), Value::String(endpoint.clone()));
    }
    if let Some(event_type) = &env.event_type {
        env_state.insert("event_type".into(), Value::String(event_type.clone()));
    }
    if let Some(request_id) = &env.request_id {
        env_state.insert("request_id".into(), Value::String(request_id.clone()));
    }
    if let Some(state) = &env.state {
        for key in ["redaction_policy", "upstream_base_url", "weight_version"] {
            if let Some(value) = state.get(key) {
                env_state.insert(key.to_string(), value.clone());
            }
        }
    }
    if !env_state.is_empty() {
        meta.insert("env_state".into(), Value::Object(env_state));
    }
    if !meta.is_empty() {
        row.insert("meta_json".into(), Value::Object(meta));
    }
}

pub(crate) fn synthesize_openai_msg_corpus_value(stories: &[StorylineDocument]) -> Result<Value> {
    let mut records = Vec::new();
    for story in stories {
        if story.session_id.is_empty() || story.agent.id.is_empty() {
            anyhow::bail!("invalid Storyline identity for OpenAI conversion");
        }
        let context_len = story
            .turns
            .iter()
            .take_while(|turn| turn.is_copied_context == Some(true))
            .count();
        let context_count = i64::try_from(context_len)
            .context("OpenAI copied context exceeds Storyline turn id")?;
        let mut history = encode_context_turns(&story.turns[..context_len])?;
        let interactions = storyline_interactions(&story.turns[context_len..])?;
        for (interaction_index, (user, agent)) in interactions.into_iter().enumerate() {
            let step_id =
                encoded_openai_step_id(story, context_count, interaction_index, user, agent)?;
            let mut messages = history.clone();
            let user_message = user.map(|user| json!({"role": "user", "content": user.message}));
            if let Some(user_message) = &user_message {
                messages.push(user_message.clone());
            }
            let tool_results = encode_tool_results(agent.observation.as_ref());
            messages.extend(tool_results.iter().cloned());
            let response = encode_agent_message(agent)?;
            let mut row = json!({
                "session_id": story.session_id,
                "step_id": step_id,
                "messages": messages,
                "response": response,
            });
            populate_openai_row_fields(
                row.as_object_mut()
                    .ok_or_else(|| anyhow::anyhow!("OpenAI row must be an object"))?,
                story,
                agent,
            );
            records.push(row);

            if let Some(user_message) = user_message {
                history.push(user_message);
            }
            history.extend(tool_results);
            history.push(response);
        }
    }
    Ok(json!({"session_steps": records}))
}

pub(crate) fn storylines_to_openai_value(stories: &[StorylineDocument]) -> Result<Value> {
    let mut value = synthesize_openai_msg_corpus_value(stories)?;
    let rows = value["session_steps"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("canonical OpenAI envelope lost session_steps"))?;
    let story_by_session = stories
        .iter()
        .enumerate()
        .map(|(index, story)| (story.session_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut carriers = Vec::new();
    for (target_index, row) in rows.iter().enumerate() {
        let session = row["session_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("canonical OpenAI row missing session_id"))?;
        let story_index = *story_by_session.get(session).ok_or_else(|| {
            anyhow::anyhow!("canonical OpenAI row has unknown session '{session}'")
        })?;
        carriers.push(CarrierBinding {
            story_index,
            pointer: format!("/session_steps/{target_index}"),
        });
    }
    let mut carriers_by_story = vec![Vec::new(); stories.len()];
    for carrier in &carriers {
        carriers_by_story[carrier.story_index].push(carrier.pointer.clone());
    }

    let mut merged = std::collections::BTreeMap::<String, Value>::new();
    let mut source_id = None::<String>;
    for (story_index, story) in stories.iter().enumerate() {
        let Some(source) = story.unknown_fields.sources.get("openai-msg") else {
            continue;
        };
        if source_id
            .as_ref()
            .is_some_and(|id| id != &source.source_document_id)
        {
            anyhow::bail!("one OpenAI output cannot merge multiple source documents");
        }
        source_id = Some(source.source_document_id.clone());
        for (pointer, field_value) in &source.fields {
            let pointer =
                relocate_openai_unknown_pointer(&value, pointer, &carriers_by_story[story_index])?;
            match merged.get(&pointer) {
                Some(existing) if existing != field_value => {
                    anyhow::bail!("OpenAI unknown-field conflict at '{pointer}'")
                }
                Some(_) => {}
                None => {
                    merged.insert(pointer, field_value.clone());
                }
            }
        }
    }
    for (pointer, field_value) in merged {
        ensure_openai_unknown_parents(&mut value, &pointer)
            .with_context(|| format!("prepare OpenAI unknown field '{pointer}'"))?;
        restore_json_pointer(&mut value, &pointer, field_value, PointerWrite::InsertOnly)
            .with_context(|| format!("restore OpenAI unknown field '{pointer}'"))?;
    }
    write_foreign_unknown_fields_envelope(
        DocumentFormat::OpenaiMsg,
        &mut value,
        stories,
        &carriers,
    )?;
    Ok(value)
}

fn relocate_openai_unknown_pointer(
    target: &Value,
    pointer: &str,
    story_carriers: &[String],
) -> Result<String> {
    let tokens = decode_json_pointer(pointer)?;
    if tokens.len() < 2 || tokens[0] != "session_steps" {
        return Ok(pointer.to_string());
    }
    let source_index = tokens[1]
        .parse::<usize>()
        .with_context(|| format!("OpenAI unknown pointer '{pointer}' has an invalid row index"))?;
    let source_row_pointer = format!("/session_steps/{source_index}");
    if target.pointer(&source_row_pointer).is_some() {
        return Ok(pointer.to_string());
    }

    if story_carriers.len() != 1 {
        return Ok(pointer.to_string());
    }
    let mut relocated = story_carriers[0].clone();
    for token in &tokens[2..] {
        relocated = pointer_join(&relocated, token);
    }
    Ok(relocated)
}

fn ensure_openai_unknown_parents(target: &mut Value, pointer: &str) -> Result<()> {
    let tokens = decode_json_pointer(pointer)?;
    let Some((_, parents)) = tokens.split_last() else {
        anyhow::bail!("cannot restore an OpenAI unknown field at the document root");
    };
    let mut current = target;
    for (index, token) in parents.iter().enumerate() {
        let next_is_index = parents
            .get(index + 1)
            .is_some_and(|next| next.parse::<usize>().is_ok());
        current = match current {
            Value::Object(object) => object.entry(token.clone()).or_insert_with(|| {
                if next_is_index {
                    Value::Array(Vec::new())
                } else {
                    Value::Object(Map::new())
                }
            }),
            Value::Array(array) => {
                let array_index = token.parse::<usize>().with_context(|| {
                    format!("OpenAI unknown pointer '{pointer}' has a non-numeric array index")
                })?;
                while array.len() <= array_index {
                    array.push(Value::Null);
                }
                let slot = &mut array[array_index];
                if slot.is_null() {
                    *slot = if next_is_index {
                        Value::Array(Vec::new())
                    } else {
                        Value::Object(Map::new())
                    };
                }
                slot
            }
            _ => anyhow::bail!(
                "OpenAI unknown pointer '{pointer}' has a non-container canonical parent"
            ),
        };
    }
    Ok(())
}

fn openai_context_turn(id: i64, message: &Map<String, Value>) -> Option<StorylineTurn> {
    let source = match message.get("role").and_then(Value::as_str)? {
        "system" => "system",
        "user" => "user",
        "assistant" => "agent",
        _ => return None,
    };
    let tool_calls = (source == "agent")
        .then(|| parse_tool_calls(message.get("tool_calls")))
        .flatten();
    Some(StorylineTurn {
        id,
        kind: Some(if tool_calls.is_some() {
            "autonomous".into()
        } else if source == "agent" {
            "llm.response".into()
        } else if source == "user" {
            "llm.request".into()
        } else {
            "context".into()
        }),
        timestamp: None,
        source: source.into(),
        message: message.get("content").cloned().unwrap_or(Value::Null),
        reasoning_content: None,
        reasoning_effort: None,
        tool_calls,
        observation: None,
        metrics: None,
        model_name: None,
        llm_call_count: None,
        is_copied_context: Some(true),
        latency_ms: None,
        ttft_ms: None,
        extra: None,
        env: None,
        prompt: None,
        finished_at: None,
    })
}

fn openai_turn_ids(context_count: i64, step_id: i64) -> InputResult<(i64, i64)> {
    if step_id <= 0 {
        return Err(InputIssue::invalid(
            "OpenAI corpus row requires positive integer step_id",
        ));
    }
    let agent_id = step_id
        .checked_mul(2)
        .and_then(|value| context_count.checked_add(value))
        .ok_or_else(|| InputIssue::invalid("OpenAI corpus step_id overflows Storyline turn id"))?;
    let user_id = agent_id
        .checked_sub(1)
        .ok_or_else(|| InputIssue::invalid("OpenAI corpus step_id overflows Storyline turn id"))?;
    Ok((user_id, agent_id))
}

fn openai_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn assign_stable_string(
    slot: &mut Option<String>,
    incoming: Option<String>,
    overlay: &mut Option<String>,
) {
    let Some(incoming) = incoming else {
        return;
    };
    match slot {
        None => *slot = Some(incoming),
        Some(existing) if existing == &incoming => {}
        Some(_) => *overlay = Some(incoming),
    }
}

fn assign_stable_state(
    task_state: &mut serde_json::Map<String, Value>,
    key: &str,
    incoming: Option<Value>,
    overlay: &mut serde_json::Map<String, Value>,
) {
    let Some(incoming) = incoming else {
        return;
    };
    if incoming.is_null() {
        return;
    }
    match task_state.get(key) {
        None => {
            task_state.insert(key.to_string(), incoming);
        }
        Some(existing) if existing == &incoming => {}
        Some(_) => {
            overlay.insert(key.to_string(), incoming);
        }
    }
}

fn openai_turn_env(
    row: &Map<String, Value>,
    env_state: Option<&Value>,
    task_env: &mut StorylineEnv,
    task_state: &mut serde_json::Map<String, Value>,
) -> Option<StorylineEnv> {
    let env_state = env_state.and_then(Value::as_object);
    let mut overlay = StorylineEnv::default();
    let mut overlay_state = serde_json::Map::new();
    assign_stable_string(
        &mut task_env.name,
        openai_string(row.get("env_name")),
        &mut overlay.name,
    );
    assign_stable_string(
        &mut task_env.endpoint,
        openai_string(env_state.and_then(|state| state.get("endpoint"))),
        &mut overlay.endpoint,
    );
    assign_stable_state(
        task_state,
        "dataset_type",
        row.get("dataset_type").cloned(),
        &mut overlay_state,
    );
    assign_stable_state(task_state, "dt", row.get("dt").cloned(), &mut overlay_state);
    let group_id = parsed_meta(row)
        .as_ref()
        .and_then(|meta| meta.get("group_id"))
        .cloned();
    assign_stable_state(task_state, "group_id", group_id, &mut overlay_state);
    for key in ["redaction_policy", "upstream_base_url", "weight_version"] {
        assign_stable_state(
            task_state,
            key,
            env_state.and_then(|state| state.get(key)).cloned(),
            &mut overlay_state,
        );
    }
    overlay.id = openai_string(row.get("id"));
    overlay.event_type = openai_string(env_state.and_then(|state| state.get("event_type")));
    overlay.request_id = openai_string(env_state.and_then(|state| state.get("request_id")));
    overlay.state = (!overlay_state.is_empty()).then_some(overlay_state);
    (!overlay.is_empty()).then_some(overlay)
}

fn rows_to_storyline(
    session_id: &str,
    records: &mut [(usize, Value)],
    relative_path: &str,
) -> InputResult<StorylineDocument> {
    records.sort_by_key(|(_, row)| row.get("step_id").and_then(Value::as_i64));
    let mut seen_steps = HashSet::new();
    let mut turns = Vec::with_capacity(records.len().saturating_mul(2));
    let mut agent_source = None;
    let mut first_agent_id = None;
    let mut first_model: Option<String> = None;
    let mut run_id: Option<String> = None;
    let mut context_count = 0_i64;
    let mut task_env = StorylineEnv::default();
    let mut task_state = serde_json::Map::new();

    for (record_index, (ordinal, raw)) in records.iter_mut().enumerate() {
        let row = raw.as_object_mut().ok_or_else(|| {
            InputIssue::invalid("OpenAI corpus row must be an object")
                .at(format!("rows[{ordinal}]"))
        })?;
        let step_id = row
            .get("step_id")
            .and_then(Value::as_i64)
            .filter(|step_id| *step_id > 0)
            .ok_or_else(|| {
                InputIssue::invalid("OpenAI corpus row requires positive integer step_id")
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
        if first_agent_id.is_none() {
            first_agent_id = row
                .get("agent_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
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

        let output = select_output_message(row).ok_or_else(|| {
            InputIssue::invalid("OpenAI corpus row has no assistant output")
                .at(format!("rows[{ordinal}]"))
        })?;
        let tool_calls = parse_tool_calls(output.get("tool_calls"))
            .or_else(|| parse_embedded_tool_call(output.get("content"), step_id));
        let message = output
            .get("content")
            .filter(|value| content_has_value(value))
            .or_else(|| {
                output
                    .get("refusal")
                    .filter(|value| content_has_value(value))
            })
            .cloned()
            .unwrap_or_else(|| output.get("content").cloned().unwrap_or(Value::Null));
        let reasoning_content = output
            .get("reasoning_content")
            .and_then(Value::as_str)
            .map(str::to_string);
        let metrics = normalized_metrics(row, env_state.as_ref());
        let timestamp = row
            .get("created_at")
            .filter(|value| !value.is_null())
            .cloned()
            .map(StorylineTimestamp::from_json)
            .transpose()
            .map_err(|issue| issue.at(format!("rows[{ordinal}].created_at")))?;
        let latency_ms = env_state
            .as_ref()
            .and_then(|state| state.get("total_latency_ms"))
            .and_then(number_to_i64);
        let ttft_ms = env_state
            .as_ref()
            .and_then(|state| state.get("ttft_ms"))
            .and_then(number_to_i64);

        let request_messages = row.get("messages").cloned();
        let user_message = last_user_message(request_messages.as_ref());
        if record_index == 0 {
            let context_end = user_message.as_ref().map(|(index, _)| *index).unwrap_or(0);
            if let Some(messages) = request_messages.as_ref().and_then(Value::as_array) {
                for message in messages.iter().take(context_end) {
                    let Some(message) = message.as_object() else {
                        continue;
                    };
                    let turn_id = i64::try_from(turns.len())
                        .ok()
                        .and_then(|value| value.checked_add(1))
                        .ok_or_else(|| {
                            InputIssue::invalid("OpenAI context size overflows Storyline turn id")
                        })?;
                    if let Some(turn) = openai_context_turn(turn_id, message) {
                        turns.push(turn);
                    }
                }
            }
            context_count = i64::try_from(turns.len()).map_err(|_| {
                InputIssue::invalid("OpenAI context size overflows Storyline turn id")
            })?;
        }
        let observation = distribute_tool_results(request_messages.as_ref(), &mut turns);
        let (user_turn_id, agent_turn_id) = openai_turn_ids(context_count, step_id)
            .map_err(|issue| issue.at(format!("rows[{ordinal}].step_id")))?;
        if let Some((_, message)) = user_message.as_ref() {
            turns.push(StorylineTurn {
                id: user_turn_id,
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
                extra: None,
                env: None,
                prompt: None,
                finished_at: None,
            });
        }

        turns.push(StorylineTurn {
            id: agent_turn_id,
            kind: Some(if tool_calls.is_some() {
                "autonomous".into()
            } else {
                "llm.response".into()
            }),
            timestamp,
            source: "agent".into(),
            message,
            reasoning_content,
            reasoning_effort: None,
            tool_calls,
            observation,
            metrics,
            model_name: model.clone(),
            llm_call_count: Some(1),
            is_copied_context: None,
            latency_ms,
            ttft_ms,
            extra: None,
            env: None,
            prompt: None,
            finished_at: None,
        });

        let turn_env = openai_turn_env(row, env_state.as_ref(), &mut task_env, &mut task_state);
        if let Some(turn) = turns.last_mut() {
            turn.env = turn_env;
        }

        let mapped_agent_id = first_agent_id
            .as_deref()
            .or(agent_source.as_deref())
            .or(first_model.as_deref());
        consume_openai_row(
            row,
            session_id,
            step_id,
            model.as_deref(),
            run_id.as_deref(),
            mapped_agent_id,
        );
    }

    task_env.state = (!task_state.is_empty()).then_some(task_state);
    let task = StorylineTask {
        env: (!task_env.is_empty()).then_some(task_env),
        llm: None,
        result: None,
    };

    let final_metrics = turns.last().and_then(|turn| turn.metrics.clone());
    let agent_id = first_agent_id
        .or(agent_source)
        .or_else(|| first_model.clone())
        .unwrap_or_else(|| "openai-import".into());
    Ok(StorylineDocument {
        schema_version: STORYLINE_SCHEMA_VERSION.into(),
        origin: Some(StorylineOrigin {
            format: DocumentFormat::OpenaiMsg.as_str().into(),
            schema_version: None,
            document_id: Some(relative_path.into()),
        }),
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
        extra: None,
        meta: None,
        task: (!task.is_empty()).then_some(task),
        prompt: None,
        started_at: None,
        finished_at: None,
        unknown_fields: Default::default(),
        unknown_key_counts: Default::default(),
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

fn distribute_tool_results(
    messages: Option<&Value>,
    previous_turns: &mut [StorylineTurn],
) -> Option<Value> {
    let mut current_results = Vec::new();
    for message in messages?
        .as_array()?
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
    {
        let Some(source_call_id) = message.get("tool_call_id").and_then(Value::as_str) else {
            continue;
        };
        if source_call_id.is_empty() {
            continue;
        }
        let result = json!({
            "source_call_id": source_call_id,
            "content": message.get("content").cloned().unwrap_or(Value::Null),
        });
        let previous = previous_turns.iter_mut().rev().find(|turn| {
            turn.tool_calls
                .as_deref()
                .is_some_and(|calls| calls.iter().any(|call| call.tool_call_id == source_call_id))
        });
        if let Some(previous) = previous {
            append_tool_result(&mut previous.observation, result);
        } else if !current_results.contains(&result) {
            current_results.push(result);
        }
    }
    (!current_results.is_empty()).then(|| json!({"results": current_results}))
}

fn append_tool_result(observation: &mut Option<Value>, result: Value) {
    let results = observation
        .get_or_insert_with(|| json!({"results": []}))
        .get_mut("results")
        .and_then(Value::as_array_mut);
    if let Some(results) = results {
        if !results.contains(&result) {
            results.push(result);
        }
    }
}

fn encode_tool_results(observation: Option<&Value>) -> Vec<Value> {
    observation
        .and_then(|value| value.get("results"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|result| {
            let source_call_id = result.get("source_call_id")?.as_str()?;
            if source_call_id.is_empty() {
                return None;
            }
            Some(json!({
                "role": "tool",
                "tool_call_id": source_call_id,
                "content": result.get("content").cloned().unwrap_or(Value::Null),
            }))
        })
        .collect()
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

fn select_output_message(row: &Map<String, Value>) -> Option<&Map<String, Value>> {
    let response = row.get("response").and_then(Value::as_object);
    if response.is_some_and(message_has_output) {
        return response;
    }
    row.get("messages")?
        .as_array()?
        .iter()
        .rev()
        .filter_map(Value::as_object)
        .find(|message| {
            message.get("role").and_then(Value::as_str) == Some("assistant")
                && message_has_output(message)
        })
}

fn message_has_output(message: &Map<String, Value>) -> bool {
    let has_tools = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty());
    let has_refusal = message.get("refusal").is_some_and(content_has_value);
    has_tools || has_refusal || message.get("content").is_some_and(content_has_value)
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
            Some(StorylineToolCall {
                tool_call_id,
                function_name,
                arguments,
                result: Default::default(),
                duration_ms: None,
                extra: None,
                kind: None,
                response: None,
            })
        })
        .collect::<Vec<_>>();
    (!parsed.is_empty()).then_some(parsed)
}

fn encode_tool_calls(calls: &[StorylineToolCall]) -> Result<Value> {
    calls
        .iter()
        .map(|call| {
            let arguments = Value::String(serde_json::to_string(&call.arguments)?);
            let mut encoded = json!({
                "id": call.tool_call_id,
                "function": {
                    "name": call.function_name,
                    "arguments": arguments,
                }
            });
            encoded["type"] = Value::String("function".into());
            Ok(encoded)
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
        extra: None,
        kind: None,
        response: None,
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
    let mut metrics = Map::new();
    for field in OPENAI_ROW_METRIC_FIELDS {
        if let Some(value) = row.get(*field) {
            metrics.insert((*field).to_string(), value.clone());
        }
    }
    if let Some(env_state) = env_state.and_then(Value::as_object) {
        for field in OPENAI_ENV_METRIC_FIELDS {
            if metrics.contains_key(*field) {
                continue;
            }
            if let Some(value) = env_state.get(*field) {
                metrics.insert((*field).to_string(), value.clone());
            }
        }
    }
    (!metrics.is_empty()).then_some(Value::Object(metrics))
}

fn number_to_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value as i64))
}

#[cfg(test)]
#[path = "openai_corpus/tests.rs"]
mod tests;

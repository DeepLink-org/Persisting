//! Lossless JSON-model import/export for OpenAI-message trajectory corpora.
//!
//! The reader accepts either a top-level row array or a `session_steps`
//! envelope containing rows from many sessions. Unmapped container and row
//! fields are retained as controlled unknown fields so strict recovery can rebuild
//! the original source document.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use anyhow::Context as _;
use chrono::{SecondsFormat, TimeZone, Utc};
use serde_json::{json, Map, Value};

use crate::format::DocumentFormat;
use crate::formats::storyline::{
    StorylineAgent, StorylineDocument, StorylineToolCall, StorylineTurn,
};
use crate::formats::unknown_fields::{
    attach_carried_unknown_fields, normalize_openai_pointer, restore_json_pointer,
    take_unknown_fields_envelope, validate_unknown_fields_with,
    write_foreign_unknown_fields_envelope, CarrierBinding, PointerWrite, SourceUnknownFields,
    UnknownFieldLimits,
};
use crate::{InputIssue, InputResult, Result};

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
    for (session_id, records) in groups {
        let story_index = stories.len();
        let mut story = rows_to_storyline(&session_id, records.clone(), &relative_path)?;
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
    let owned_counts = stories
        .iter()
        .map(|story| story.unknown_key_counts.get("openai-msg").cloned())
        .collect::<Vec<_>>();
    attach_carried_unknown_fields(
        carried_envelope,
        &carriers,
        &mut stories,
        UnknownFieldLimits::default(),
    )?;
    for (story, owned) in stories.iter_mut().zip(owned_counts) {
        if let Some(owned) = owned {
            story.unknown_key_counts.insert("openai-msg".into(), owned);
        }
    }
    Ok(stories)
}

fn capture_openai_unknowns(
    story: &mut StorylineDocument,
    source_document_id: &str,
    root_unknown: &Map<String, Value>,
    records: &[(usize, Value)],
) -> InputResult<()> {
    story
        .unknown_fields
        .sources
        .entry("openai-msg".into())
        .or_insert_with(|| SourceUnknownFields {
            source_document_id: source_document_id.to_string(),
            fields: Default::default(),
        });
    insert_openai_map(story, source_document_id, "", root_unknown)?;
    for (ordinal, record) in records {
        let row = record.as_object().expect("rows were validated as objects");
        let row_prefix = format!("/session_steps/{ordinal}");
        for (key, value) in row {
            if !is_canonical_openai_row_key(key) {
                story.unknown_fields.insert(
                    "openai-msg",
                    source_document_id,
                    pointer_join(&row_prefix, key),
                    value.clone(),
                )?;
            }
        }
        if let Some(messages) = row.get("messages").and_then(Value::as_array) {
            let output_index = match select_output_message(row).map(|(_, location)| location) {
                Some(OutputLocation::Message(index)) => Some(index),
                _ => None,
            };
            let mut request_index = 0usize;
            for (index, message) in messages.iter().enumerate() {
                let Some(message) = message.as_object() else {
                    continue;
                };
                let prefix = if output_index == Some(index) {
                    pointer_join(&row_prefix, "response")
                } else {
                    let prefix = pointer_join(
                        &pointer_join(&row_prefix, "messages"),
                        &request_index.to_string(),
                    );
                    request_index += 1;
                    prefix
                };
                capture_openai_message(story, source_document_id, &prefix, message)?;
            }
        }
        if let Some(response) = row.get("response").and_then(Value::as_object) {
            capture_openai_message(
                story,
                source_document_id,
                &pointer_join(&row_prefix, "response"),
                response,
            )?;
        }
    }
    Ok(())
}

fn is_canonical_openai_row_key(key: &str) -> bool {
    matches!(
        key,
        "session_id"
            | "step_id"
            | "id"
            | "messages"
            | "response"
            | "agent_model"
            | "llm_model"
            | "run_id"
            | "run_bucket"
            | "job_id"
            | "created_at"
            | "meta"
            | "env_state"
            | "metrics"
            | "call_id"
            | "agent_id"
            | "group_id"
            | "env_name"
    ) || ROW_METRIC_FIELDS.contains(&key)
}

fn capture_openai_message(
    story: &mut StorylineDocument,
    source_document_id: &str,
    prefix: &str,
    message: &Map<String, Value>,
) -> InputResult<()> {
    for (key, value) in message {
        if !matches!(
            key.as_str(),
            "role" | "content" | "name" | "tool_call_id" | "tool_calls"
        ) {
            story.unknown_fields.insert(
                "openai-msg",
                source_document_id,
                pointer_join(prefix, key),
                value.clone(),
            )?;
        }
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (index, call) in calls.iter().enumerate() {
            let Some(call) = call.as_object() else {
                continue;
            };
            let call_prefix = pointer_join(&pointer_join(prefix, "tool_calls"), &index.to_string());
            for (key, value) in call {
                if !matches!(key.as_str(), "id" | "type" | "function") {
                    story.unknown_fields.insert(
                        "openai-msg",
                        source_document_id,
                        pointer_join(&call_prefix, key),
                        value.clone(),
                    )?;
                }
            }
            if let Some(function) = call.get("function").and_then(Value::as_object) {
                for (key, value) in function {
                    if !matches!(key.as_str(), "name" | "arguments") {
                        story.unknown_fields.insert(
                            "openai-msg",
                            source_document_id,
                            pointer_join(&pointer_join(&call_prefix, "function"), key),
                            value.clone(),
                        )?;
                    }
                }
            }
        }
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

/// Recover original OpenAI files from Storylines produced by the corpus reader.
///
/// This is intentionally strict: Storylines without complete lossless metadata
/// are rejected instead of being silently synthesized from normalized fields.
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
                    .extra
                    .as_ref()
                    .and_then(|extra| extra.get("openai_source_document_id"))
                    .and_then(Value::as_str)
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
        || story
            .extra
            .as_ref()
            .and_then(|extra| extra.get("openai_source_document_id"))
            .is_some()
}

/// Explicitly synthesize an OpenAI message row array from Storyline semantics.
///
/// This is a cross-format projection, not a lossless recovery operation. Use
/// [`recover_openai_msg_files`] when the Storylines originated from an OpenAI
/// corpus and exact JSON-model recovery is required.
pub(crate) fn synthesize_openai_msg_corpus(stories: &[StorylineDocument]) -> Result<Value> {
    let mut records = Vec::<(Option<u64>, usize, usize, Value)>::new();
    let mut sequence = 0usize;
    for (story_index, story) in stories.iter().enumerate() {
        if story.session_id.is_empty() || story.agent.id.is_empty() {
            anyhow::bail!("invalid Storyline identity for OpenAI conversion");
        }
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
            if let Some(user) = user {
                if let Some(message) = messages
                    .iter_mut()
                    .rev()
                    .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
                {
                    if let Some(message) = message.as_object_mut() {
                        message.insert("content".into(), user.message.clone());
                    }
                }
            }
            if messages.is_empty() {
                if let Some(user) = user {
                    messages.push(json!({"role": "user", "content": user.message}));
                }
            }
            if let Some(agent) = agent {
                messages.extend(encode_tool_results(agent.observation.as_ref()));
            }
            let response = agent
                .map(|turn| {
                    let mut response = json!({
                    "role": "assistant",
                    "content": crate::convert::message_text(&turn.message)
                        .map(Value::String)
                        .unwrap_or_else(|| turn.message.clone()),
                    });
                    if let Some(calls) = &turn.tool_calls {
                        response["tool_calls"] = encode_tool_calls(calls)?;
                    }
                    Ok::<_, anyhow::Error>(response)
                })
                .transpose()?;
            let call_id = agent
                .and_then(|turn| turn.extra.as_ref())
                .and_then(|extra| extra.get("call_id"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let ordinal = agent
                .and_then(|turn| turn.extra.as_ref())
                .and_then(|extra| extra.get("openai_source_ordinal"))
                .and_then(Value::as_u64);
            let step_id = agent
                .and_then(|turn| turn.extra.as_ref())
                .and_then(|extra| extra.get("openai_step_id"))
                .and_then(Value::as_i64)
                .unwrap_or(output.id);
            records.push((
                ordinal,
                sequence,
                story_index,
                json!({
                "id": call_id,
                    "session_id": story.session_id,
                    "step_id": step_id,
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
                    "created_at": output.timestamp.clone(),
                    "messages": messages,
                    "response": response,
                    "run_bucket": story.run_id.clone().unwrap_or_default(),
                    "call_id": call_id,
                }),
            ));
            sequence += 1;
        }
    }
    records.sort_by_key(|(ordinal, sequence, _, _)| {
        (ordinal.is_none(), ordinal.unwrap_or(u64::MAX), *sequence)
    });
    Ok(json!({
        "session_steps": records.into_iter().map(|(_, _, _, row)| row).collect::<Vec<_>>()
    }))
}

pub(crate) fn storylines_to_openai_value(stories: &[StorylineDocument]) -> Result<Value> {
    let mut value = synthesize_openai_msg_corpus(stories)?;
    let rows = value["session_steps"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("canonical OpenAI envelope lost session_steps"))?;
    let story_by_session = stories
        .iter()
        .enumerate()
        .map(|(index, story)| (story.session_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut hint_by_row = HashMap::<(String, i64), (usize, usize)>::new();
    for (story_index, story) in stories.iter().enumerate() {
        for turn in story.turns.iter().filter(|turn| turn.source == "agent") {
            let Some(extra) = turn.extra.as_ref() else {
                continue;
            };
            let Some(ordinal) = extra.get("openai_source_ordinal").and_then(Value::as_u64) else {
                continue;
            };
            let step_id = extra
                .get("openai_step_id")
                .and_then(Value::as_i64)
                .unwrap_or(turn.id);
            let ordinal = usize::try_from(ordinal).context("OpenAI source ordinal overflow")?;
            hint_by_row.insert((story.session_id.clone(), step_id), (ordinal, story_index));
        }
    }
    let mut ordinal_to_target = HashMap::<usize, usize>::new();
    let mut carriers = Vec::new();
    for (target_index, row) in rows.iter().enumerate() {
        let session = row["session_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("canonical OpenAI row missing session_id"))?;
        let step_id = row["step_id"]
            .as_i64()
            .ok_or_else(|| anyhow::anyhow!("canonical OpenAI row missing step_id"))?;
        let story_index = *story_by_session.get(session).ok_or_else(|| {
            anyhow::anyhow!("canonical OpenAI row has unknown session '{session}'")
        })?;
        if let Some((ordinal, hinted_story)) = hint_by_row.get(&(session.to_string(), step_id)) {
            if *hinted_story != story_index
                || ordinal_to_target.insert(*ordinal, target_index).is_some()
            {
                anyhow::bail!("duplicate OpenAI source row carrier for ordinal {ordinal}");
            }
        }
        carriers.push(CarrierBinding {
            story_index,
            pointer: format!("/session_steps/{target_index}"),
        });
    }

    let mut merged = std::collections::BTreeMap::<String, Value>::new();
    let mut source_id = None::<String>;
    for story in stories {
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
            let target_pointer = remap_openai_pointer(pointer, &ordinal_to_target)?;
            match merged.get(&target_pointer) {
                Some(existing) if existing != field_value => {
                    anyhow::bail!("OpenAI unknown-field conflict at '{target_pointer}'")
                }
                Some(_) => {}
                None => {
                    merged.insert(target_pointer, field_value.clone());
                }
            }
        }
    }
    for (pointer, field_value) in merged {
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

fn remap_openai_pointer(
    pointer: &str,
    ordinal_to_target: &HashMap<usize, usize>,
) -> Result<String> {
    if !pointer.starts_with("/session_steps/") {
        return Ok(pointer.to_string());
    }
    let suffix = &pointer["/session_steps/".len()..];
    let (ordinal, rest) = suffix.split_once('/').unwrap_or((suffix, ""));
    let ordinal = ordinal
        .parse::<usize>()
        .with_context(|| format!("invalid OpenAI source ordinal in '{pointer}'"))?;
    let target = ordinal_to_target.get(&ordinal).ok_or_else(|| {
        anyhow::anyhow!("OpenAI unknown field references filtered or missing source row {ordinal}")
    })?;
    Ok(if rest.is_empty() {
        format!("/session_steps/{target}")
    } else {
        format!("/session_steps/{target}/{rest}")
    })
}

fn rows_to_storyline(
    session_id: &str,
    mut records: Vec<(usize, Value)>,
    relative_path: &str,
) -> InputResult<StorylineDocument> {
    records.sort_by_key(|(_, row)| row.get("step_id").and_then(Value::as_i64));
    let mut seen_steps = HashSet::new();
    let mut turns = Vec::with_capacity(records.len().saturating_mul(2));
    let mut agent_source = None;
    let mut first_agent_id = None;
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
            .map(str::to_string)
            .unwrap_or_else(|| format!("step-{step_id}"));
        let request_messages = row.get("messages").cloned();
        let user_message = last_user_message(request_messages.as_ref());
        let observation = parse_tool_results(request_messages.as_ref());
        let request_messages = request_message_context(
            request_messages,
            user_message.as_ref().map(|(index, _)| *index),
            output_location,
        );
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
                extra: Some(json!({"call_id": call_id})),
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
            observation,
            metrics,
            model_name: model,
            llm_call_count: Some(1),
            is_copied_context: None,
            latency_ms,
            ttft_ms,
            extra: Some(json!({
                "call_id": call_id,
                "openai_source_ordinal": ordinal,
                "openai_step_id": step_id,
                "request_messages": request_messages,
            })),
        });
        next_turn_id += 1;
    }

    let final_metrics = turns.last().and_then(|turn| turn.metrics.clone());
    let agent_id = first_agent_id
        .or(agent_source)
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
        extra: Some(json!({"openai_source_document_id": relative_path})),
        unknown_fields: Default::default(),
        unknown_key_counts: Default::default(),
        turns,
    })
}

fn request_message_context(
    mut messages: Option<Value>,
    user_message_index: Option<usize>,
    output_location: OutputLocation,
) -> Option<Value> {
    let values = messages.as_mut()?.as_array_mut()?;
    if let Some(index) = user_message_index {
        if let Some(message) = values.get_mut(index).and_then(Value::as_object_mut) {
            message.remove("content");
        }
    }
    if let OutputLocation::Message(index) = output_location {
        if index < values.len() {
            values.remove(index);
        }
    }
    values.retain(|message| message.get("role").and_then(Value::as_str) != Some("tool"));
    for message in values.iter_mut().filter_map(Value::as_object_mut) {
        retain_canonical_openai_message(message);
    }
    messages
}

fn retain_canonical_openai_message(message: &mut Map<String, Value>) {
    message.retain(|key, _| {
        matches!(
            key.as_str(),
            "role" | "content" | "name" | "tool_call_id" | "tool_calls"
        )
    });
    let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) else {
        return;
    };
    for call in calls.iter_mut().filter_map(Value::as_object_mut) {
        call.retain(|key, _| matches!(key.as_str(), "id" | "type" | "function"));
        if let Some(function) = call.get_mut("function").and_then(Value::as_object_mut) {
            function.retain(|key, _| matches!(key.as_str(), "name" | "arguments"));
        }
    }
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

fn parse_tool_results(messages: Option<&Value>) -> Option<Value> {
    let results = messages?
        .as_array()?
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        .filter_map(|message| {
            let source_call_id = message.get("tool_call_id")?.as_str()?;
            if source_call_id.is_empty() {
                return None;
            }
            Some(json!({
                "source_call_id": source_call_id,
                "content": message.get("content").cloned().unwrap_or(Value::Null),
            }))
        })
        .collect::<Vec<_>>();
    (!results.is_empty()).then(|| json!({"results": results}))
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
            Some(StorylineToolCall {
                tool_call_id,
                function_name,
                arguments,
                result: Default::default(),
                duration_ms: None,
                extra: None,
            })
        })
        .collect::<Vec<_>>();
    (!parsed.is_empty()).then_some(parsed)
}

fn encode_tool_calls(calls: &[StorylineToolCall]) -> Result<Value> {
    calls
        .iter()
        .map(|call| {
            Ok(json!({
                "id": call.tool_call_id,
                "type": "function",
                "function": {
                    "name": call.function_name,
                    "arguments": serde_json::to_string(&call.arguments)?,
                }
            }))
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

    #[test]
    fn openai_unknown_fields_use_exact_row_paths() {
        let input = json!({"root_vendor": 1, "session_steps": [{
            "session_id": "s", "step_id": 1,
            "messages": [{
                "role": "user", "content": "hi", "message_vendor": null, "0": true
            }],
            "response": {"role": "assistant", "content": "ok"},
            "row_vendor": [3, 2, 1]
        }]});
        let stories = parse_openai_msg_corpus_value(&input, "corpus.json").unwrap();
        let fields = &stories[0].unknown_fields.sources["openai-msg"].fields;
        assert_eq!(fields["/root_vendor"], 1);
        assert_eq!(fields["/session_steps/0/row_vendor"], json!([3, 2, 1]));
        assert_eq!(
            fields["/session_steps/0/messages/0/message_vendor"],
            Value::Null
        );
        assert_eq!(
            stories[0].unknown_key_counts["openai-msg"]["/session_steps/*/messages/*/0"],
            1
        );
    }
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
    fn corpus_roundtrip_emits_canonical_envelope_in_source_order() {
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
        let rows = recovered[0].document["session_steps"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["session_id"], "s-1");
        assert_eq!(rows[0]["step_id"], 2);
        assert_eq!(rows[1]["session_id"], "s-2");
        assert_eq!(rows[2]["step_id"], 1);
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
    fn openai_unknown_fields_preserve_values_but_storyline_content_is_authoritative() {
        let input = corpus();
        let mut stories = parse_openai_msg_corpus_value(&input, "corpus.json").unwrap();
        assert!(!serde_json::to_string(&stories)
            .unwrap()
            .contains(&["_pchron", "icle_"].concat()));

        stories[0].turns[0].message = json!("edited user");
        stories[0].turns[1].message = json!("edited assistant");
        let recovered = recover_openai_msg_files(&stories).unwrap();
        let rows = recovered[0].document["session_steps"].as_array().unwrap();
        let first_session_row = rows.iter().find(|row| row["id"] == "evt-1").unwrap();
        assert_eq!(first_session_row["messages"][1]["content"], "edited user");
        assert_eq!(first_session_row["response"]["content"], "edited assistant");
        assert_eq!(rows[0]["unknown"], Value::Null);
    }

    #[test]
    fn message_unknowns_and_tool_results_restore_once() {
        let input = json!({"session_steps": [{
            "id": "",
            "session_id": "s",
            "agent_id": "agent",
            "step_id": 1,
            "messages": [
                {"role": "user", "content": "run", "vendor_message": 7},
                {"role": "tool", "tool_call_id": "call-1", "content": "ok"}
            ],
            "response": {
                "role": "assistant",
                "content": "done",
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "inspect", "arguments": "{}"}
                }]
            }
        }]});

        let stories = parse_openai_msg_corpus_value(&input, "tool-results.json").unwrap();
        assert_eq!(
            stories[0].turns[1].observation.as_ref().unwrap()["results"][0],
            json!({"source_call_id": "call-1", "content": "ok"})
        );
        let recovered = recover_openai_msg_files(&stories).unwrap();
        let row = &recovered[0].document["session_steps"][0];
        assert_eq!(row["agent_id"], "agent");
        assert_eq!(row["id"], "");
        assert_eq!(row["messages"][0]["vendor_message"], 7);
        assert_eq!(
            row["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|message| message["role"] == "tool")
                .count(),
            1
        );
        assert_ne!(row["created_at"], "");
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
        assert_eq!(recovered[0].document["custom"], Value::Null);
        assert_eq!(recovered[0].document["session_id"], "s-1");
        assert!(recovered[0].document["session_steps"].is_array());
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
        let recovered = recover_openai_msg_files(&stories).unwrap();
        let row = &recovered[0].document["session_steps"][0];
        assert_eq!(row["session_id"], "child-session");
        assert_eq!(row["step_id"], 7);
        assert_eq!(row["messages"][0]["content"], "question");
        assert_eq!(row["response"]["content"], "answer");
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
        let canonical = recover_openai_msg_files(&expected).unwrap()[0]
            .document
            .clone();
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
        assert_eq!(recovered[0].document, canonical);
    }

    #[cfg(feature = "lance-store")]
    #[tokio::test]
    async fn fractional_created_at_is_lossless_through_lance() {
        let mut input = corpus();
        input[0]["created_at"] = json!(1_700_000_001.123_456_f64);
        let expected = parse_openai_msg_corpus_value(&input, "fractional.json").unwrap();
        let canonical = recover_openai_msg_files(&expected).unwrap()[0]
            .document
            .clone();
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
            canonical
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
        let canonical = recover_openai_msg_files(&expected).unwrap()[0]
            .document
            .clone();
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
            canonical
        );
    }
}

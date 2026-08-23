use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use serde_json::{json, Map, Value};

use super::codec::{
    emit_stories, DecodeContext, DecodeReport, FormatCapabilities, ProbeConfidence,
    TrajectoryFormat,
};
use super::common::jsonl::{
    filename_stem, for_each_jsonl_object, join_text_parts, leftover_textless_parts,
    parse_json_value,
};
use crate::format::DocumentFormat;
use crate::formats::storyline::{
    StoryLink, StorylineDocument, StorylineEnv, StorylineOrigin, StorylinePrompt, StorylineTask,
    StorylineTaskResult, StorylineToolCall, StorylineTurn,
};
use crate::formats::timestamp::StorylineTimestamp;
use crate::{InputIssue, InputResult};

pub struct CodexFormat;

const SOURCE: &str = "codex";

impl TrajectoryFormat for CodexFormat {
    fn id(&self) -> DocumentFormat {
        DocumentFormat::Codex
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["jsonl", "json", "ndjson"]
    }

    fn capabilities(&self) -> FormatCapabilities {
        FormatCapabilities {
            decode: true,
            encode: false,
            direct_query: true,
            streaming_input: true,
        }
    }

    fn probe(&self, path: Option<&Path>, content: &[u8]) -> InputResult<ProbeConfidence> {
        Ok(if content_has_codex_fingerprint(content) {
            ProbeConfidence::ContentFingerprint
        } else if path_has_codex_hint(path) {
            ProbeConfidence::PathHint
        } else {
            ProbeConfidence::None
        })
    }

    fn decode(
        &self,
        reader: &mut dyn BufRead,
        ctx: &DecodeContext<'_>,
        emit: &mut dyn FnMut(StorylineDocument) -> InputResult<()>,
    ) -> InputResult<DecodeReport> {
        emit_stories(decode_from_reader(reader, &ctx.source.relative_path)?, emit)
    }
}

pub(crate) fn looks_like_codex_event(value: &Value) -> bool {
    let event_type = value.get("type").and_then(Value::as_str);
    value.get("timestamp").is_some()
        && value.get("payload").is_some()
        && matches!(
            event_type,
            Some("session_meta" | "response_item" | "event_msg")
        )
}

fn path_has_codex_hint(path: Option<&Path>) -> bool {
    let Some(path) = path else {
        return false;
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.starts_with("rollout-") && name.ends_with(".jsonl")
}

fn content_has_codex_fingerprint(content: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(content) else {
        return false;
    };
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            let candidate = value
                .as_array()
                .and_then(|values| values.first())
                .unwrap_or(&value);
            if looks_like_codex_event(candidate) {
                return true;
            }
        }
        for line in trimmed
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(32)
        {
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                if looks_like_codex_event(&value) {
                    return true;
                }
            }
        }
    }
    false
}

fn decode_from_reader<R: BufRead>(
    reader: R,
    relative_path: &str,
) -> InputResult<Vec<StorylineDocument>> {
    let source_id = relative_path.replace('\\', "/");

    let mut session_id = filename_stem(&source_id);
    let mut agent_id = "codex".to_string();
    let mut agent_version = None;
    let mut cwd = None;
    let mut parent_id = None;
    let mut model = None;
    let mut prompt_system = String::new();
    let mut started_at = None;
    let mut finished_at = None;
    let mut task_complete = false;
    let mut turns: Vec<StorylineTurn> = Vec::new();
    let mut pending_agent: Option<usize> = None;
    let mut pending_turn_key: Option<String> = None;
    let mut call_index: HashMap<String, (usize, usize)> = HashMap::new();
    let mut unknown = Map::new();
    let mut saw_fingerprint = false;

    let object_count = for_each_jsonl_object(reader, |line_number, value| {
        saw_fingerprint |= looks_like_codex_event(&value);
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|raw| StorylineTimestamp::from_rfc3339(raw).ok());
        if started_at.is_none() {
            started_at = timestamp.clone();
        }
        if let Some(timestamp) = timestamp.clone() {
            finished_at = Some(timestamp);
        }

        let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = value.get("payload").cloned().unwrap_or(Value::Null);
        match event_type {
            "session_meta" => {
                if let Some(id) = payload.get("id").and_then(Value::as_str) {
                    session_id = id.to_string();
                }
                if let Some(value) = payload.get("cwd").and_then(Value::as_str) {
                    cwd = Some(value.to_string());
                }
                if let Some(value) = payload.get("cli_version").and_then(Value::as_str) {
                    agent_version = Some(value.to_string());
                }
                if let Some(value) = payload
                    .get("parent_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    parent_id = Some(value.to_string());
                }
                if payload
                    .get("source")
                    .and_then(|source| source.get("subagent"))
                    .is_some()
                {
                    agent_id = "codex-subagent".into();
                }
            }
            "turn_context" => {
                if let Some(next_model) = payload.get("model").and_then(Value::as_str) {
                    model = Some(next_model.to_string());
                }
            }
            "response_item" => apply_response_item(
                &payload,
                timestamp,
                model.as_deref(),
                &mut turns,
                &mut pending_agent,
                &mut pending_turn_key,
                &mut call_index,
                &mut prompt_system,
                &mut unknown,
                line_number,
            ),
            "event_msg" => apply_event_msg(
                &payload,
                &mut turns,
                pending_agent,
                &mut task_complete,
                &mut unknown,
                line_number,
            ),
            _ => {
                unknown.insert(format!("/events/{line_number}"), value);
            }
        }
        Ok(())
    })?;
    if object_count == 0 {
        return empty_session(&source_id, &filename_stem(&source_id));
    }
    if !saw_fingerprint {
        return Err(InputIssue::invalid(
            "codex JSONL has no session/transcript fingerprint",
        ));
    }

    let mut story = StorylineDocument::new(session_id, agent_id);
    story.origin = Some(StorylineOrigin {
        format: SOURCE.into(),
        schema_version: None,
        document_id: Some(source_id.clone()),
    });
    story.agent.version = agent_version;
    story.agent.model_name = model;
    if let Some(parent_session_id) = parent_id {
        story.parent = Some(StoryLink {
            parent_session_id,
            spawn_call_id: None,
            spawn_id: None,
            relation: "spawn".into(),
        });
    }
    let task_env = cwd.map(|name| StorylineEnv {
        name: Some(name),
        ..StorylineEnv::default()
    });
    let task_result = task_complete.then_some(StorylineTaskResult {
        status: Some("complete".into()),
        ..StorylineTaskResult::default()
    });
    if task_env.is_some() || task_result.is_some() {
        story.task = Some(StorylineTask {
            env: task_env,
            llm: None,
            result: task_result,
        });
    }
    story.prompt = StorylinePrompt::from_pair(&prompt_system, "");
    story.started_at = started_at;
    story.finished_at = finished_at;
    story.turns = turns;
    for (pointer, value) in unknown {
        story
            .unknown_fields
            .insert(SOURCE, source_id.as_str(), pointer, value)?;
    }
    story.refresh_unknown_key_counts()?;
    story.validate()?;
    Ok(vec![story])
}

fn empty_session(source_id: &str, session_id: &str) -> InputResult<Vec<StorylineDocument>> {
    let mut story = StorylineDocument::new(session_id, "codex");
    story.origin = Some(StorylineOrigin {
        format: SOURCE.into(),
        schema_version: None,
        document_id: Some(source_id.to_string()),
    });
    story.validate()?;
    Ok(vec![story])
}

fn apply_response_item(
    payload: &Value,
    timestamp: Option<StorylineTimestamp>,
    model: Option<&str>,
    turns: &mut Vec<StorylineTurn>,
    pending_agent: &mut Option<usize>,
    pending_turn_key: &mut Option<String>,
    call_index: &mut HashMap<String, (usize, usize)>,
    prompt_system: &mut String,
    unknown: &mut Map<String, Value>,
    line_number: usize,
) {
    let item_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
    let turn_key = payload
        .get("internal_chat_message_metadata_passthrough")
        .and_then(|meta| meta.get("turn_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    match item_type {
        "message" => match payload.get("role").and_then(Value::as_str) {
            Some("user") => {
                flush_agent(pending_agent, pending_turn_key);
                turns.push(user_turn(
                    next_turn_id(turns),
                    timestamp,
                    content_text(payload).unwrap_or_default(),
                ));
                record_textless_content(payload, line_number, unknown);
            }
            Some("assistant") => {
                let index = ensure_agent_turn(
                    turns,
                    pending_agent,
                    pending_turn_key,
                    turn_key,
                    timestamp,
                    model,
                );
                append_agent_text(&mut turns[index], content_text(payload));
                record_textless_content(payload, line_number, unknown);
            }
            Some("developer") => {
                if let Some(text) = content_text(payload) {
                    if !prompt_system.is_empty() {
                        prompt_system.push('\n');
                    }
                    prompt_system.push_str(&text);
                }
            }
            _ => {
                unknown.insert(format!("/events/{line_number}"), payload.clone());
            }
        },
        "agent_message" => {
            let index = ensure_agent_turn(
                turns,
                pending_agent,
                pending_turn_key,
                turn_key,
                timestamp,
                model,
            );
            append_agent_text(
                &mut turns[index],
                payload
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| content_text(payload)),
            );
        }
        "reasoning" => {
            let summary = payload
                .get("summary")
                .and_then(join_text_parts)
                .or_else(|| {
                    payload
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
            if summary.is_none() && payload.get("encrypted_content").is_some() {
                unknown.insert(
                    format!("/events/{line_number}"),
                    json!({ "type": "reasoning", "encrypted": true }),
                );
            } else if let Some(summary) = summary {
                let index = ensure_agent_turn(
                    turns,
                    pending_agent,
                    pending_turn_key,
                    turn_key,
                    timestamp,
                    model,
                );
                let turn = &mut turns[index];
                turn.reasoning_content = Some(match turn.reasoning_content.take() {
                    Some(existing) => format!("{existing}\n{summary}"),
                    None => summary,
                });
            }
        }
        "function_call" | "custom_tool_call" | "tool_search_call" => {
            let index = ensure_agent_turn(
                turns,
                pending_agent,
                pending_turn_key,
                turn_key,
                timestamp,
                model,
            );
            let call = tool_call_from_payload(payload, item_type);
            let tool_idx = {
                let tools = turns[index].tool_calls.get_or_insert_with(Vec::new);
                tools.push(call);
                tools.len() - 1
            };
            if let Some(call_id) = payload.get("call_id").and_then(Value::as_str) {
                call_index.insert(call_id.to_string(), (index, tool_idx));
            }
        }
        "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
            attach_tool_output(
                payload,
                turns,
                call_index,
                pending_agent,
                pending_turn_key,
                timestamp,
                model,
            );
        }
        _ => {
            unknown.insert(format!("/events/{line_number}"), payload.clone());
        }
    }
}

fn apply_event_msg(
    payload: &Value,
    turns: &mut [StorylineTurn],
    pending_agent: Option<usize>,
    task_complete: &mut bool,
    unknown: &mut Map<String, Value>,
    line_number: usize,
) {
    match payload.get("type").and_then(Value::as_str).unwrap_or("") {
        "token_count" => {
            if let Some(index) = pending_agent {
                turns[index].metrics = Some(payload.clone());
            }
        }
        "task_complete" => *task_complete = true,
        "user_message" | "agent_message" => {}
        _ => {
            unknown.insert(format!("/events/{line_number}"), payload.clone());
        }
    }
}

fn tool_call_from_payload(payload: &Value, item_type: &str) -> StorylineToolCall {
    let call_id = payload
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match item_type {
        "custom_tool_call" => StorylineToolCall {
            tool_call_id: call_id,
            function_name: payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("custom_tool")
                .to_string(),
            arguments: json!({ "input": payload.get("input").cloned().unwrap_or(Value::Null) }),
            result: None,
            duration_ms: None,
            extra: None,
            kind: Some("custom".into()),
            response: None,
        },
        "tool_search_call" => StorylineToolCall {
            tool_call_id: call_id,
            function_name: "tool_search".into(),
            arguments: payload.clone(),
            result: None,
            duration_ms: None,
            extra: None,
            kind: None,
            response: None,
        },
        _ => StorylineToolCall {
            tool_call_id: call_id,
            function_name: payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("function")
                .to_string(),
            arguments: payload
                .get("arguments")
                .and_then(Value::as_str)
                .map(parse_json_value)
                .or_else(|| payload.get("arguments").cloned())
                .unwrap_or(Value::Null),
            result: None,
            duration_ms: None,
            extra: None,
            kind: None,
            response: None,
        },
    }
}

fn attach_tool_output(
    payload: &Value,
    turns: &mut Vec<StorylineTurn>,
    call_index: &mut HashMap<String, (usize, usize)>,
    pending_agent: &mut Option<usize>,
    pending_turn_key: &mut Option<String>,
    timestamp: Option<StorylineTimestamp>,
    model: Option<&str>,
) {
    let output = payload.get("output").cloned().unwrap_or(Value::Null);
    if let Some(call_id) = payload.get("call_id").and_then(Value::as_str) {
        if let Some(&(turn_idx, tool_idx)) = call_index.get(call_id) {
            if let Some(call) = turns
                .get_mut(turn_idx)
                .and_then(|turn| turn.tool_calls.as_mut())
                .and_then(|calls| calls.get_mut(tool_idx))
            {
                call.result = Some(output);
                return;
            }
        }
    }
    let index = ensure_agent_turn(
        turns,
        pending_agent,
        pending_turn_key,
        None,
        timestamp,
        model,
    );
    turns[index].observation = Some(json!({ "results": [output] }));
}

fn ensure_agent_turn(
    turns: &mut Vec<StorylineTurn>,
    pending_agent: &mut Option<usize>,
    pending_turn_key: &mut Option<String>,
    turn_key: Option<String>,
    timestamp: Option<StorylineTimestamp>,
    model: Option<&str>,
) -> usize {
    match (*pending_agent, pending_turn_key.as_ref(), turn_key.as_ref()) {
        (Some(index), Some(current), Some(next)) if current == next => return index,
        (Some(index), _, None) => return index,
        (Some(index), None, _) => return index,
        _ => {}
    }
    let index = turns.len();
    turns.push(agent_turn(next_turn_id(turns), timestamp, model));
    *pending_agent = Some(index);
    *pending_turn_key = turn_key;
    index
}

fn flush_agent(pending_agent: &mut Option<usize>, pending_turn_key: &mut Option<String>) {
    *pending_agent = None;
    *pending_turn_key = None;
}

fn next_turn_id(turns: &[StorylineTurn]) -> i64 {
    i64::try_from(turns.len() + 1).unwrap_or(i64::MAX)
}

fn user_turn(id: i64, timestamp: Option<StorylineTimestamp>, text: String) -> StorylineTurn {
    StorylineTurn {
        id,
        kind: None,
        timestamp,
        source: "user".into(),
        message: Value::String(text),
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
    }
}

fn agent_turn(
    id: i64,
    timestamp: Option<StorylineTimestamp>,
    model: Option<&str>,
) -> StorylineTurn {
    StorylineTurn {
        id,
        kind: None,
        timestamp,
        source: "agent".into(),
        message: Value::String(String::new()),
        reasoning_content: None,
        reasoning_effort: None,
        tool_calls: None,
        observation: None,
        metrics: None,
        model_name: model.map(str::to_string),
        llm_call_count: Some(1),
        is_copied_context: None,
        latency_ms: None,
        ttft_ms: None,
        extra: None,
        env: None,
        prompt: None,
        finished_at: None,
    }
}

fn content_text(payload: &Value) -> Option<String> {
    payload
        .get("content")
        .and_then(join_text_parts)
        .or_else(|| {
            payload
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn record_textless_content(payload: &Value, line_number: usize, unknown: &mut Map<String, Value>) {
    if let Some(content) = payload.get("content") {
        for (index, part) in leftover_textless_parts(content) {
            unknown.insert(format!("/events/{line_number}/content/{index}"), part);
        }
    }
}

fn append_agent_text(turn: &mut StorylineTurn, text: Option<String>) {
    let Some(text) = text.filter(|value| !value.is_empty()) else {
        return;
    };
    match &mut turn.message {
        Value::String(existing) if existing.is_empty() => *existing = text,
        Value::String(existing) => {
            existing.push('\n');
            existing.push_str(&text);
        }
        other => *other = Value::String(text),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::formats::codec::{decode_all, DocumentSource};
    use serde_json::json;

    fn codex_to_storylines(
        input: &str,
        relative_path: &str,
    ) -> InputResult<Vec<StorylineDocument>> {
        decode_all(
            &CodexFormat,
            &mut Cursor::new(input.as_bytes()),
            &DocumentSource::new(relative_path),
        )
    }

    const FIXTURE: &str = r#"{"timestamp":"2026-08-03T08:15:11.000Z","type":"session_meta","payload":{"id":"sess-codex","cwd":"/tmp/demo","cli_version":"0.40.0"}}
{"timestamp":"2026-08-03T08:15:12.000Z","type":"turn_context","payload":{"model":"gpt-5","turn_id":"t1"}}
{"timestamp":"2026-08-03T08:15:13.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"list files"}],"internal_chat_message_metadata_passthrough":{"turn_id":"t1"}}}
{"timestamp":"2026-08-03T08:15:14.000Z","type":"response_item","payload":{"type":"reasoning","summary":[{"text":"need ls"}],"internal_chat_message_metadata_passthrough":{"turn_id":"t1"}}}
{"timestamp":"2026-08-03T08:15:15.000Z","type":"response_item","payload":{"type":"function_call","name":"exec","arguments":"{\"cmd\":\"ls\"}","call_id":"c1","internal_chat_message_metadata_passthrough":{"turn_id":"t1"}}}
{"timestamp":"2026-08-03T08:15:16.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"a.rs"}}
{"timestamp":"2026-08-03T08:15:17.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"***","call_id":"c2","internal_chat_message_metadata_passthrough":{"turn_id":"t1"}}}
{"timestamp":"2026-08-03T08:15:18.000Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"c2","output":"ok"}}
{"timestamp":"2026-08-03T08:15:19.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}],"internal_chat_message_metadata_passthrough":{"turn_id":"t1"}}}
{"timestamp":"2026-08-03T08:15:20.000Z","type":"event_msg","payload":{"type":"token_count","input_tokens":10,"output_tokens":4}}
{"timestamp":"2026-08-03T08:15:21.000Z","type":"event_msg","payload":{"type":"task_complete"}}
{"timestamp":"2026-08-03T08:15:22.000Z","type":"world_state","payload":{"agents_md":{"text":"secret"}}}
"#;

    #[test]
    fn maps_codex_session_into_storyline_turns_and_tools() {
        let stories = codex_to_storylines(FIXTURE, "rollout-sess-codex.jsonl").unwrap();
        assert_eq!(stories.len(), 1);
        let story = &stories[0];
        assert_eq!(story.session_id, "sess-codex");
        assert_eq!(story.origin.as_ref().unwrap().format, "codex");
        assert_eq!(
            story
                .task
                .as_ref()
                .unwrap()
                .env
                .as_ref()
                .unwrap()
                .name
                .as_deref(),
            Some("/tmp/demo")
        );
        assert_eq!(story.turns.len(), 2);
        assert_eq!(story.turns[0].source, "user");
        assert_eq!(story.turns[0].message, json!("list files"));
        assert_eq!(story.turns[1].source, "agent");
        assert_eq!(story.turns[1].message, json!("done"));
        assert_eq!(story.turns[1].reasoning_content.as_deref(), Some("need ls"));
        let tools = story.turns[1].tool_calls.as_ref().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].tool_call_id, "c1");
        assert_eq!(tools[0].function_name, "exec");
        assert_eq!(tools[0].arguments, json!({"cmd":"ls"}));
        assert_eq!(tools[0].result, Some(json!("a.rs")));
        assert_eq!(tools[1].function_name, "apply_patch");
        assert_eq!(tools[1].kind.as_deref(), Some("custom"));
        assert_eq!(tools[1].result, Some(json!("ok")));
        assert_eq!(story.turns[1].model_name.as_deref(), Some("gpt-5"));
        assert!(story.turns[1].metrics.is_some());
        assert_eq!(
            story
                .task
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap()
                .status
                .as_deref(),
            Some("complete")
        );
        assert!(story.unknown_fields.sources["codex"]
            .fields
            .contains_key("/events/12"));
    }

    #[test]
    fn empty_jsonl_is_a_zero_turn_storyline() {
        let stories = codex_to_storylines("", "empty.jsonl").unwrap();
        assert_eq!(stories[0].turns.len(), 0);
        assert_eq!(stories[0].session_id, "empty");
    }

    #[test]
    fn bad_line_reports_line_number() {
        let error = codex_to_storylines("{not-json}\n", "bad.jsonl").unwrap_err();
        assert_eq!(error.location().as_deref(), Some("line 1"));
    }

    #[test]
    fn rejects_jsonl_without_codex_fingerprint() {
        let error = codex_to_storylines("{\"foo\":1}\n", "other.jsonl").unwrap_err();
        assert!(error.to_string().contains("fingerprint"), "{error}");
        let error = codex_to_storylines("1\n", "scalar.jsonl").unwrap_err();
        assert!(error.to_string().contains("object"), "{error}");
        let claude = r#"{"type":"user","sessionId":"s","uuid":"u","message":{"role":"user","content":"hi"}}
"#;
        let error = codex_to_storylines(claude, "rollout-mismatch.jsonl").unwrap_err();
        assert!(error.to_string().contains("fingerprint"), "{error}");
    }

    #[test]
    fn preserves_unknown_payload_and_non_text_content() {
        let input = r#"{"timestamp":"2026-08-03T08:15:11.000Z","type":"session_meta","payload":{"id":"sess-img"}}
{"timestamp":"2026-08-03T08:15:12.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"see"},{"type":"input_image","image_url":"https://example.test/a.png"}]}}
{"timestamp":"2026-08-03T08:15:13.000Z","type":"world_state","payload":{"agents_md":{"text":"secret"}}}
"#;
        let story = &codex_to_storylines(input, "rollout-img.jsonl").unwrap()[0];
        assert_eq!(story.turns[0].message, json!("see"));
        let fields = &story.unknown_fields.sources["codex"].fields;
        assert_eq!(
            fields["/events/2/content/1"],
            json!({"type":"input_image","image_url":"https://example.test/a.png"})
        );
        assert_eq!(
            fields["/events/3"],
            json!({
                "timestamp":"2026-08-03T08:15:13.000Z",
                "type":"world_state",
                "payload":{"agents_md":{"text":"secret"}}
            })
        );
    }

    #[test]
    fn streams_large_jsonl_without_collecting_all_values() {
        let mut input = String::from(
            r#"{"timestamp":"2026-08-03T08:15:11.000Z","type":"session_meta","payload":{"id":"sess-stream"}}
"#,
        );
        for index in 0..20_000 {
            input.push_str(&format!(
                r#"{{"timestamp":"2026-08-03T08:15:12.000Z","type":"event_msg","payload":{{"type":"token_count","input_tokens":{index},"output_tokens":0}}}}
"#
            ));
        }
        let stories = codex_to_storylines(&input, "rollout-stream.jsonl").unwrap();
        assert_eq!(stories[0].session_id, "sess-stream");
        assert_eq!(stories[0].turns.len(), 0);
    }
}

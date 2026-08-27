use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::codec::{
    DecodeContext, DecodeReport, FormatCapabilities, ProbeConfidence, TrajectoryFormat,
    emit_stories,
};
use super::common::jsonl::{filename_stem, for_each_jsonl_object};
use crate::format::DocumentFormat;
use crate::formats::storyline::{
    StoryLink, StorylineDocument, StorylineEnv, StorylineOrigin, StorylineTask, StorylineToolCall,
    StorylineTurn,
};
use crate::formats::timestamp::StorylineTimestamp;
use crate::{InputIssue, InputResult};

pub struct ClaudeCodeFormat;

const SOURCE: &str = "claude-code";

impl TrajectoryFormat for ClaudeCodeFormat {
    fn id(&self) -> DocumentFormat {
        DocumentFormat::ClaudeCode
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

    fn probe(&self, _path: Option<&Path>, content: &[u8]) -> InputResult<ProbeConfidence> {
        Ok(if content_has_claude_fingerprint(content) {
            ProbeConfidence::ContentFingerprint
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

pub(crate) fn looks_like_claude_code_event(value: &Value) -> bool {
    let event_type = value.get("type").and_then(Value::as_str);
    matches!(event_type, Some("user" | "assistant" | "system"))
        && (value.get("sessionId").is_some() || value.get("uuid").is_some())
        && value.get("step_id").is_none()
}

fn content_has_claude_fingerprint(content: &[u8]) -> bool {
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
            if looks_like_claude_code_event(candidate) {
                return true;
            }
        }
        for line in trimmed
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(32)
        {
            if let Ok(value) = serde_json::from_str::<Value>(line)
                && looks_like_claude_code_event(&value)
            {
                return true;
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
    let mut agent_id = "claude-code".to_string();
    let mut agent_version = None;
    let mut cwd = None;
    let mut parent_id = None;
    let mut spawn_call_id = None;
    let mut subagent_id = None;
    let mut model = None;
    let mut started_at = None;
    let mut finished_at = None;
    let mut turns: Vec<StorylineTurn> = Vec::new();
    let mut pending_agent: Option<usize> = None;
    let mut call_index: HashMap<String, (usize, usize)> = HashMap::new();
    let mut unknown = Map::new();
    let mut saw_fingerprint = false;

    let object_count = for_each_jsonl_object(reader, |line_number, value| {
        saw_fingerprint |= looks_like_claude_code_event(&value);
        if let Some(id) = value.get("sessionId").and_then(Value::as_str) {
            session_id = id.to_string();
        }
        if let Some(id) = value.get("agentId").and_then(Value::as_str) {
            subagent_id = Some(id.to_string());
            agent_id = id.to_string();
        }
        if let Some(version) = value.get("version").and_then(Value::as_str) {
            agent_version = Some(version.to_string());
        }
        if cwd.is_none() {
            cwd = value.get("cwd").and_then(Value::as_str).map(str::to_string);
        }
        if parent_id.is_none() {
            parent_id = value
                .get("parentSessionId")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if spawn_call_id.is_none() {
            spawn_call_id = value
                .pointer("/meta/toolUseId")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
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
        if let Some(message_model) = value
            .pointer("/message/model")
            .and_then(Value::as_str)
            .or_else(|| value.get("model").and_then(Value::as_str))
        {
            model = Some(message_model.to_string());
        }

        match value.get("type").and_then(Value::as_str).unwrap_or("") {
            "user" => {
                record_claude_leftover(&value, line_number, &mut unknown);
                apply_user_event(
                    &value,
                    timestamp,
                    &mut turns,
                    &mut pending_agent,
                    &mut call_index,
                );
            }
            "assistant" => {
                record_claude_leftover(&value, line_number, &mut unknown);
                apply_assistant_event(
                    &value,
                    timestamp,
                    model.as_deref(),
                    &mut turns,
                    &mut pending_agent,
                    &mut call_index,
                );
            }
            _ => {
                unknown.insert(format!("/events/{line_number}"), value);
            }
        }
        Ok(())
    })?;
    if object_count == 0 {
        let mut story = StorylineDocument::new(filename_stem(&source_id), "claude-code");
        story.origin = Some(origin(&source_id));
        story.validate()?;
        return Ok(vec![story]);
    }
    if !saw_fingerprint {
        return Err(InputIssue::invalid(
            "claude-code JSONL has no session/transcript fingerprint",
        ));
    }

    let (session_id, parent) = if let Some(subagent_id) = subagent_id {
        (
            subagent_id,
            Some(StoryLink {
                parent_session_id: session_id,
                spawn_call_id,
                spawn_id: None,
                relation: "spawn".into(),
            }),
        )
    } else {
        (
            session_id,
            parent_id.map(|parent_session_id| StoryLink {
                parent_session_id,
                spawn_call_id,
                spawn_id: None,
                relation: "spawn".into(),
            }),
        )
    };

    let mut story = StorylineDocument::new(session_id, agent_id);
    story.origin = Some(origin(&source_id));
    story.agent.version = agent_version;
    story.agent.model_name = model;
    story.parent = parent;
    if let Some(name) = cwd {
        story.task = Some(StorylineTask {
            env: Some(StorylineEnv {
                name: Some(name),
                ..StorylineEnv::default()
            }),
            llm: None,
            result: None,
        });
    }
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

fn record_claude_leftover(value: &Value, line_number: usize, unknown: &mut Map<String, Value>) {
    let content = value
        .pointer("/message/content")
        .or_else(|| value.get("content"));
    let Some(Value::Array(parts)) = content else {
        return;
    };
    for (index, part) in parts.iter().enumerate() {
        let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
        if matches!(kind, "text" | "thinking" | "tool_use" | "tool_result") {
            continue;
        }
        unknown.insert(
            format!("/events/{line_number}/content/{index}"),
            part.clone(),
        );
    }
}

fn origin(source_id: &str) -> StorylineOrigin {
    StorylineOrigin {
        format: SOURCE.into(),
        schema_version: None,
        document_id: Some(source_id.to_string()),
    }
}

fn apply_user_event(
    value: &Value,
    timestamp: Option<StorylineTimestamp>,
    turns: &mut Vec<StorylineTurn>,
    pending_agent: &mut Option<usize>,
    call_index: &mut HashMap<String, (usize, usize)>,
) {
    let content = value
        .pointer("/message/content")
        .cloned()
        .or_else(|| value.get("content").cloned())
        .unwrap_or(Value::Null);
    let tool_results = collect_tool_results(&content);
    if !tool_results.is_empty() {
        for (tool_use_id, result) in tool_results {
            if let Some(&(turn_idx, tool_idx)) = call_index.get(&tool_use_id)
                && let Some(call) = turns
                    .get_mut(turn_idx)
                    .and_then(|turn| turn.tool_calls.as_mut())
                    .and_then(|calls| calls.get_mut(tool_idx))
            {
                call.result = Some(result);
                continue;
            }
            if let Some(index) = *pending_agent {
                turns[index].observation = Some(json!({ "results": [result] }));
            }
        }
        if visible_user_text(&content).is_none() {
            *pending_agent = None;
            return;
        }
    }
    *pending_agent = None;
    if let Some(text) = visible_user_text(&content) {
        turns.push(text_turn(
            next_turn_id(turns),
            timestamp,
            "user",
            text,
            None,
            value.get("isSidechain").and_then(Value::as_bool),
        ));
    }
}

fn apply_assistant_event(
    value: &Value,
    timestamp: Option<StorylineTimestamp>,
    model: Option<&str>,
    turns: &mut Vec<StorylineTurn>,
    pending_agent: &mut Option<usize>,
    call_index: &mut HashMap<String, (usize, usize)>,
) {
    let content = value
        .pointer("/message/content")
        .cloned()
        .or_else(|| value.get("content").cloned())
        .unwrap_or(Value::Null);
    let index = match *pending_agent {
        Some(index) => index,
        None => {
            let index = turns.len();
            turns.push(text_turn(
                next_turn_id(turns),
                timestamp,
                "agent",
                String::new(),
                model,
                value.get("isSidechain").and_then(Value::as_bool),
            ));
            *pending_agent = Some(index);
            index
        }
    };
    if let Some(text) = collect_typed_text(&content, "text") {
        append_text(&mut turns[index], text);
    }
    if let Some(thinking) = collect_typed_text(&content, "thinking") {
        let turn = &mut turns[index];
        turn.reasoning_content = Some(match turn.reasoning_content.take() {
            Some(existing) => format!("{existing}\n{thinking}"),
            None => thinking,
        });
    }
    for (id, name, input) in collect_tool_uses(&content) {
        let tools = turns[index].tool_calls.get_or_insert_with(Vec::new);
        let tool_idx = tools.len();
        tools.push(StorylineToolCall {
            tool_call_id: id.clone(),
            function_name: name,
            arguments: input,
            result: None,
            duration_ms: None,
            extra: None,
            kind: None,
            response: None,
        });
        call_index.insert(id, (index, tool_idx));
    }
}

fn collect_tool_uses(content: &Value) -> Vec<(String, String, Value)> {
    let Some(parts) = content.as_array() else {
        return Vec::new();
    };
    parts
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|part| {
            (
                part.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                part.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string(),
                part.get("input").cloned().unwrap_or(Value::Null),
            )
        })
        .collect()
}

fn collect_tool_results(content: &Value) -> Vec<(String, Value)> {
    let Some(parts) = content.as_array() else {
        return Vec::new();
    };
    parts
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|part| {
            (
                part.get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                part.get("content").cloned().unwrap_or(Value::Null),
            )
        })
        .collect()
}

fn visible_user_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Array(_) => collect_typed_text(content, "text"),
        _ => None,
    }
}

fn collect_typed_text(content: &Value, kind: &str) -> Option<String> {
    let parts = content.as_array()?;
    let texts = parts
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some(kind))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    (!texts.is_empty()).then_some(texts.join(""))
}

fn text_turn(
    id: i64,
    timestamp: Option<StorylineTimestamp>,
    source: &str,
    text: String,
    model: Option<&str>,
    sidechain: Option<bool>,
) -> StorylineTurn {
    StorylineTurn {
        id,
        kind: None,
        timestamp,
        source: source.into(),
        message: Value::String(text),
        reasoning_content: None,
        reasoning_effort: None,
        tool_calls: None,
        observation: None,
        metrics: None,
        model_name: model.map(str::to_string),
        llm_call_count: (source == "agent").then_some(1),
        is_copied_context: None,
        latency_ms: None,
        ttft_ms: None,
        extra: sidechain.and_then(|flag| flag.then_some(json!({ "isSidechain": true }))),
        env: None,
        prompt: None,
        finished_at: None,
    }
}

fn append_text(turn: &mut StorylineTurn, text: String) {
    match &mut turn.message {
        Value::String(existing) if existing.is_empty() => *existing = text,
        Value::String(existing) => {
            existing.push('\n');
            existing.push_str(&text);
        }
        other => *other = Value::String(text),
    }
}

fn next_turn_id(turns: &[StorylineTurn]) -> i64 {
    i64::try_from(turns.len() + 1).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::formats::codec::{DocumentSource, decode_all};
    use serde_json::json;

    fn claude_code_to_storylines(
        input: &str,
        relative_path: &str,
    ) -> InputResult<Vec<StorylineDocument>> {
        decode_all(
            &ClaudeCodeFormat,
            &mut Cursor::new(input.as_bytes()),
            &DocumentSource::new(relative_path),
        )
    }

    const FIXTURE: &str = r#"{"type":"user","sessionId":"claude-1","cwd":"/tmp/app","version":"2.1.0","timestamp":"2026-08-03T08:00:00.000Z","message":{"role":"user","content":"fix the test"}}
{"type":"assistant","sessionId":"claude-1","timestamp":"2026-08-03T08:00:01.000Z","message":{"model":"claude-opus","content":[{"type":"thinking","text":"read test"},{"type":"text","text":"looking"},{"type":"tool_use","id":"toolu_1","name":"Read","input":{"path":"t.rs"}}]}}
{"type":"user","sessionId":"claude-1","timestamp":"2026-08-03T08:00:02.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"fn t(){}"}]}}
{"type":"assistant","sessionId":"claude-1","timestamp":"2026-08-03T08:00:03.000Z","message":{"content":[{"type":"text","text":"done"}]}}
{"type":"compact_boundary","sessionId":"claude-1","timestamp":"2026-08-03T08:00:04.000Z"}
"#;

    #[test]
    fn maps_claude_transcript_tools_and_skips_compact_boundary() {
        let stories = claude_code_to_storylines(FIXTURE, "sess.jsonl").unwrap();
        let story = &stories[0];
        assert_eq!(story.session_id, "claude-1");
        assert_eq!(story.origin.as_ref().unwrap().format, "claude-code");
        assert_eq!(story.turns.len(), 3);
        assert_eq!(story.turns[0].source, "user");
        assert_eq!(story.turns[0].message, json!("fix the test"));
        assert_eq!(story.turns[1].source, "agent");
        assert_eq!(
            story.turns[1].reasoning_content.as_deref(),
            Some("read test")
        );
        let tools = story.turns[1].tool_calls.as_ref().unwrap();
        assert_eq!(tools[0].tool_call_id, "toolu_1");
        assert_eq!(tools[0].function_name, "Read");
        assert_eq!(tools[0].result, Some(json!("fn t(){}")));
        assert_eq!(story.turns[2].message, json!("done"));
        assert!(
            story.unknown_fields.sources["claude-code"]
                .fields
                .contains_key("/events/5")
        );
        assert_eq!(
            story.unknown_fields.sources["claude-code"].fields["/events/5"]["type"],
            json!("compact_boundary")
        );
        assert_eq!(
            story.unknown_fields.sources["claude-code"].fields["/events/5"]["sessionId"],
            json!("claude-1")
        );
    }

    #[test]
    fn subagent_uses_agent_id_and_links_parent_session() {
        let input = r#"{"type":"assistant","sessionId":"parent-sess","agentId":"agent-a","uuid":"u1","timestamp":"2026-08-03T08:00:00.000Z","message":{"content":[{"type":"text","text":"child work"}]},"meta":{"toolUseId":"toolu_spawn"}}
"#;
        let story = &claude_code_to_storylines(input, "agent-a.jsonl").unwrap()[0];
        assert_eq!(story.session_id, "agent-a");
        let parent = story.parent.as_ref().expect("parent link");
        assert_eq!(parent.parent_session_id, "parent-sess");
        assert_eq!(parent.spawn_call_id.as_deref(), Some("toolu_spawn"));
        assert_eq!(story.turns[0].message, json!("child work"));
    }

    #[test]
    fn rejects_jsonl_without_claude_fingerprint() {
        let error = claude_code_to_storylines("{\"agentId\":\"x\"}\n", ".meta.json").unwrap_err();
        assert!(error.to_string().contains("fingerprint"), "{error}");
        let error = claude_code_to_storylines("[]\n", "array.jsonl").unwrap_err();
        assert!(error.to_string().contains("object"), "{error}");
    }
}

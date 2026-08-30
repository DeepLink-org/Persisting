use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use web_time::{SystemTime, UNIX_EPOCH};

use crate::api;
use crate::components::trajectory_fence;
use crate::llm::{self, LlmConfig};
use crate::model::{QueryCatalog, QueryEvidence, RunAnalysis, RunSummary, TurnDetail};

pub const THREAD_BYTE_LIMIT: usize = 200 * 1024;
pub const LLM_MESSAGE_BYTE_LIMIT: usize = 32 * 1024;
pub const TURN_BODY_LIMIT: usize = 8 * 1024;
pub const TOOL_NAMES: [&str; 3] = ["get_analysis", "get_turn", "query_sql"];
static JSON_TOOL_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadRole {
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadMessage {
    pub role: ThreadRole,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ParsedToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
    #[serde(default)]
    pub truncated: bool,
    /// Thinking-mode models require this echoed on the next chat request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssistantThread {
    pub messages: Vec<ThreadMessage>,
    pub updated_at: i64,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParsedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AssistantTurn {
    ToolCalls(Vec<ParsedToolCall>),
    Final(String),
    Invalid,
}

pub const MAX_TOOL_ROUNDS: usize = 8;

pub struct LoopState {
    pub messages: Vec<ThreadMessage>,
    pub tool_rounds: usize,
    pub json_mode: bool,
    pub illegal_json_streak: usize,
    pub fetched_turn_ids: Vec<i64>,
    pub force_final: bool,
}

#[derive(Debug, PartialEq)]
pub enum DriveResult {
    Continue,
    Done { text: String },
    Failed { message: String },
}

pub fn apply_model_turn(
    state: &mut LoopState,
    turn: AssistantTurn,
    reasoning_content: Option<String>,
    mut execute: impl FnMut(&ParsedToolCall) -> String,
) -> DriveResult {
    if state.force_final {
        return match turn {
            AssistantTurn::Final(text) if !text.trim().is_empty() => {
                state.illegal_json_streak = 0;
                DriveResult::Done { text }
            }
            _ => DriveResult::Failed {
                message:
                    "The model did not produce a final answer because the available run data was insufficient."
                        .into(),
            },
        };
    }

    match turn {
        AssistantTurn::ToolCalls(calls) => {
            let remaining = MAX_TOOL_ROUNDS.saturating_sub(state.tool_rounds);
            let calls = calls.into_iter().take(remaining).collect::<Vec<_>>();
            if !calls.is_empty() {
                state.messages.push(ThreadMessage {
                    role: ThreadRole::Assistant,
                    text: String::new(),
                    tool_calls: Some(calls.clone()),
                    tool_call_id: None,
                    tool_name: None,
                    sql: None,
                    truncated: false,
                    reasoning_content,
                });
            }

            for call in calls {
                let result = execute(&call);
                state.tool_rounds += 1;

                if call.name == "get_turn" {
                    let turn_id = call.arguments.get("turn_id").and_then(|value| {
                        value
                            .as_i64()
                            .or_else(|| value.as_str().and_then(|raw| raw.parse().ok()))
                    });
                    if let Some(turn_id) = turn_id {
                        let marker = format!("[turn:{turn_id}]");
                        if result.contains(&marker) && !state.fetched_turn_ids.contains(&turn_id) {
                            state.fetched_turn_ids.push(turn_id);
                        }
                    }
                }

                state.messages.push(ThreadMessage {
                    role: ThreadRole::Tool,
                    text: result,
                    tool_calls: None,
                    tool_call_id: Some(call.id),
                    tool_name: Some(call.name),
                    sql: None,
                    truncated: false,
                    reasoning_content: None,
                });
            }

            if state.tool_rounds >= MAX_TOOL_ROUNDS {
                state.force_final = true;
            }
            DriveResult::Continue
        }
        AssistantTurn::Final(text) => {
            state.illegal_json_streak = 0;
            DriveResult::Done { text }
        }
        AssistantTurn::Invalid if !state.json_mode => {
            state.json_mode = true;
            DriveResult::Continue
        }
        AssistantTurn::Invalid => {
            state.illegal_json_streak += 1;
            if state.illegal_json_streak >= 2 {
                DriveResult::Failed {
                    message: "The model could not use tool-calling. Try a different OpenAI-compatible model in Settings.".into(),
                }
            } else {
                DriveResult::Continue
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentAnswer {
    pub thread: AssistantThread,
    pub text: String,
    pub sql: Option<String>,
    pub truncated: bool,
    pub fetched_turn_ids: Vec<i64>,
}

pub struct AnswerRequest<'a> {
    pub config: &'a LlmConfig,
    pub user_message: &'a str,
    pub run: &'a RunSummary,
    pub analysis: &'a RunAnalysis,
    pub focused_turn_id: Option<i64>,
    pub thread: AssistantThread,
    pub on_step: Option<&'a dyn Fn(&str)>,
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(1)
        .max(1)
}

pub fn thread_storage_key(run: &RunSummary) -> String {
    format!("pchronicle_copilot:{}", run.query())
}

pub fn thread_byte_size(thread: &AssistantThread) -> usize {
    serde_json::to_string(thread)
        .map(|raw| raw.len())
        .unwrap_or(0)
}

fn shrink_tool_text(text: &str) -> String {
    const KEEP: usize = 512;
    if text.len() <= KEEP {
        return text.to_string();
    }
    let mut end = KEEP.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[… truncated …]", &text[..end])
}

pub fn trim_thread(thread: &mut AssistantThread) {
    while thread_byte_size(thread) > THREAD_BYTE_LIMIT {
        let Some(index) = thread
            .messages
            .iter()
            .position(|message| message.role == ThreadRole::Tool && !message.truncated)
        else {
            break;
        };
        thread.messages[index].text = shrink_tool_text(&thread.messages[index].text);
        thread.messages[index].truncated = true;
        thread.truncated = true;
    }
}

pub fn compress_messages_for_llm(messages: &[ThreadMessage]) -> Vec<ThreadMessage> {
    let mut out = messages.to_vec();
    loop {
        let encoded = serde_json::to_string(&out).unwrap_or_default();
        if encoded.len() <= LLM_MESSAGE_BYTE_LIMIT {
            return out;
        }
        let Some(index) = out.iter().position(|message| {
            message.role == ThreadRole::Tool
                && message.text.len() > 64
                && shrink_tool_text(&message.text) != message.text
        }) else {
            return out;
        };
        out[index].text = shrink_tool_text(&out[index].text);
        out[index].truncated = true;
    }
}

pub async fn answer(request: AnswerRequest<'_>) -> Result<AgentAnswer, String> {
    if !request.config.is_configured() {
        return Err(
            "Configure an OpenAI-compatible model in Settings before using Assistant.".into(),
        );
    }

    let mut state = LoopState {
        messages: request.thread.messages.clone(),
        tool_rounds: 0,
        json_mode: false,
        illegal_json_streak: 0,
        fetched_turn_ids: Vec::new(),
        force_final: false,
    };
    state.messages.push(ThreadMessage {
        role: ThreadRole::User,
        text: request.user_message.to_string(),
        tool_calls: None,
        tool_call_id: None,
        tool_name: None,
        sql: None,
        truncated: false,
        reasoning_content: None,
    });

    let catalog_context = match api::query_catalog().await {
        Ok(catalog) => format_catalog_schema(&catalog),
        Err(_) => "SQL catalog unavailable; do not guess table or column names.".into(),
    };
    let base_system = system_prompt(
        request.run,
        request.analysis,
        request.focused_turn_id,
        &catalog_context,
    );
    let mut last_sql = None;
    let mut evidence_truncated = false;

    loop {
        let tools_enabled = !state.json_mode && !state.force_final;
        let system = mode_system_prompt(&base_system, state.json_mode, state.force_final);
        let messages =
            openai_messages(&compress_messages_for_llm(&state.messages), state.json_mode);
        let message = match chat_with_tools(
            request.config,
            &system,
            messages,
            tools_enabled,
            state.json_mode,
        )
        .await
        {
            Ok(message) => message,
            Err(error) if !state.json_mode && error.suggests_tools_unsupported() => {
                state.json_mode = true;
                continue;
            }
            Err(error) => return Err(error.message),
        };

        let turn = if state.json_mode {
            message
                .get("content")
                .and_then(Value::as_str)
                .map(parse_json_fallback)
                .unwrap_or(AssistantTurn::Invalid)
        } else {
            parse_native_message(&message)
        };

        let mut results = VecDeque::new();
        let mut sql_by_call = HashMap::new();
        if let AssistantTurn::ToolCalls(calls) = &turn {
            let remaining = MAX_TOOL_ROUNDS.saturating_sub(state.tool_rounds);
            for call in calls.iter().take(remaining) {
                if let Some(on_step) = request.on_step {
                    on_step(&tool_step(call));
                }
                let execution = execute_tool(call, request.run, request.analysis).await;
                if execution.truncated {
                    evidence_truncated = true;
                }
                if let Some(sql) = execution.sql {
                    last_sql = Some(sql.clone());
                    sql_by_call.insert(call.id.clone(), sql);
                }
                results.push_back(execution.text);
            }
        }

        let result = apply_model_turn(
            &mut state,
            turn,
            extract_reasoning_content(&message),
            |_| {
                results
                    .pop_front()
                    .unwrap_or_else(|| "Tool call budget exhausted.".into())
            },
        );
        for message in state.messages.iter_mut().rev() {
            let Some(call_id) = message.tool_call_id.as_ref() else {
                continue;
            };
            if let Some(sql) = sql_by_call.remove(call_id) {
                message.sql = Some(sql);
            }
            if sql_by_call.is_empty() {
                break;
            }
        }

        match result {
            DriveResult::Continue => {}
            DriveResult::Done { text } => {
                return Ok(finish_answer(
                    request.thread,
                    state,
                    text,
                    last_sql,
                    evidence_truncated,
                ));
            }
            DriveResult::Failed { message } => {
                return Ok(finish_answer(
                    request.thread,
                    state,
                    message,
                    last_sql,
                    evidence_truncated,
                ));
            }
        }
    }
}

fn system_prompt(
    run: &RunSummary,
    analysis: &RunAnalysis,
    focused_turn_id: Option<i64>,
    catalog_context: &str,
) -> String {
    let mut prompt = format!(
        "You are pChronicle Assistant for local Agent run debugging. Use only data from the current run. Call tools when details are needed; do not invent facts. Missing measurements are not zero. Do not infer an error from arbitrary message text. Reconstructed events are derived from imported run data, not directly recorded events. Answer in the user's language, preferably in 3–7 concise bullets. Separate recorded facts from inference. Cite every inspected step with the internal marker [turn:ID]. Mention coverage or truncation when tool results report it.\n\nCurrent run analysis:\nsession={}\nstatus={}\nevent_origin={}\nstep_count={}\nevent_count={}\nerror_count={}\ntotal_tokens={}\nlatency_p95={}\nlatency_samples={}/{}\n\nquery_sql schema:\n{}",
        run.session_id,
        run.status,
        analysis.event_provenance.as_str(),
        analysis.turn_count,
        analysis.event_count,
        analysis.error_count,
        analysis
            .total_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unavailable".into()),
        analysis
            .latency_ms
            .p95
            .map(|value| format!("{value:.1}"))
            .unwrap_or_else(|| "unavailable".into()),
        analysis.latency_ms.sample_count,
        analysis.latency_ms.total_count,
        catalog_context,
    );
    if let Some(turn_id) = focused_turn_id {
        prompt.push_str(&format!(
            "\nThe user is currently viewing step #{turn_id}. Do not assume its body; call get_turn if needed."
        ));
    }
    prompt
}

fn format_catalog_schema(catalog: &QueryCatalog) -> String {
    let tables = crate::model::queryable_tables(catalog);
    if tables.is_empty() {
        return "SQL catalog is available but contains no tables.".into();
    }
    tables
        .iter()
        .map(|table| {
            let fields = table
                .fields
                .iter()
                .map(|field| format!("{} {}", field.name, field.data_type))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} [{}] ({fields})", table.name, table.kind)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn mode_system_prompt(base: &str, json_mode: bool, force_final: bool) -> String {
    let mut prompt = base.to_string();
    if json_mode {
        prompt.push_str(
            "\nReturn JSON only: either {\"tool\":\"get_analysis|get_turn|query_sql\",\"arguments\":{}} or {\"final\":\"...\"}.",
        );
    }
    if force_final {
        prompt.push_str("\nAnswer now from the run data already gathered. Do not call tools.");
    }
    prompt
}

fn openai_messages(messages: &[ThreadMessage], json_mode: bool) -> Vec<Value> {
    let mut mapped = Vec::new();
    for message in messages {
        match message.role {
            ThreadRole::User => {
                mapped.push(json!({"role": "user", "content": message.text}));
            }
            ThreadRole::Assistant => {
                if let Some(calls) = message
                    .tool_calls
                    .as_ref()
                    .filter(|calls| !calls.is_empty())
                {
                    if json_mode {
                        let replay = calls
                            .iter()
                            .map(|call| {
                                serde_json::to_string(&json!({
                                    "tool": call.name,
                                    "arguments": call.arguments,
                                }))
                                .unwrap_or_else(|_| "{}".into())
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        mapped.push(json!({"role": "assistant", "content": replay}));
                    } else {
                        let tool_calls = calls
                            .iter()
                            .map(|call| {
                                json!({
                                    "id": call.id,
                                    "type": "function",
                                    "function": {
                                        "name": call.name,
                                        "arguments": serde_json::to_string(&call.arguments)
                                            .unwrap_or_else(|_| "{}".into()),
                                    },
                                })
                            })
                            .collect::<Vec<_>>();
                        mapped.push(assistant_tool_call_message(
                            tool_calls,
                            message.reasoning_content.as_deref(),
                        ));
                    }
                } else {
                    mapped.push(assistant_text_message(
                        &message.text,
                        message.reasoning_content.as_deref(),
                    ));
                }
            }
            ThreadRole::Tool if json_mode => {
                let name = message.tool_name.as_deref().unwrap_or("unknown");
                mapped.push(json!({
                    "role": "user",
                    "content": format!("Tool {name} result:\n{}", message.text),
                }));
            }
            ThreadRole::Tool => {
                mapped.push(json!({
                    "role": "tool",
                    "tool_call_id": message.tool_call_id,
                    "content": message.text,
                }));
            }
        }
    }

    mapped
}

fn assistant_tool_call_message(tool_calls: Vec<Value>, reasoning_content: Option<&str>) -> Value {
    json!({
        "role": "assistant",
        "content": null,
        "tool_calls": tool_calls,
        "reasoning_content": reasoning_content.unwrap_or(""),
    })
}

fn assistant_text_message(text: &str, reasoning_content: Option<&str>) -> Value {
    let mut payload = json!({"role": "assistant", "content": text});
    if let Some(reasoning) = reasoning_content.filter(|text| !text.is_empty()) {
        payload["reasoning_content"] = json!(reasoning);
    }
    payload
}

fn extract_reasoning_content(message: &Value) -> Option<String> {
    ["reasoning_content", "reasoning", "reasoning_text"]
        .into_iter()
        .find_map(|key| {
            message
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        })
}

fn tools_payload() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": TOOL_NAMES[0],
                "description": "Get aggregate analysis for the current run.",
                "parameters": {"type": "object", "properties": {}}
            }
        },
        {
            "type": "function",
            "function": {
                "name": TOOL_NAMES[1],
                "description": "Fetch one step in the current run.",
                "parameters": {
                    "type": "object",
                    "properties": {"turn_id": {"type": "integer"}},
                    "required": ["turn_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": TOOL_NAMES[2],
                "description": "Run one server-enforced read-only SQL query.",
                "parameters": {
                    "type": "object",
                    "properties": {"sql": {"type": "string"}},
                    "required": ["sql"]
                }
            }
        }
    ])
}

async fn chat_with_tools(
    config: &LlmConfig,
    system: &str,
    messages: Vec<Value>,
    tools_enabled: bool,
    json_mode: bool,
) -> Result<Value, llm::CompletionError> {
    let first = llm::complete(
        config,
        llm::CompletionRequest {
            system: system.into(),
            messages: messages.clone(),
            tools: tools_enabled.then(tools_payload),
            response_format: json_mode.then(|| json!({"type":"json_object"})),
            temperature: if json_mode { 0.1 } else { 0.3 },
        },
    )
    .await;
    match first {
        Err(error) if json_mode && error.suggests_response_format_unsupported() => {
            llm::complete(
                config,
                llm::CompletionRequest {
                    system: system.into(),
                    messages,
                    tools: tools_enabled.then(tools_payload),
                    response_format: None,
                    temperature: if json_mode { 0.1 } else { 0.3 },
                },
            )
            .await
        }
        result => result,
    }
}

struct ToolExecution {
    text: String,
    sql: Option<String>,
    truncated: bool,
}

async fn execute_tool(
    call: &ParsedToolCall,
    run: &RunSummary,
    analysis: &RunAnalysis,
) -> ToolExecution {
    match call.name.as_str() {
        "get_analysis" => ToolExecution {
            text: format_analysis_result(analysis),
            sql: None,
            truncated: false,
        },
        "get_turn" => {
            let turn_id = call.arguments.get("turn_id").and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_str().and_then(|raw| raw.parse().ok()))
            });
            let text = match turn_id {
                Some(turn_id) => api::turn_detail(run, turn_id)
                    .await
                    .map(|detail| format_turn_result(&detail))
                    .unwrap_or_else(|error| format!("get_turn failed: {error}")),
                None => "get_turn failed: `turn_id` must be an integer.".into(),
            };
            ToolExecution {
                truncated: text.contains("truncated=true"),
                text,
                sql: None,
            }
        }
        "query_sql" => {
            let Some(sql) = call.arguments.get("sql").and_then(Value::as_str) else {
                return ToolExecution {
                    text: "query_sql failed: `sql` must be a string.".into(),
                    sql: None,
                    truncated: false,
                };
            };
            match api::query_evidence(sql).await {
                Ok(evidence) => ToolExecution {
                    text: format_sql_result(sql, &evidence),
                    sql: Some(sql.to_string()),
                    truncated: evidence.truncated,
                },
                Err(error) => ToolExecution {
                    text: format!("query_sql failed: {error}"),
                    sql: None,
                    truncated: false,
                },
            }
        }
        name => ToolExecution {
            text: unknown_tool_result(name),
            sql: None,
            truncated: false,
        },
    }
}

fn tool_step(call: &ParsedToolCall) -> String {
    if call.name == "get_turn" {
        if let Some(turn_id) = call.arguments.get("turn_id").and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|raw| raw.parse().ok()))
        }) {
            return format!("get_turn #{turn_id}");
        }
    }
    call.name.clone()
}

fn finish_answer(
    mut thread: AssistantThread,
    state: LoopState,
    mut text: String,
    sql: Option<String>,
    evidence_truncated: bool,
) -> AgentAnswer {
    let fetched_turn_ids = state.fetched_turn_ids;
    if !fetched_turn_ids.is_empty() {
        text.push_str("\n\n");
        text.push_str(&trajectory_fence("Cited turns", fetched_turn_ids.clone()));
    }
    thread.messages = state.messages;
    thread.messages.push(ThreadMessage {
        role: ThreadRole::Assistant,
        text: text.clone(),
        tool_calls: None,
        tool_call_id: None,
        tool_name: None,
        sql: sql.clone(),
        truncated: evidence_truncated,
        reasoning_content: None,
    });
    thread.updated_at = now_millis();
    trim_thread(&mut thread);
    AgentAnswer {
        truncated: evidence_truncated || thread.truncated,
        thread,
        text,
        sql,
        fetched_turn_ids,
    }
}

fn extract_json(value: &str) -> &str {
    let value = value.trim();
    if value.starts_with('{') {
        return value;
    }
    if let Some(start) = value.find("```") {
        let rest = &value[start + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        if let Some(end) = rest.find("```") {
            return rest[..end].trim();
        }
    }
    value
}

fn parse_arguments(value: &Value) -> Result<Value, ()> {
    match value {
        Value::String(raw) => {
            let parsed: Value = serde_json::from_str(raw).map_err(|_| ())?;
            if parsed.is_object() {
                Ok(parsed)
            } else {
                Err(())
            }
        }
        Value::Object(_) => Ok(value.clone()),
        _ => Err(()),
    }
}

pub fn parse_native_message(message: &Value) -> AssistantTurn {
    if let Some(tool_calls) = message.get("tool_calls") {
        let Some(calls) = tool_calls.as_array() else {
            return AssistantTurn::Invalid;
        };
        if !calls.is_empty() {
            let mut parsed = Vec::new();
            for (index, call) in calls.iter().enumerate() {
                let fallback = format!("call-{index}");
                let id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(&fallback)
                    .to_string();
                let name = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let arguments = call
                    .pointer("/function/arguments")
                    .ok_or(())
                    .and_then(parse_arguments);
                match (name.is_empty(), arguments) {
                    (false, Ok(arguments)) => parsed.push(ParsedToolCall {
                        id,
                        name,
                        arguments,
                    }),
                    _ => return AssistantTurn::Invalid,
                }
            }
            return AssistantTurn::ToolCalls(parsed);
        }
        // empty array: fall through to content
    }
    let content = message.get("content").and_then(Value::as_str).unwrap_or("");
    if looks_like_dsml_tool_calls(content) {
        return match parse_dsml_tool_calls(content) {
            Some(calls) if !calls.is_empty() => AssistantTurn::ToolCalls(calls),
            _ => AssistantTurn::Invalid,
        };
    }
    if !content.trim().is_empty() {
        AssistantTurn::Final(content.to_string())
    } else {
        AssistantTurn::Invalid
    }
}

const DSML_MARKER: &str = "|DSML|";

pub fn visible_assistant_text(text: &str) -> Option<String> {
    if !looks_like_dsml_tool_calls(text) {
        return Some(text.to_string());
    }
    let stripped = strip_dsml_markup(text);
    let trimmed = stripped.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(stripped)
    }
}

fn looks_like_dsml_tool_calls(content: &str) -> bool {
    let content = canonicalize_dsml_tags(content);
    content.contains(&format!("<{DSML_MARKER}tool_calls>"))
        || content.contains(&format!("<{DSML_MARKER}function_calls>"))
        || content.contains(&format!("<{DSML_MARKER}invoke"))
        || content.contains(&format!("</{DSML_MARKER}tool_calls>"))
        || content.contains(&format!("</{DSML_MARKER}function_calls>"))
}

fn canonicalize_dsml_tags(content: &str) -> String {
    let mut output = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(offset) = rest.find("DSML") {
        output.push_str(&rest[..offset]);
        let (before, dsml_and_after) = rest.split_at(offset);
        let prefix = before
            .chars()
            .rev()
            .take_while(|ch| ch.is_whitespace() || is_dsml_pipe(*ch))
            .collect::<String>();
        let pipe_left = prefix.chars().any(is_dsml_pipe);
        let after_dsml = &dsml_and_after["DSML".len()..];
        let suffix_len = after_dsml
            .chars()
            .take_while(|ch| ch.is_whitespace() || is_dsml_pipe(*ch))
            .map(char::len_utf8)
            .sum::<usize>();
        let pipe_right = after_dsml[..suffix_len].chars().any(is_dsml_pipe);
        if pipe_left && pipe_right {
            output.truncate(output.len() - prefix.len());
            output.push_str(DSML_MARKER);
            rest = &after_dsml[suffix_len..];
        } else {
            output.push_str("DSML");
            rest = after_dsml;
        }
    }
    output.push_str(rest);
    output
        .replace(
            &format!("<{DSML_MARKER}function_calls>"),
            &format!("<{DSML_MARKER}tool_calls>"),
        )
        .replace(
            &format!("</{DSML_MARKER}function_calls>"),
            &format!("</{DSML_MARKER}tool_calls>"),
        )
}

fn is_dsml_pipe(ch: char) -> bool {
    matches!(ch, '|' | '｜' | '│')
}

fn parse_dsml_tool_calls(content: &str) -> Option<Vec<ParsedToolCall>> {
    let content = canonicalize_dsml_tags(content);
    let open = format!("<{DSML_MARKER}tool_calls>");
    let close = format!("</{DSML_MARKER}tool_calls>");
    let body = if let Some(start) = content.find(&open) {
        let start = start + open.len();
        let end = content[start..]
            .find(&close)
            .map(|rel| start + rel)
            .unwrap_or(content.len());
        &content[start..end]
    } else {
        content.as_str()
    };
    let invoke_open = format!("<{DSML_MARKER}invoke");
    let invoke_close = format!("</{DSML_MARKER}invoke>");
    let mut calls = Vec::new();
    let mut rest = body;
    while let Some(offset) = rest.find(&invoke_open) {
        rest = &rest[offset + invoke_open.len()..];
        let gt = rest.find('>')?;
        let attrs = &rest[..gt];
        let name = dsml_attr(attrs, "name")?;
        rest = &rest[gt + 1..];
        let close_at = rest.find(&invoke_close)?;
        let params = parse_dsml_parameters(DSML_MARKER, &rest[..close_at])?;
        rest = &rest[close_at + invoke_close.len()..];
        if name.trim().is_empty() {
            return None;
        }
        calls.push(ParsedToolCall {
            id: format!(
                "dsml-{}-{}",
                now_millis(),
                JSON_TOOL_ID.fetch_add(1, Ordering::Relaxed)
            ),
            name: name.to_string(),
            arguments: params,
        });
    }
    (!calls.is_empty()).then_some(calls)
}

fn strip_dsml_markup(text: &str) -> String {
    let mut text = canonicalize_dsml_tags(text);
    let open = format!("<{DSML_MARKER}tool_calls>");
    let close = format!("</{DSML_MARKER}tool_calls>");
    while let Some(start) = text.find(&open) {
        if let Some(rel) = text[start + open.len()..].find(&close) {
            let end = start + open.len() + rel + close.len();
            text.replace_range(start..end, "");
        } else {
            text.replace_range(start.., "");
            break;
        }
    }
    let invoke_open = format!("<{DSML_MARKER}invoke");
    let invoke_close = format!("</{DSML_MARKER}invoke>");
    while let Some(start) = text.find(&invoke_open) {
        if let Some(rel) = text[start..].find(&invoke_close) {
            let end = start + rel + invoke_close.len();
            text.replace_range(start..end, "");
        } else {
            text.replace_range(start.., "");
            break;
        }
    }
    for tag in [
        format!("</{DSML_MARKER}invoke>"),
        format!("</{DSML_MARKER}tool_calls>"),
        format!("</{DSML_MARKER}parameter>"),
    ] {
        text = text.replace(&tag, "");
    }
    text
}

fn parse_dsml_parameters(marker: &str, body: &str) -> Option<Value> {
    let open = format!("<{marker}parameter");
    let close = format!("</{marker}parameter>");
    let mut map = serde_json::Map::new();
    let mut rest = body;
    while let Some(offset) = rest.find(&open) {
        rest = &rest[offset + open.len()..];
        let gt = rest.find('>')?;
        let attrs = &rest[..gt];
        let name = dsml_attr(attrs, "name")?;
        let is_string = dsml_attr(attrs, "string").unwrap_or("true") != "false";
        rest = &rest[gt + 1..];
        let close_at = rest.find(&close)?;
        let raw = rest[..close_at].trim();
        rest = &rest[close_at + close.len()..];
        let value = if is_string {
            Value::String(raw.to_string())
        } else {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
        };
        map.insert(name.to_string(), value);
    }
    Some(Value::Object(map))
}

fn dsml_attr<'a>(attrs: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=\"");
    let start = attrs.find(&needle)? + needle.len();
    let end = attrs[start..].find('"')? + start;
    Some(&attrs[start..end])
}

pub fn parse_json_fallback(content: &str) -> AssistantTurn {
    let raw = extract_json(content);
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return AssistantTurn::Invalid;
    };
    if let Some(final_text) = value
        .get("final")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        return AssistantTurn::Final(final_text.to_string());
    }
    let Some(name) = value.get("tool").and_then(Value::as_str) else {
        return AssistantTurn::Invalid;
    };
    if !matches!(name, "get_analysis" | "get_turn" | "query_sql") {
        return AssistantTurn::Invalid;
    }
    let arguments = match value.get("arguments") {
        None => json!({}),
        Some(args) if args.is_object() => args.clone(),
        Some(_) => return AssistantTurn::Invalid,
    };
    AssistantTurn::ToolCalls(vec![ParsedToolCall {
        id: format!(
            "json-{}-{}",
            now_millis(),
            JSON_TOOL_ID.fetch_add(1, Ordering::Relaxed)
        ),
        name: name.into(),
        arguments,
    }])
}

fn truncate(value: &str, limit: usize) -> (String, bool) {
    if value.len() <= limit {
        return (value.to_string(), false);
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}\n[… truncated …]", &value[..end]), true)
}

pub fn unknown_tool_result(name: &str) -> String {
    format!("Unknown tool `{name}`. Valid tools: get_analysis, get_turn, query_sql.")
}

pub fn format_analysis_result(analysis: &RunAnalysis) -> String {
    format!(
        "event_provenance={} turns={} events={} tools={} explicit_errors={} tokens={} latency_p95={} latency_samples={}/{}\nsources={:?}\nkinds={:?}\nmodels={:?}\ntool_names={:?}",
        analysis.event_provenance.as_str(),
        analysis.turn_count,
        analysis.event_count,
        analysis.tool_call_count,
        analysis.error_count,
        analysis
            .total_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unavailable".into()),
        analysis
            .latency_ms
            .p95
            .map(|value| format!("{value:.1}"))
            .unwrap_or_else(|| "unavailable".into()),
        analysis.latency_ms.sample_count,
        analysis.latency_ms.total_count,
        analysis
            .source_breakdown
            .iter()
            .map(|item| format!("{}:{}", item.name, item.turn_count))
            .collect::<Vec<_>>(),
        analysis
            .kind_breakdown
            .iter()
            .map(|item| format!("{}:{}", item.name, item.turn_count))
            .collect::<Vec<_>>(),
        analysis
            .model_breakdown
            .iter()
            .map(|item| format!("{}:{}", item.name, item.turn_count))
            .collect::<Vec<_>>(),
        analysis
            .tools
            .iter()
            .map(|tool| format!("{}:{}", tool.name, tool.count))
            .collect::<Vec<_>>(),
    )
}

pub fn format_turn_result(detail: &TurnDetail) -> String {
    let (body, truncated) = truncate(&detail.turn.text(), TURN_BODY_LIMIT);
    format!(
        "[turn:{}] source={} kind={} model={} latency={} tools={}\n{}\n{}",
        detail.summary.id,
        detail.summary.source,
        detail.summary.kind.as_deref().unwrap_or("unknown"),
        detail
            .summary
            .model_name
            .as_deref()
            .unwrap_or("unavailable"),
        detail
            .summary
            .latency_ms
            .map(|value| format!("{value:.1}"))
            .unwrap_or_else(|| "unavailable".into()),
        detail.summary.tool_names.join(","),
        body,
        if truncated {
            "truncated=true"
        } else {
            "truncated=false"
        }
    )
}

pub fn format_sql_result(sql: &str, evidence: &QueryEvidence) -> String {
    format!(
        "SQL:\n{sql}\nreturned_rows={} truncated={}\n{}",
        evidence.returned_rows,
        evidence.truncated,
        serde_json::to_string(&evidence.rows).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TurnSummary;

    fn empty_state() -> LoopState {
        LoopState {
            messages: Vec::new(),
            tool_rounds: 0,
            json_mode: false,
            illegal_json_streak: 0,
            fetched_turn_ids: Vec::new(),
            force_final: false,
        }
    }

    #[test]
    fn unconfigured_config_is_detected() {
        let config = LlmConfig::default();
        assert!(!config.is_configured());
    }

    #[test]
    fn loop_runs_three_tools_then_final() {
        let mut state = empty_state();
        let calls = vec![
            ParsedToolCall {
                id: "1".into(),
                name: "get_analysis".into(),
                arguments: json!({}),
            },
            ParsedToolCall {
                id: "2".into(),
                name: "get_turn".into(),
                arguments: json!({"turn_id": 4}),
            },
            ParsedToolCall {
                id: "3".into(),
                name: "query_sql".into(),
                arguments: json!({"sql": "SELECT 1"}),
            },
        ];
        let result = apply_model_turn(&mut state, AssistantTurn::ToolCalls(calls), None, |call| {
            format!("ok {}", call.name)
        });
        assert!(matches!(result, DriveResult::Continue));
        assert_eq!(state.tool_rounds, 3);
        assert_eq!(state.messages.len(), 4);
        assert_eq!(state.messages[0].role, ThreadRole::Assistant);
        assert_eq!(state.messages[0].tool_calls.as_ref().unwrap().len(), 3);
        let done = apply_model_turn(
            &mut state,
            AssistantTurn::Final("see [turn:4]".into()),
            None,
            |_| String::new(),
        );
        assert_eq!(
            done,
            DriveResult::Done {
                text: "see [turn:4]".into()
            }
        );
    }

    #[test]
    fn unknown_tool_does_not_stop_the_loop() {
        let mut state = empty_state();
        let call = ParsedToolCall {
            id: "1".into(),
            name: "drop".into(),
            arguments: json!({}),
        };
        let result = apply_model_turn(
            &mut state,
            AssistantTurn::ToolCalls(vec![call]),
            None,
            |call| unknown_tool_result(&call.name),
        );
        assert!(matches!(result, DriveResult::Continue));
        assert!(state.messages[1].text.contains("Unknown tool"));
    }

    #[test]
    fn two_invalid_json_rounds_stop() {
        let mut state = empty_state();
        state.json_mode = true;
        assert!(matches!(
            apply_model_turn(&mut state, AssistantTurn::Invalid, None, |_| String::new()),
            DriveResult::Continue
        ));
        match apply_model_turn(&mut state, AssistantTurn::Invalid, None, |_| String::new()) {
            DriveResult::Failed { message } => assert!(message.contains("tool-calling")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn eighth_tool_sets_force_final() {
        let mut state = empty_state();
        state.tool_rounds = 7;
        let call = ParsedToolCall {
            id: "1".into(),
            name: "get_analysis".into(),
            arguments: json!({}),
        };
        apply_model_turn(
            &mut state,
            AssistantTurn::ToolCalls(vec![call]),
            None,
            |_| "ok".into(),
        );
        assert_eq!(state.tool_rounds, 8);
        assert!(state.force_final);
    }

    #[test]
    fn apply_model_turn_force_final_tool_calls_fail_instead_of_continuing() {
        let mut state = empty_state();
        state.force_final = true;
        let call = ParsedToolCall {
            id: "late".into(),
            name: "get_analysis".into(),
            arguments: json!({}),
        };
        let mut executed = false;
        let result = apply_model_turn(
            &mut state,
            AssistantTurn::ToolCalls(vec![call]),
            None,
            |_| {
                executed = true;
                "should not execute".into()
            },
        );
        match result {
            DriveResult::Failed { message } => {
                assert!(message.contains("final answer"));
                assert!(message.contains("run data"));
            }
            other => panic!("{other:?}"),
        }
        assert!(!executed);
        assert!(state.messages.is_empty());
    }

    #[test]
    fn tool_batch_executes_only_remaining_budget() {
        let mut state = empty_state();
        state.tool_rounds = 7;
        let calls = vec![
            ParsedToolCall {
                id: "first".into(),
                name: "get_analysis".into(),
                arguments: json!({}),
            },
            ParsedToolCall {
                id: "leftover".into(),
                name: "query_sql".into(),
                arguments: json!({"sql": "SELECT 1"}),
            },
        ];
        let mut executed = Vec::new();
        let result = apply_model_turn(&mut state, AssistantTurn::ToolCalls(calls), None, |call| {
            executed.push(call.id.clone());
            "ok".into()
        });
        assert_eq!(result, DriveResult::Continue);
        assert_eq!(executed, vec!["first"]);
        assert_eq!(state.messages.len(), 2);
        assert_eq!(
            state.messages[0]
                .tool_calls
                .as_ref()
                .unwrap()
                .iter()
                .map(|call| call.id.as_str())
                .collect::<Vec<_>>(),
            vec!["first"]
        );
        assert!(state.force_final);
    }

    #[test]
    fn records_only_successfully_fetched_turn_ids() {
        let mut state = empty_state();
        let calls = vec![
            ParsedToolCall {
                id: "number".into(),
                name: "get_turn".into(),
                arguments: json!({"turn_id": 4}),
            },
            ParsedToolCall {
                id: "string".into(),
                name: "get_turn".into(),
                arguments: json!({"turn_id": "5"}),
            },
            ParsedToolCall {
                id: "duplicate".into(),
                name: "get_turn".into(),
                arguments: json!({"turn_id": 4}),
            },
            ParsedToolCall {
                id: "missing-marker".into(),
                name: "get_turn".into(),
                arguments: json!({"turn_id": 6}),
            },
        ];
        apply_model_turn(
            &mut state,
            AssistantTurn::ToolCalls(calls),
            None,
            |call| match call.id.as_str() {
                "number" | "duplicate" => "[turn:4] evidence".into(),
                "string" => "[turn:5] evidence".into(),
                _ => "Step details could not be loaded".into(),
            },
        );
        assert_eq!(state.fetched_turn_ids, vec![4, 5]);
    }

    #[test]
    fn native_invalid_switches_mode_without_incrementing_streak() {
        let mut state = empty_state();
        assert_eq!(
            apply_model_turn(&mut state, AssistantTurn::Invalid, None, |_| String::new()),
            DriveResult::Continue
        );
        assert!(state.json_mode);
        assert_eq!(state.illegal_json_streak, 0);
    }

    #[test]
    fn final_resets_invalid_streak_and_tool_messages_keep_call_metadata() {
        let mut state = empty_state();
        state.illegal_json_streak = 1;
        let call = ParsedToolCall {
            id: "call-7".into(),
            name: "query_sql".into(),
            arguments: json!({"sql": "SELECT 7"}),
        };
        apply_model_turn(
            &mut state,
            AssistantTurn::ToolCalls(vec![call]),
            None,
            |_| "seven".into(),
        );
        assert_eq!(state.messages[0].role, ThreadRole::Assistant);
        assert_eq!(state.messages[1].role, ThreadRole::Tool);
        assert_eq!(state.messages[1].tool_call_id.as_deref(), Some("call-7"));
        assert_eq!(state.messages[1].tool_name.as_deref(), Some("query_sql"));
        assert_eq!(state.messages[1].sql, None);

        assert_eq!(
            apply_model_turn(
                &mut state,
                AssistantTurn::Final("done".into()),
                None,
                |_| { String::new() }
            ),
            DriveResult::Done {
                text: "done".into()
            }
        );
        assert_eq!(state.illegal_json_streak, 0);
    }

    #[test]
    fn context_truncation_stays_on_utf8_boundaries() {
        let (value, truncated) = truncate(&"轨".repeat(100), 32);
        assert!(truncated);
        assert!(value.starts_with("轨轨"));
    }

    fn sample_run(session: &str, run_id: Option<&str>) -> RunSummary {
        RunSummary {
            dataset: "captures".into(),
            file: "events.lance".into(),
            run_id: run_id.map(str::to_string),
            agent_id: "agent".into(),
            model_name: None,
            session_id: session.into(),
            root_session_id: None,
            path: String::new(),
            row_count: 1,
            duplicate_event_ids: 0,
            status: "completed".into(),
        }
    }

    fn tool_msg(text: &str) -> ThreadMessage {
        ThreadMessage {
            role: ThreadRole::Tool,
            text: text.into(),
            tool_calls: None,
            tool_call_id: Some("call-1".into()),
            tool_name: Some("query_sql".into()),
            sql: None,
            truncated: false,
            reasoning_content: None,
        }
    }

    #[test]
    fn thread_key_follows_run_query_and_isolates_sessions() {
        let a = sample_run("s-a", Some("r1"));
        let b = sample_run("s-b", Some("r1"));
        let no_run = sample_run("s-a", None);
        assert_eq!(
            thread_storage_key(&a),
            format!("pchronicle_copilot:{}", a.query())
        );
        assert_ne!(thread_storage_key(&a), thread_storage_key(&b));
        assert_eq!(
            thread_storage_key(&no_run),
            format!("pchronicle_copilot:{}", no_run.query())
        );
        assert!(!thread_storage_key(&no_run).contains("run_id="));
    }

    #[test]
    fn trim_thread_shrinks_oldest_tool_results_first() {
        let mut thread = AssistantThread {
            messages: vec![
                ThreadMessage {
                    role: ThreadRole::User,
                    text: "keep me".into(),
                    tool_calls: None,
                    tool_call_id: None,
                    tool_name: None,
                    sql: None,
                    truncated: false,
                    reasoning_content: None,
                },
                tool_msg(&"x".repeat(180 * 1024)),
                tool_msg(&"y".repeat(180 * 1024)),
                ThreadMessage {
                    role: ThreadRole::Assistant,
                    text: "final".into(),
                    tool_calls: None,
                    tool_call_id: None,
                    tool_name: None,
                    sql: None,
                    truncated: false,
                    reasoning_content: None,
                },
            ],
            updated_at: 1,
            truncated: false,
        };
        trim_thread(&mut thread);
        assert!(thread.truncated);
        assert!(thread_byte_size(&thread) <= THREAD_BYTE_LIMIT);
        assert_eq!(thread.messages[0].text, "keep me");
        assert_eq!(thread.messages[3].text, "final");
        assert!(thread.messages[1].truncated);
        assert!(thread.messages[1].text.len() < 180 * 1024);
    }

    #[test]
    fn compress_messages_for_llm_caps_tool_payload() {
        let messages = vec![
            tool_msg(&"z".repeat(40 * 1024)),
            ThreadMessage {
                role: ThreadRole::User,
                text: "q".into(),
                tool_calls: None,
                tool_call_id: None,
                tool_name: None,
                sql: None,
                truncated: false,
                reasoning_content: None,
            },
        ];
        let compressed = compress_messages_for_llm(&messages);
        let encoded = serde_json::to_string(&compressed).unwrap();
        assert!(encoded.len() <= LLM_MESSAGE_BYTE_LIMIT);
        assert_eq!(compressed.last().unwrap().text, "q");
    }

    #[test]
    fn compress_messages_for_llm_stops_when_tool_payload_cannot_shrink() {
        let unshrinkable_tool = tool_msg(&"t".repeat(200));
        let bulk = ThreadMessage {
            role: ThreadRole::User,
            text: "u".repeat(35 * 1024),
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            sql: None,
            truncated: false,
            reasoning_content: None,
        };
        let messages = vec![unshrinkable_tool, bulk];
        let before = serde_json::to_string(&messages).unwrap();
        assert!(before.len() > LLM_MESSAGE_BYTE_LIMIT);

        let compressed = compress_messages_for_llm(&messages);
        let after = serde_json::to_string(&compressed).unwrap();
        assert!(after.len() <= before.len());
        assert_eq!(compressed[0].text, "t".repeat(200));
    }

    #[test]
    fn native_tool_calls_parse_arguments_string() {
        let message = serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "c1",
                "type": "function",
                "function": {"name": "get_turn", "arguments": "{\"turn_id\":12}"}
            }]
        });
        match parse_native_message(&message) {
            AssistantTurn::ToolCalls(calls) => {
                assert_eq!(calls[0].id, "c1");
                assert_eq!(calls[0].name, "get_turn");
                assert_eq!(calls[0].arguments["turn_id"], 12);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn native_prose_is_final_not_invalid() {
        let message = serde_json::json!({"content": "3 turns, no explicit errors."});
        assert_eq!(
            parse_native_message(&message),
            AssistantTurn::Final("3 turns, no explicit errors.".into())
        );
    }

    #[test]
    fn json_fallback_accepts_tool_and_final() {
        assert!(matches!(
            parse_json_fallback(r#"{"tool":"get_analysis","arguments":{}}"#),
            AssistantTurn::ToolCalls(_)
        ));
        assert_eq!(
            parse_json_fallback("```json\n{\"final\":\"done\"}\n```"),
            AssistantTurn::Final("done".into())
        );
        assert_eq!(parse_json_fallback("not json"), AssistantTurn::Invalid);
    }

    #[test]
    fn json_fallback_tool_call_ids_are_unique() {
        let first = parse_json_fallback(r#"{"tool":"get_analysis","arguments":{}}"#);
        let second = parse_json_fallback(r#"{"tool":"get_analysis","arguments":{}}"#);
        let AssistantTurn::ToolCalls(first) = first else {
            panic!("expected first tool call");
        };
        let AssistantTurn::ToolCalls(second) = second else {
            panic!("expected second tool call");
        };
        assert_ne!(first[0].id, second[0].id);
    }

    #[test]
    fn json_fallback_rejects_empty_final() {
        assert_eq!(
            parse_json_fallback(r#"{"final":""}"#),
            AssistantTurn::Invalid
        );
        assert_eq!(
            parse_json_fallback(r#"{"final":"  \n\t"}"#),
            AssistantTurn::Invalid
        );
    }

    #[test]
    fn openai_messages_precede_native_tool_runs_with_assistant_calls() {
        let messages = vec![
            ThreadMessage {
                role: ThreadRole::User,
                text: "inspect".into(),
                tool_calls: None,
                tool_call_id: None,
                tool_name: None,
                sql: None,
                truncated: false,
                reasoning_content: None,
            },
            ThreadMessage {
                role: ThreadRole::Assistant,
                text: String::new(),
                tool_calls: Some(vec![
                    ParsedToolCall {
                        id: "analysis-1".into(),
                        name: "get_analysis".into(),
                        arguments: json!({}),
                    },
                    ParsedToolCall {
                        id: "turn-1".into(),
                        name: "get_turn".into(),
                        arguments: json!({"turn_id": 4}),
                    },
                ]),
                tool_call_id: None,
                tool_name: None,
                sql: None,
                truncated: false,
                reasoning_content: None,
            },
            ThreadMessage {
                role: ThreadRole::Tool,
                text: "analysis result".into(),
                tool_calls: None,
                tool_call_id: Some("analysis-1".into()),
                tool_name: Some("get_analysis".into()),
                sql: None,
                truncated: false,
                reasoning_content: None,
            },
            ThreadMessage {
                role: ThreadRole::Tool,
                text: "turn result".into(),
                tool_calls: None,
                tool_call_id: Some("turn-1".into()),
                tool_name: Some("get_turn".into()),
                sql: None,
                truncated: false,
                reasoning_content: None,
            },
            ThreadMessage {
                role: ThreadRole::Assistant,
                text: "done".into(),
                tool_calls: None,
                tool_call_id: None,
                tool_name: None,
                sql: None,
                truncated: false,
                reasoning_content: None,
            },
        ];

        let mapped = openai_messages(&messages, false);
        assert_eq!(mapped.len(), 5);
        assert_eq!(mapped[1]["role"], "assistant");
        assert!(mapped[1]["content"].is_null());
        assert_eq!(mapped[1]["tool_calls"][0]["id"], "analysis-1");
        assert_eq!(
            mapped[1]["tool_calls"][0]["function"]["name"],
            "get_analysis"
        );
        assert_eq!(mapped[1]["tool_calls"][0]["function"]["arguments"], "{}");
        assert_eq!(mapped[1]["tool_calls"][1]["id"], "turn-1");
        assert_eq!(mapped[1]["tool_calls"][1]["function"]["name"], "get_turn");
        assert_eq!(
            mapped[1]["tool_calls"][1]["function"]["arguments"],
            r#"{"turn_id":4}"#
        );
        assert_eq!(mapped[1]["reasoning_content"], "");
        assert_eq!(mapped[2]["role"], "tool");
        assert_eq!(mapped[2]["tool_call_id"], "analysis-1");
        assert_eq!(mapped[3]["role"], "tool");
        assert_eq!(mapped[3]["tool_call_id"], "turn-1");
        assert_eq!(mapped[4]["role"], "assistant");
    }

    #[test]
    fn openai_messages_preserve_sequential_tool_call_rounds_and_arguments() {
        let mut state = empty_state();
        apply_model_turn(
            &mut state,
            AssistantTurn::ToolCalls(vec![ParsedToolCall {
                id: "round-1".into(),
                name: "get_analysis".into(),
                arguments: json!({}),
            }]),
            None,
            |_| "analysis".into(),
        );
        apply_model_turn(
            &mut state,
            AssistantTurn::ToolCalls(vec![ParsedToolCall {
                id: "round-2".into(),
                name: "get_turn".into(),
                arguments: json!({"turn_id": 4}),
            }]),
            None,
            |_| "[turn:4] evidence".into(),
        );

        let mapped = openai_messages(&state.messages, false);
        let assistant_tool_calls = mapped
            .iter()
            .filter(|message| {
                message["role"] == "assistant"
                    && message.get("tool_calls").is_some_and(Value::is_array)
            })
            .collect::<Vec<_>>();

        assert_eq!(assistant_tool_calls.len(), 2);
        assert_eq!(assistant_tool_calls[0]["tool_calls"][0]["id"], "round-1");
        assert_eq!(assistant_tool_calls[1]["tool_calls"][0]["id"], "round-2");
        assert_eq!(
            assistant_tool_calls[1]["tool_calls"][0]["function"]["arguments"],
            r#"{"turn_id":4}"#
        );
        assert_eq!(assistant_tool_calls[0]["reasoning_content"], "");
        assert_eq!(assistant_tool_calls[1]["reasoning_content"], "");
    }

    #[test]
    fn openai_messages_echo_thinking_mode_reasoning_on_tool_call_assistants() {
        let mut state = empty_state();
        apply_model_turn(
            &mut state,
            AssistantTurn::ToolCalls(vec![ParsedToolCall {
                id: "round-1".into(),
                name: "get_analysis".into(),
                arguments: json!({}),
            }]),
            Some("need the run summary first".into()),
            |_| "analysis".into(),
        );

        let mapped = openai_messages(&state.messages, false);
        assert_eq!(mapped[0]["role"], "assistant");
        assert_eq!(mapped[0]["reasoning_content"], "need the run summary first");
        assert_eq!(mapped[0]["tool_calls"][0]["id"], "round-1");
    }

    #[test]
    fn extract_reasoning_content_prefers_provider_field() {
        assert_eq!(
            extract_reasoning_content(&json!({
                "role": "assistant",
                "content": null,
                "reasoning_content": "plan the tool call",
                "tool_calls": []
            })),
            Some("plan the tool call".into())
        );
        assert_eq!(
            extract_reasoning_content(&json!({"reasoning": "alt"})),
            Some("alt".into())
        );
        assert_eq!(extract_reasoning_content(&json!({"content": "hi"})), None);
    }

    #[test]
    fn openai_messages_map_json_mode_tools_as_text() {
        let mapped = openai_messages(
            &[
                ThreadMessage {
                    role: ThreadRole::Assistant,
                    text: String::new(),
                    tool_calls: Some(vec![ParsedToolCall {
                        id: "sql-1".into(),
                        name: "query_sql".into(),
                        arguments: json!({"sql": "SELECT 1"}),
                    }]),
                    tool_call_id: None,
                    tool_name: None,
                    sql: None,
                    truncated: false,
                    reasoning_content: None,
                },
                ThreadMessage {
                    role: ThreadRole::Tool,
                    text: "three rows".into(),
                    tool_calls: None,
                    tool_call_id: Some("sql-1".into()),
                    tool_name: Some("query_sql".into()),
                    sql: Some("SELECT 1".into()),
                    truncated: false,
                    reasoning_content: None,
                },
            ],
            true,
        );

        assert_eq!(
            mapped,
            vec![
                json!({
                    "role": "assistant",
                    "content": r#"{"arguments":{"sql":"SELECT 1"},"tool":"query_sql"}"#
                }),
                json!({
                    "role": "user",
                    "content": "Tool query_sql result:\nthree rows"
                })
            ]
        );
        assert!(
            mapped
                .iter()
                .all(|message| message.get("tool_calls").is_none())
        );
        assert!(mapped.iter().all(|message| message["role"] != "tool"));
    }

    #[test]
    fn tools_unsupported_requires_protocol_keyword_in_client_error() {
        let unknown_model = llm::CompletionError {
            status: Some(400),
            message: "LLM HTTP 400: unknown model".into(),
        };
        let tool_choice = llm::CompletionError {
            status: Some(400),
            message: "LLM HTTP 400: unsupported tool_choice".into(),
        };
        assert!(!unknown_model.suggests_tools_unsupported());
        assert!(tool_choice.suggests_tools_unsupported());
    }

    #[test]
    fn native_tool_calls_reject_malformed_entries() {
        let missing_name = serde_json::json!({
            "tool_calls": [{
                "id": "c1",
                "function": {"arguments": "{}"}
            }]
        });
        assert_eq!(parse_native_message(&missing_name), AssistantTurn::Invalid);

        let bad_arguments = serde_json::json!({
            "tool_calls": [{
                "id": "c1",
                "function": {"name": "get_turn", "arguments": "not-json"}
            }]
        });
        assert_eq!(parse_native_message(&bad_arguments), AssistantTurn::Invalid);
    }

    #[test]
    fn native_tool_calls_accept_object_arguments() {
        let message = serde_json::json!({
            "tool_calls": [{
                "id": "c1",
                "function": {"name": "get_turn", "arguments": {"turn_id": 12}}
            }]
        });
        match parse_native_message(&message) {
            AssistantTurn::ToolCalls(calls) => {
                assert_eq!(calls[0].name, "get_turn");
                assert_eq!(calls[0].arguments["turn_id"], 12);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn native_empty_content_without_tool_calls_is_invalid() {
        let message = serde_json::json!({"content": ""});
        assert_eq!(parse_native_message(&message), AssistantTurn::Invalid);
    }

    #[test]
    fn native_non_array_tool_calls_with_content_is_invalid() {
        let string_tool_calls = serde_json::json!({
            "tool_calls": "not-an-array",
            "content": "3 turns, no explicit errors."
        });
        assert_eq!(
            parse_native_message(&string_tool_calls),
            AssistantTurn::Invalid
        );

        let object_tool_calls = serde_json::json!({
            "tool_calls": {"id": "c1"},
            "content": "3 turns, no explicit errors."
        });
        assert_eq!(
            parse_native_message(&object_tool_calls),
            AssistantTurn::Invalid
        );
    }

    #[test]
    fn native_empty_tool_calls_falls_through_to_content() {
        let message = serde_json::json!({
            "tool_calls": [],
            "content": "done"
        });
        assert_eq!(
            parse_native_message(&message),
            AssistantTurn::Final("done".into())
        );
    }

    #[test]
    fn native_deepseek_dsml_tool_calls_are_parsed_from_content() {
        let content = concat!(
            "<think>need a few turns</think>\n",
            "<｜DSML｜tool_calls>\n",
            "<｜DSML｜invoke name=\"get_turn\">\n",
            "<｜DSML｜parameter name=\"turn_id\" string=\"false\">16</｜DSML｜parameter>\n",
            "</｜DSML｜invoke>\n",
            "<｜DSML｜invoke name=\"get_turn\">\n",
            "<｜DSML｜parameter name=\"turn_id\" string=\"false\">18</｜DSML｜parameter>\n",
            "</｜DSML｜invoke>\n",
            "</｜DSML｜tool_calls>"
        );
        match parse_native_message(&json!({"content": content})) {
            AssistantTurn::ToolCalls(calls) => {
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0].name, "get_turn");
                assert_eq!(calls[0].arguments["turn_id"], 16);
                assert_eq!(calls[1].arguments["turn_id"], 18);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn native_deepseek_dsml_parses_string_and_ascii_tokens() {
        let content = concat!(
            "<|DSML|tool_calls>\n",
            "<|DSML|invoke name=\"query_sql\">\n",
            "<|DSML|parameter name=\"sql\" string=\"true\">SELECT 1</|DSML|parameter>\n",
            "</|DSML|invoke>\n",
            "</|DSML|tool_calls>"
        );
        match parse_native_message(&json!({"content": content})) {
            AssistantTurn::ToolCalls(calls) => {
                assert_eq!(calls[0].name, "query_sql");
                assert_eq!(calls[0].arguments["sql"], "SELECT 1");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn native_openai_tool_calls_win_over_dsml_content() {
        let message = json!({
            "content": "<｜DSML｜tool_calls><｜DSML｜invoke name=\"get_turn\"></｜DSML｜invoke></｜DSML｜tool_calls>",
            "tool_calls": [{
                "id": "c1",
                "function": {"name": "get_analysis", "arguments": "{}"}
            }]
        });
        match parse_native_message(&message) {
            AssistantTurn::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "get_analysis");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn native_broken_dsml_tool_calls_are_invalid() {
        let content = "<｜DSML｜tool_calls><｜DSML｜invoke name=\"get_turn\">";
        assert_eq!(
            parse_native_message(&json!({"content": content})),
            AssistantTurn::Invalid
        );
    }

    #[test]
    fn native_spaced_dsml_fragment_is_parsed_as_tool_calls() {
        let content = concat!(
            "</ | | DSML | | invoke>\n",
            "< | | DSML | | invoke name=\"get_turn\">\n",
            "< | | DSML | | parameter name=\"turn_id\" string=\"false\">13</ | | DSML | | parameter>\n",
            "</ | | DSML | | invoke>\n",
            "</ | | DSML | | tool_calls>"
        );
        match parse_native_message(&json!({"content": content})) {
            AssistantTurn::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "get_turn");
                assert_eq!(calls[0].arguments["turn_id"], 13);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(visible_assistant_text(content), None);
    }

    #[test]
    fn native_dsml_function_calls_wrapper_is_accepted() {
        let content = concat!(
            "<｜DSML｜function_calls>\n",
            "<｜DSML｜invoke name=\"get_analysis\">\n",
            "</｜DSML｜invoke>\n",
            "</｜DSML｜function_calls>"
        );
        match parse_native_message(&json!({"content": content})) {
            AssistantTurn::ToolCalls(calls) => {
                assert_eq!(calls[0].name, "get_analysis");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn visible_assistant_text_keeps_prose_and_strips_dsml() {
        let mixed = concat!(
            "Redundant retries appear after turn 7.\n",
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"get_turn\">",
            "<｜DSML｜parameter name=\"turn_id\" string=\"false\">7</｜DSML｜parameter>",
            "</｜DSML｜invoke></｜DSML｜tool_calls>"
        );
        let visible = visible_assistant_text(mixed).expect("prose remains");
        assert!(visible.contains("Redundant retries"));
        assert!(!visible.contains("DSML"));
        assert_eq!(
            visible_assistant_text("No markup here."),
            Some("No markup here.".into())
        );
    }

    #[test]
    fn native_tool_calls_reject_non_object_arguments() {
        let array_args = serde_json::json!({
            "tool_calls": [{
                "id": "c1",
                "function": {"name": "get_turn", "arguments": [1, 2]}
            }]
        });
        assert_eq!(parse_native_message(&array_args), AssistantTurn::Invalid);

        let number_args = serde_json::json!({
            "tool_calls": [{
                "id": "c1",
                "function": {"name": "get_turn", "arguments": 42}
            }]
        });
        assert_eq!(parse_native_message(&number_args), AssistantTurn::Invalid);

        let string_array_args = serde_json::json!({
            "tool_calls": [{
                "id": "c1",
                "function": {"name": "get_turn", "arguments": "[1,2]"}
            }]
        });
        assert_eq!(
            parse_native_message(&string_array_args),
            AssistantTurn::Invalid
        );
    }

    #[test]
    fn json_fallback_rejects_non_object_arguments() {
        assert_eq!(
            parse_json_fallback(r#"{"tool":"get_analysis","arguments":[]}"#),
            AssistantTurn::Invalid
        );
        assert_eq!(
            parse_json_fallback(r#"{"tool":"get_turn","arguments":42}"#),
            AssistantTurn::Invalid
        );
    }

    use crate::model::{MetricStats, StorylineTurn};

    fn stats() -> MetricStats {
        MetricStats {
            sample_count: 1,
            total_count: 3,
            p50: None,
            p95: None,
            max: None,
        }
    }

    fn sample_analysis() -> RunAnalysis {
        RunAnalysis {
            run: sample_run("s-a", Some("r1")),
            event_provenance: crate::model::EventProvenance::Canonical,
            event_count: 3,
            turn_count: 3,
            tool_call_count: 0,
            error_count: 0,
            start_timestamp: None,
            end_timestamp: None,
            models: Vec::new(),
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            latency_ms: stats(),
            ttft_ms: stats(),
            latency_histogram: Vec::new(),
            source_breakdown: Vec::new(),
            kind_breakdown: Vec::new(),
            model_breakdown: Vec::new(),
            tools: Vec::new(),
        }
    }

    fn sample_detail(message: Value) -> TurnDetail {
        TurnDetail {
            summary: TurnSummary {
                id: 9,
                source: "agent".into(),
                kind: None,
                timestamp: None,
                call_id: None,
                preview: String::new(),
                user_prompt: None,
                char_count: 0,
                modalities: Vec::new(),
                model_name: None,
                latency_ms: None,
                ttft_ms: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                tool_names: Vec::new(),
                event_seqs: Vec::new(),
                has_error: false,
            },
            turn: StorylineTurn {
                id: 9,
                kind: None,
                timestamp: None,
                source: "agent".into(),
                message,
                reasoning_content: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                model_name: None,
                latency_ms: None,
                ttft_ms: None,
                extra: None,
            },
            wire_tool_calls: Vec::new(),
            event_provenance: crate::model::EventProvenance::Canonical,
            events: Vec::new(),
        }
    }

    #[test]
    fn turn_formatter_truncates_on_utf8_boundary() {
        let detail = sample_detail(Value::String("轨".repeat(20_000)));
        let text = format_turn_result(&detail);
        assert!(text.contains("[… truncated …]"));
        assert!(text.is_char_boundary(text.find("[… truncated …]").unwrap()));
    }

    #[test]
    fn unknown_tool_is_an_error_string() {
        let text = unknown_tool_result("drop_table");
        assert!(text.contains("drop_table"));
        assert!(text.to_ascii_lowercase().contains("unknown"));
    }

    #[test]
    fn analysis_formatter_omits_turn_bodies() {
        let text = format_analysis_result(&sample_analysis());
        assert!(text.contains("turns=3"));
        assert!(!text.contains("preview"));
    }
}

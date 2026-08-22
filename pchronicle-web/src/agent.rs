use std::cmp::Ordering;

use gloo_net::http::Request;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api;
use crate::components::{table_fence, trajectory_fence};
use crate::model::{RunAnalysis, RunSummary, TurnDetail, TurnSummary};

const STORAGE_KEY: &str = "pchronicle_llm_config";
const DEFAULT_CONTEXT_LIMIT: usize = 32 * 1024;
const FULL_CONTEXT_LIMIT: usize = 64 * 1024;

pub const THREAD_BYTE_LIMIT: usize = 200 * 1024;
pub const LLM_MESSAGE_BYTE_LIMIT: usize = 32 * 1024;

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
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CopilotThread {
    pub messages: Vec<ThreadMessage>,
    pub updated_at: i64,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LlmConfig {
    pub api_base: String,
    pub api_key: String,
    pub model: String,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_base: "https://api.deepseek.com/v1".into(),
            api_key: String::new(),
            model: "deepseek-chat".into(),
        }
    }
}

impl LlmConfig {
    pub fn is_configured(&self) -> bool {
        !self.api_base.trim().is_empty()
            && !self.api_key.trim().is_empty()
            && !self.model.trim().is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentAnswer {
    pub text: String,
    pub action: String,
    pub sql: Option<String>,
    pub truncated: bool,
}

pub struct AnswerRequest<'a> {
    pub config: &'a LlmConfig,
    pub user_message: &'a str,
    pub run: &'a RunSummary,
    pub analysis: &'a RunAnalysis,
    pub turns: &'a [TurnSummary],
    pub selected: Option<&'a TurnDetail>,
    pub include_full_turn: bool,
}

#[derive(Debug, Deserialize)]
struct Selection {
    action: String,
    skill_id: Option<String>,
    sql: Option<String>,
    #[serde(default)]
    reply: String,
}

pub fn load_config() -> LlmConfig {
    let Some(window) = web_sys::window() else {
        return LlmConfig::default();
    };
    let Some(storage) = window.local_storage().ok().flatten() else {
        return LlmConfig::default();
    };
    storage
        .get_item(STORAGE_KEY)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save_config(config: &LlmConfig) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(storage) = window.local_storage().ok().flatten() else {
        return;
    };
    if let Ok(raw) = serde_json::to_string(config) {
        let _ = storage.set_item(STORAGE_KEY, &raw);
    }
}

pub fn thread_storage_key(run: &RunSummary) -> String {
    format!("pchronicle_copilot:{}", run.query())
}

pub fn thread_byte_size(thread: &CopilotThread) -> usize {
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

pub fn trim_thread(thread: &mut CopilotThread) {
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

pub fn skill_ids() -> &'static [&'static str] {
    &[
        "trajectory_summary",
        "failure_locator",
        "latency_hotspots",
        "tool_usage",
        "cohort_compare",
    ]
}

pub async fn answer(request: AnswerRequest<'_>) -> Result<AgentAnswer, String> {
    let AnswerRequest {
        config,
        user_message,
        run,
        analysis,
        turns,
        selected,
        include_full_turn,
    } = request;
    let (base_context, context_truncated) =
        evidence_context(run, analysis, turns, selected, include_full_turn);
    let explicit_skill = resolve_skill(user_message);
    if !config.is_configured() {
        let skill = explicit_skill.unwrap_or("trajectory_summary");
        let (evidence, sql) = run_skill(skill, run, analysis, turns).await?;
        let evidence = decorate_skill_evidence(skill, evidence, turns);
        return Ok(AgentAnswer {
            text: format!(
                "**{}**\n\n{}\n\nConfigure an OpenAI-compatible model in Settings for a natural-language interpretation.",
                skill_title(skill),
                evidence
            ),
            action: skill.into(),
            sql,
            truncated: context_truncated,
        });
    }

    let selection = if let Some(skill) = explicit_skill {
        Selection {
            action: "skill".into(),
            skill_id: Some(skill.into()),
            sql: None,
            reply: String::new(),
        }
    } else {
        select_action(config, user_message, &base_context).await?
    };

    match selection.action.as_str() {
        "sql" => {
            let sql = selection
                .sql
                .filter(|sql| !sql.trim().is_empty())
                .ok_or_else(|| "The model selected SQL without returning a query.".to_string())?;
            let result = api::query_evidence(&sql).await?;
            let evidence = format!(
                "SQL:\n{sql}\n\nreturned_rows={} truncated={}\n{}",
                result.returned_rows,
                result.truncated,
                serde_json::to_string_pretty(&result.rows).unwrap_or_default()
            );
            let summary = summarize(config, user_message, &base_context, &evidence).await?;
            let component = table_fence("SQL query result", result.clone());
            Ok(AgentAnswer {
                text: format!("{summary}\n\n{component}"),
                action: "read-only SQL".into(),
                sql: Some(sql),
                truncated: context_truncated || result.truncated,
            })
        }
        "answer" => Ok(AgentAnswer {
            text: if selection.reply.trim().is_empty() {
                "I could not map that request to available trajectory evidence.".into()
            } else {
                selection.reply
            },
            action: "context answer".into(),
            sql: None,
            truncated: context_truncated,
        }),
        _ => {
            let skill = selection
                .skill_id
                .as_deref()
                .filter(|skill| skill_ids().contains(skill))
                .unwrap_or("trajectory_summary");
            let (evidence, sql) = run_skill(skill, run, analysis, turns).await?;
            let evidence = decorate_skill_evidence(skill, evidence, turns);
            let components = component_fences(&evidence);
            let summary = summarize(config, user_message, &base_context, &evidence).await?;
            Ok(AgentAnswer {
                text: if components.is_empty() {
                    summary
                } else {
                    format!("{summary}\n\n{components}")
                },
                action: skill.into(),
                sql,
                truncated: context_truncated,
            })
        }
    }
}

async fn run_skill(
    skill: &str,
    run: &RunSummary,
    analysis: &RunAnalysis,
    turns: &[TurnSummary],
) -> Result<(String, Option<String>), String> {
    match skill {
        "failure_locator" => {
            let evidence = turns
                .iter()
                .filter(|turn| turn.has_error)
                .take(20)
                .map(|turn| {
                    format!(
                        "- [turn:{}] {} {} — {}",
                        turn.id,
                        turn.source,
                        turn.kind.as_deref().unwrap_or("unknown"),
                        turn.preview
                    )
                })
                .collect::<Vec<_>>();
            Ok((
                if evidence.is_empty() {
                    "No turns contain an explicit error kind, failing status, non-null error_type, or HTTP status >= 400. This does not prove the run succeeded.".into()
                } else {
                    format!("Explicit error evidence:\n{}", evidence.join("\n"))
                },
                None,
            ))
        }
        "latency_hotspots" => {
            let mut ranked = turns
                .iter()
                .filter_map(|turn| turn.latency_ms.map(|latency| (turn, latency)))
                .collect::<Vec<_>>();
            ranked.sort_by(|left, right| right.1.partial_cmp(&left.1).unwrap_or(Ordering::Equal));
            let lines = ranked
                .into_iter()
                .take(20)
                .map(|(turn, latency)| {
                    format!("- [turn:{}] {:.1} ms — {}", turn.id, latency, turn.preview)
                })
                .collect::<Vec<_>>();
            Ok((
                format!(
                    "Latency coverage: {}/{} turns; P50={}; P95={}; max={}\n{}",
                    analysis.latency_ms.sample_count,
                    analysis.latency_ms.total_count,
                    optional_number(analysis.latency_ms.p50),
                    optional_number(analysis.latency_ms.p95),
                    optional_number(analysis.latency_ms.max),
                    if lines.is_empty() {
                        "No captured latency samples.".into()
                    } else {
                        lines.join("\n")
                    }
                ),
                None,
            ))
        }
        "tool_usage" => Ok((
            if analysis.tools.is_empty() {
                "No structured tool calls were captured.".into()
            } else {
                analysis
                    .tools
                    .iter()
                    .map(|tool| {
                        format!(
                            "- {}: {} calls, duration coverage {}/{}, total {}, average {}, max {}, error-associated {}",
                            tool.name,
                            tool.count,
                            tool.duration_sample_count,
                            tool.count,
                            optional_number(tool.total_duration_ms),
                            optional_number(tool.average_duration_ms),
                            optional_number(tool.max_duration_ms),
                            tool.error_associated_count,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            None,
        )),
        "cohort_compare" => {
            let catalog = api::query_catalog().await?;
            let database = catalog.database;
            let session = sql_literal(&run.session_id);
            let sql = format!(
                "SELECT session_id, COUNT(*) AS step_count, AVG(latency_ms) AS avg_latency_ms, MAX(latency_ms) AS max_latency_ms FROM {database}.steps GROUP BY session_id ORDER BY avg_latency_ms DESC NULLS LAST LIMIT 50"
            );
            let result = api::query_evidence(&sql).await?;
            let component = table_fence("Cohort comparison", result.clone());
            Ok((
                format!(
                    "Selected session: {session}\nCohort rows={} truncated={}\n\n{}",
                    result.returned_rows, result.truncated, component
                ),
                Some(sql),
            ))
        }
        _ => Ok((overview_evidence(run, analysis, turns), None)),
    }
}

fn decorate_skill_evidence(skill: &str, evidence: String, turns: &[TurnSummary]) -> String {
    let mut selected = match skill {
        "failure_locator" => turns
            .iter()
            .filter(|turn| turn.has_error)
            .map(|turn| turn.id)
            .take(20)
            .collect::<Vec<_>>(),
        "latency_hotspots" => {
            let mut ranked = turns
                .iter()
                .filter_map(|turn| turn.latency_ms.map(|latency| (turn.id, latency)))
                .collect::<Vec<_>>();
            ranked.sort_by(|left, right| right.1.partial_cmp(&left.1).unwrap_or(Ordering::Equal));
            ranked.into_iter().map(|(id, _)| id).take(20).collect()
        }
        "tool_usage" => turns
            .iter()
            .filter(|turn| !turn.tool_names.is_empty())
            .map(|turn| turn.id)
            .take(20)
            .collect(),
        "trajectory_summary" => turns.iter().map(|turn| turn.id).take(20).collect(),
        _ => Vec::new(),
    };
    selected.dedup();
    if selected.is_empty() {
        evidence
    } else {
        format!(
            "{evidence}\n\n{}",
            trajectory_fence(skill_title(skill), selected)
        )
    }
}

fn component_fences(value: &str) -> String {
    let lines = value.lines().collect::<Vec<_>>();
    let mut fences = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index].starts_with("```pchronicle:") {
            let start = index;
            index += 1;
            while index < lines.len() && lines[index] != "```" {
                index += 1;
            }
            if index < lines.len() {
                fences.push(lines[start..=index].join("\n"));
            }
        }
        index += 1;
    }
    fences.join("\n\n")
}

fn overview_evidence(run: &RunSummary, analysis: &RunAnalysis, turns: &[TurnSummary]) -> String {
    let top = turns
        .iter()
        .take(12)
        .map(|turn| format!("- [turn:{}] {} — {}", turn.id, turn.source, turn.preview))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Run: agent={} session={} status={}\nEvents={} turns={} tools={} explicit_errors={}\nTokens: prompt={} completion={} total={}\nLatency: samples={}/{} p50={} p95={} max={}\nTurn evidence:\n{}",
        run.agent_id,
        run.session_id,
        run.status,
        analysis.event_count,
        analysis.turn_count,
        analysis.tool_call_count,
        analysis.error_count,
        optional_u64(analysis.prompt_tokens),
        optional_u64(analysis.completion_tokens),
        optional_u64(analysis.total_tokens),
        analysis.latency_ms.sample_count,
        analysis.latency_ms.total_count,
        optional_number(analysis.latency_ms.p50),
        optional_number(analysis.latency_ms.p95),
        optional_number(analysis.latency_ms.max),
        top
    )
}

fn evidence_context(
    run: &RunSummary,
    analysis: &RunAnalysis,
    turns: &[TurnSummary],
    selected: Option<&TurnDetail>,
    include_full_turn: bool,
) -> (String, bool) {
    let mut context = overview_evidence(run, analysis, turns);
    if let Some(detail) = selected {
        context.push_str(&format!(
            "\nSelected [turn:{}]: source={} kind={} model={} latency={} tools={}\n",
            detail.summary.id,
            detail.summary.source,
            detail.summary.kind.as_deref().unwrap_or("unknown"),
            detail
                .summary
                .model_name
                .as_deref()
                .unwrap_or("unavailable"),
            optional_number(detail.summary.latency_ms),
            detail.summary.tool_names.join(", ")
        ));
        if detail.summary.source != "system" {
            let text = detail.turn.text();
            let excerpt_limit = if include_full_turn {
                FULL_CONTEXT_LIMIT
            } else {
                4 * 1024
            };
            context.push_str("Selected content:\n");
            context.push_str(&truncate(&text, excerpt_limit).0);
        } else {
            context.push_str("System content omitted by the minimal-evidence policy.");
        }
        if include_full_turn {
            context.push_str("\nTool calls:\n");
            context.push_str(
                &truncate(
                    &serde_json::to_string_pretty(&detail.wire_tool_calls).unwrap_or_default(),
                    12 * 1024,
                )
                .0,
            );
        }
    }
    let limit = if include_full_turn {
        FULL_CONTEXT_LIMIT
    } else {
        DEFAULT_CONTEXT_LIMIT
    };
    truncate(&context, limit)
}

async fn select_action(
    config: &LlmConfig,
    user_message: &str,
    context: &str,
) -> Result<Selection, String> {
    let catalog = api::query_catalog().await.ok();
    let database = catalog
        .as_ref()
        .map(|catalog| catalog.database.as_str())
        .unwrap_or("data");
    let system = format!(
        "You are pChronicle Copilot for local agent trajectory debugging. Select exactly one action. Return JSON only: {{\"action\":\"skill|sql|answer\",\"skill_id\":\"trajectory_summary|failure_locator|latency_hotspots|tool_usage|cohort_compare|null\",\"sql\":null,\"reply\":\"\"}}. For SQL, emit exactly one read-only SELECT/WITH/EXPLAIN over {database}.runs, {database}.steps, {database}.tool_calls, or {database}.trajectories. Prefer a built-in skill. Never claim missing data is zero and never infer an error from arbitrary message text.\n\nWorkspace evidence:\n{context}"
    );
    let text = chat(config, &system, user_message, true).await?;
    serde_json::from_str(extract_json(&text))
        .map_err(|error| format!("The model returned invalid routing JSON: {error}"))
}

async fn summarize(
    config: &LlmConfig,
    question: &str,
    context: &str,
    evidence: &str,
) -> Result<String, String> {
    let system = "You are pChronicle Copilot. Answer in the user's language in 3-7 concise bullets. Separate captured facts from inference. Cite relevant turns using the exact form [turn:ID]. Mention coverage and truncation when present. Do not invent costs, errors, or missing measurements.";
    let user = format!(
        "Question: {question}\n\nMinimal workspace context:\n{context}\n\nExecuted evidence:\n{evidence}"
    );
    chat(config, system, &user, false).await
}

async fn chat(
    config: &LlmConfig,
    system: &str,
    user: &str,
    json_mode: bool,
) -> Result<String, String> {
    let url = format!(
        "{}/chat/completions",
        config.api_base.trim().trim_end_matches('/')
    );
    let mut body = json!({
        "model": config.model.trim(),
        "temperature": if json_mode { 0.1 } else { 0.3 },
        "messages": [
            {"role":"system","content":system},
            {"role":"user","content":user}
        ]
    });
    if json_mode {
        body["response_format"] = json!({"type":"json_object"});
    }
    let response = Request::post(&url)
        .header(
            "Authorization",
            &format!("Bearer {}", config.api_key.trim()),
        )
        .header("Content-Type", "application/json")
        .json(&body)
        .map_err(|error| error.to_string())?
        .send()
        .await
        .map_err(|error| format!("LLM request failed (check API base, key, and CORS): {error}"))?;
    let status = response.status();
    let value: Value = response.json().await.map_err(|error| error.to_string())?;
    if !(200..300).contains(&status) {
        return Err(format!("LLM HTTP {status}: {value}"));
    }
    value["choices"][0]["message"]["content"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "LLM returned an empty response".into())
}

fn resolve_skill(message: &str) -> Option<&'static str> {
    let normalized = message.trim().trim_start_matches('/').to_ascii_lowercase();
    skill_ids().iter().copied().find(|skill| {
        normalized == *skill
            || normalized.starts_with(&format!("{skill} "))
            || match *skill {
                "failure_locator" => normalized.contains("fail") || normalized.contains("error"),
                "latency_hotspots" => normalized.contains("slow") || normalized.contains("latency"),
                "tool_usage" => normalized.contains("tool"),
                "cohort_compare" => normalized.contains("compare") || normalized.contains("cohort"),
                _ => false,
            }
    })
}

fn skill_title(skill: &str) -> &'static str {
    match skill {
        "failure_locator" => "Failure locator",
        "latency_hotspots" => "Latency hotspots",
        "tool_usage" => "Tool usage",
        "cohort_compare" => "Cohort compare",
        _ => "Trajectory summary",
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
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        if calls.is_empty() {
            // fall through to content
        } else {
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
    }
    match message.get("content").and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => AssistantTurn::Final(text.to_string()),
        _ => AssistantTurn::Invalid,
    }
}

pub fn parse_json_fallback(content: &str) -> AssistantTurn {
    let raw = extract_json(content);
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return AssistantTurn::Invalid;
    };
    if let Some(final_text) = value.get("final").and_then(Value::as_str) {
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
        id: "json-0".into(),
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

fn optional_number(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1}"))
        .unwrap_or_else(|| "unavailable".into())
}

fn optional_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unavailable".into())
}

fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_truncation_stays_on_utf8_boundaries() {
        let (value, truncated) = truncate(&"轨".repeat(100), 32);
        assert!(truncated);
        assert!(value.starts_with("轨轨"));
    }

    #[test]
    fn explicit_commands_resolve_to_known_skills() {
        assert_eq!(resolve_skill("/latency_hotspots"), Some("latency_hotspots"));
        assert_eq!(resolve_skill("compare this cohort"), Some("cohort_compare"));
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
            error_count: 0,
        }
    }

    fn tool_msg(text: &str) -> ThreadMessage {
        ThreadMessage {
            role: ThreadRole::Tool,
            text: text.into(),
            tool_call_id: Some("call-1".into()),
            tool_name: Some("query_sql".into()),
            sql: None,
            truncated: false,
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
        let mut thread = CopilotThread {
            messages: vec![
                ThreadMessage {
                    role: ThreadRole::User,
                    text: "keep me".into(),
                    tool_call_id: None,
                    tool_name: None,
                    sql: None,
                    truncated: false,
                },
                tool_msg(&"x".repeat(180 * 1024)),
                tool_msg(&"y".repeat(180 * 1024)),
                ThreadMessage {
                    role: ThreadRole::Assistant,
                    text: "final".into(),
                    tool_call_id: None,
                    tool_name: None,
                    sql: None,
                    truncated: false,
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
                tool_call_id: None,
                tool_name: None,
                sql: None,
                truncated: false,
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
            tool_call_id: None,
            tool_name: None,
            sql: None,
            truncated: false,
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
}

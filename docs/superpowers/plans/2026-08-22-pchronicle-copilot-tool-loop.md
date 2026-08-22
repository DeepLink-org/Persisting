# pChronicle Copilot Tool-Calling Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Copilot one-shot skill router with a browser BYOK tool-calling loop that can fetch analysis, turn detail, and read-only SQL for the open run, persist the thread per run, and cite turns back into Trace.

**Architecture:** Keep inference and the API key in the browser. `agent.rs` owns a sync loop driver plus three tools pinned to the current `RunSummary`. Native OpenAI `tool_calls` is the main path; the same driver falls back to `{tool}` / `{final}` JSON. `CopilotPanel` stores `CopilotThread` in `localStorage` under `pchronicle_copilot:` + `RunSummary::query()`. Warehouse HTTP stays read-only; no new Copilot server endpoint.

**Tech Stack:** Dioxus 0.7, Rust unit tests in `pchronicle-web`, `gloo_net` to OpenAI-compatible `/chat/completions` and existing `/api/explorer/*` + `/api/query/evidence`.

**Spec:** `docs/superpowers/specs/2026-08-22-pchronicle-copilot-tool-loop-design.md`

## Global Constraints

- Browser BYOK only; the pChronicle server never receives the key.
- Tools cannot change dataset / file / run_id / agent_id / session_id.
- `query_sql` uses `api::query_evidence` (50 rows / 64 KiB). Do not use `query_evidence_interactive`.
- Do not parse SQL in the frontend; rely on server read-only validation.
- Max 8 tool executions per user message; turn body 8 KiB; thread 200 KiB; LLM messages 32 KiB.
- Delete the five skills and `select_action`. Do not keep `/skill_id` chips or a compatibility shim.
- Do not add streaming, server Copilot sessions, cohort compare, or workspace actions (tab / filter changes).
- Keep TTAS, Queue, Search, and `persisting-dlcapt` out of scope.
- Test with `cargo test --manifest-path pchronicle-web/Cargo.toml --locked --offline`. Do not call a real LLM in tests.

---

## File map

- Modify `pchronicle-web/src/agent.rs`: thread types, parse/drive loop, tool formatters, HTTP `answer()`, delete skill router.
- Modify `pchronicle-web/src/workspace.rs`: `CopilotPanel` persistence, step label, Enter submit, drop chips and selected-turn checkbox; render `ThreadMessage`.
- Unchanged: `pchronicle-web/src/api.rs` (`turn_detail`, `run_analysis`, `query_evidence`), `trajectory_fence` / `parse_rich_blocks` / `[turn:ID]` citations.

---

### Task 1: Per-run thread key and size trim

**Files:**
- Modify: `pchronicle-web/src/agent.rs`
- Test: `pchronicle-web/src/agent.rs` (`mod tests`)

**Interfaces:**
- Consumes: `RunSummary::query()`
- Produces:

```rust
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

pub fn thread_storage_key(run: &RunSummary) -> String;
pub fn thread_byte_size(thread: &CopilotThread) -> usize;
pub fn trim_thread(thread: &mut CopilotThread);
pub fn compress_messages_for_llm(messages: &[ThreadMessage]) -> Vec<ThreadMessage>;
```

- [ ] **Step 1: Write the failing tests**

Append to `agent.rs` tests. Reuse a local `RunSummary` fixture (do not import `workspace` helpers):

```rust
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
    assert_eq!(thread_storage_key(&a), format!("pchronicle_copilot:{}", a.query()));
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path pchronicle-web/Cargo.toml --locked --offline thread_key_follows_run_query -- --exact`

Expected: compile error, `thread_storage_key` not found.

- [ ] **Step 3: Write minimal implementation**

Keep existing `LlmConfig` / `load_config` / `save_config`. Add the types and:

```rust
pub fn thread_storage_key(run: &RunSummary) -> String {
    format!("pchronicle_copilot:{}", run.query())
}

pub fn thread_byte_size(thread: &CopilotThread) -> usize {
    serde_json::to_string(thread).map(|raw| raw.len()).unwrap_or(0)
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
        let Some(index) = out
            .iter()
            .position(|message| message.role == ThreadRole::Tool && message.text.len() > 64)
        else {
            return out;
        };
        out[index].text = shrink_tool_text(&out[index].text);
        out[index].truncated = true;
    }
}
```

If a 180 KiB + 180 KiB pair still exceeds 200 KiB after one shrink, the `while` loop keeps shrinking later tool messages. That is intended.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path pchronicle-web/Cargo.toml --locked --offline -- thread_key_follows_run_query trim_thread_shrinks compress_messages_for_llm`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add pchronicle-web/src/agent.rs
git commit -m "$(cat <<'EOF'
feat(pchronicle-web): persist Copilot threads per run key

Give Copilot a stable localStorage key and trim oldest tool payloads so a long thread cannot blow the browser quota.

EOF
)"
```

---

### Task 2: Parse native tool_calls and JSON fallback

**Files:**
- Modify: `pchronicle-web/src/agent.rs`
- Test: `pchronicle-web/src/agent.rs`

**Interfaces:**
- Consumes: Task 1 types
- Produces:

```rust
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

pub fn parse_native_message(message: &Value) -> AssistantTurn;
pub fn parse_json_fallback(content: &str) -> AssistantTurn;
```

Rules:

- Native: if `message.tool_calls` is a non-empty array, parse `{id, function.name, function.arguments}`. `arguments` may be a JSON string or object. Malformed entries → `Invalid`.
- Native: no `tool_calls` and non-empty `content` → `Final(content)`. Empty content and no tool_calls → `Invalid` (caller may switch to JSON mode).
- JSON fallback: parse an object (raw or fenced ````json`). `{ "final": "..." }` → `Final`. `{ "tool": "get_analysis"|"get_turn"|"query_sql", "arguments": {} }` → one `ToolCalls`. Anything else → `Invalid`.
- A prose answer in native mode is `Final`, not fallback. Do not treat ordinary assistant text as JSON failure.

- [ ] **Step 1: Write the failing tests**

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path pchronicle-web/Cargo.toml --locked --offline parse_native_message -- --exact`

Expected: `parse_native_message` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
fn parse_arguments(value: &Value) -> Result<Value, ()> {
    match value {
        Value::String(raw) => serde_json::from_str(raw).map_err(|_| ()),
        other => Ok(other.clone()),
    }
}

pub fn parse_native_message(message: &Value) -> AssistantTurn {
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        if calls.is_empty() {
            // fall through to content
        } else {
            let mut parsed = Vec::new();
            for (index, call) in calls.iter().enumerate() {
                let id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(&format!("call-{index}"))
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
                    (false, Ok(arguments)) => parsed.push(ParsedToolCall { id, name, arguments }),
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
    let arguments = value.get("arguments").cloned().unwrap_or(json!({}));
    AssistantTurn::ToolCalls(vec![ParsedToolCall {
        id: "json-0".into(),
        name: name.into(),
        arguments,
    }])
}
```

Reuse existing `extract_json`. Keep it even after deleting the old router.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path pchronicle-web/Cargo.toml --locked --offline -- native_tool_calls native_prose json_fallback_accepts`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add pchronicle-web/src/agent.rs
git commit -m "$(cat <<'EOF'
feat(pchronicle-web): parse Copilot tool calls and JSON fallback

Accept OpenAI tool_calls when the provider supports them, and the same {tool}/{final} object when it does not.

EOF
)"
```

---

### Task 3: Tool result formatters

**Files:**
- Modify: `pchronicle-web/src/agent.rs`
- Test: `pchronicle-web/src/agent.rs`

**Interfaces:**
- Consumes: `RunAnalysis`, `TurnDetail`, `QueryEvidence`, existing `truncate`
- Produces:

```rust
pub const TURN_BODY_LIMIT: usize = 8 * 1024;
pub const TOOL_NAMES: [&str; 3] = ["get_analysis", "get_turn", "query_sql"];

pub fn format_analysis_result(analysis: &RunAnalysis) -> String;
pub fn format_turn_result(detail: &TurnDetail) -> String;
pub fn format_sql_result(sql: &str, evidence: &QueryEvidence) -> String;
pub fn unknown_tool_result(name: &str) -> String;
```

`format_turn_result` must call the existing UTF-8-safe `truncate(..., TURN_BODY_LIMIT)` on message text. Do not include the full raw events array.

- [ ] **Step 1: Write the failing tests**

Use this `TurnDetail` / `RunAnalysis` in the tests (every field, no comments):

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path pchronicle-web/Cargo.toml --locked --offline unknown_tool_is_an_error_string -- --exact`

Expected: `unknown_tool_result` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
pub fn unknown_tool_result(name: &str) -> String {
    format!("Unknown tool `{name}`. Valid tools: get_analysis, get_turn, query_sql.")
}

pub fn format_analysis_result(analysis: &RunAnalysis) -> String {
    format!(
        "turns={} events={} tools={} explicit_errors={} tokens={} latency_p95={} latency_samples={}/{}\nsources={:?}\nkinds={:?}\nmodels={:?}\ntool_names={:?}",
        analysis.turn_count,
        analysis.event_count,
        analysis.tool_call_count,
        analysis.error_count,
        analysis.total_tokens.map(|value| value.to_string()).unwrap_or_else(|| "unavailable".into()),
        analysis.latency_ms.p95.map(|value| format!("{value:.1}")).unwrap_or_else(|| "unavailable".into()),
        analysis.latency_ms.sample_count,
        analysis.latency_ms.total_count,
        analysis.source_breakdown.iter().map(|item| format!("{}:{}", item.name, item.turn_count)).collect::<Vec<_>>(),
        analysis.kind_breakdown.iter().map(|item| format!("{}:{}", item.name, item.turn_count)).collect::<Vec<_>>(),
        analysis.model_breakdown.iter().map(|item| format!("{}:{}", item.name, item.turn_count)).collect::<Vec<_>>(),
        analysis.tools.iter().map(|tool| format!("{}:{}", tool.name, tool.count)).collect::<Vec<_>>(),
    )
}

pub fn format_turn_result(detail: &TurnDetail) -> String {
    let (body, truncated) = truncate(&detail.turn.text(), TURN_BODY_LIMIT);
    format!(
        "[turn:{}] source={} kind={} model={} latency={} tools={}\n{}\n{}",
        detail.summary.id,
        detail.summary.source,
        detail.summary.kind.as_deref().unwrap_or("unknown"),
        detail.summary.model_name.as_deref().unwrap_or("unavailable"),
        detail.summary.latency_ms.map(|value| format!("{value:.1}")).unwrap_or_else(|| "unavailable".into()),
        detail.summary.tool_names.join(","),
        body,
        if truncated { "truncated=true" } else { "truncated=false" }
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
```

Keep / restore `fn truncate(value: &str, limit: usize) -> (String, bool)` from the current file (UTF-8 boundary). Do not delete the existing `context_truncation_stays_on_utf8_boundaries` test.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path pchronicle-web/Cargo.toml --locked --offline -- turn_formatter unknown_tool analysis_formatter context_truncation`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add pchronicle-web/src/agent.rs
git commit -m "$(cat <<'EOF'
feat(pchronicle-web): format Copilot tool results

Bound turn bodies and analysis summaries so the model sees evidence, not the full warehouse payload.

EOF
)"
```

---

### Task 4: Sync loop driver

**Files:**
- Modify: `pchronicle-web/src/agent.rs`
- Test: `pchronicle-web/src/agent.rs`

**Interfaces:**
- Consumes: Tasks 1–3
- Produces:

```rust
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
    mut execute: impl FnMut(&ParsedToolCall) -> String,
) -> DriveResult;
```

Behavior:

- `ToolCalls`: each call counts as one toward `MAX_TOOL_ROUNDS`. Unknown names still `execute` (formatter returns error string) and append a `ThreadRole::Tool` message. `get_turn` with `arguments.turn_id` i64 appends to `fetched_turn_ids` (unique, order preserved) only when the result does not start with `Unknown tool` or `Turn evidence could not`. Simpler rule: record the id whenever `name == "get_turn"` and `turn_id` parses, even if the HTTP layer later fails — the panel fence can still point at the attempted turn. Prefer: record only if `execute` result contains `[turn:{id}]`.
- If `tool_rounds` would exceed 8, do not execute remaining calls; return `Failed { message: "Tool limit reached. Asking for a final answer without tools." }` is wrong — spec says force one more model call without tools. So `apply_model_turn` should return `Continue` after executing up to the remaining budget, and set `state.tool_rounds == MAX_TOOL_ROUNDS`. The HTTP loop (Task 5) sees `tool_rounds >= 8` and requests once with `tools=false`. Add `state.force_final: bool` set when budget hits 8 still without `Final`.
- `Final(text)` → `Done { text }` and reset `illegal_json_streak`.
- `Invalid` in native mode (`json_mode == false`) → set `json_mode = true`, `Continue` (retry same user question in JSON mode). Do **not** increment `illegal_json_streak` here.
- `Invalid` in `json_mode` → increment `illegal_json_streak`. At 2 → `Failed { message: "The model could not use tool-calling. Try a different OpenAI-compatible model in Settings." }`.

- [ ] **Step 1: Write the failing tests**

```rust
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
fn loop_runs_three_tools_then_final() {
    let mut state = empty_state();
    let calls = vec![
        ParsedToolCall { id: "1".into(), name: "get_analysis".into(), arguments: json!({}) },
        ParsedToolCall { id: "2".into(), name: "get_turn".into(), arguments: json!({"turn_id": 4}) },
        ParsedToolCall { id: "3".into(), name: "query_sql".into(), arguments: json!({"sql": "SELECT 1"}) },
    ];
    let result = apply_model_turn(&mut state, AssistantTurn::ToolCalls(calls), |call| {
        format!("ok {}", call.name)
    });
    assert!(matches!(result, DriveResult::Continue));
    assert_eq!(state.tool_rounds, 3);
    assert_eq!(state.messages.len(), 3);
    let done = apply_model_turn(&mut state, AssistantTurn::Final("see [turn:4]".into()), |_| String::new());
    assert_eq!(done, DriveResult::Done { text: "see [turn:4]".into() });
}

#[test]
fn unknown_tool_does_not_stop_the_loop() {
    let mut state = empty_state();
    let call = ParsedToolCall { id: "1".into(), name: "drop".into(), arguments: json!({}) };
    let result = apply_model_turn(&mut state, AssistantTurn::ToolCalls(vec![call]), |call| {
        unknown_tool_result(&call.name)
    });
    assert!(matches!(result, DriveResult::Continue));
    assert!(state.messages[0].text.contains("Unknown tool"));
}

#[test]
fn two_invalid_json_rounds_stop() {
    let mut state = empty_state();
    state.json_mode = true;
    assert!(matches!(
        apply_model_turn(&mut state, AssistantTurn::Invalid, |_| String::new()),
        DriveResult::Continue
    ));
    match apply_model_turn(&mut state, AssistantTurn::Invalid, |_| String::new()) {
        DriveResult::Failed { message } => assert!(message.contains("tool-calling")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn eighth_tool_sets_force_final() {
    let mut state = empty_state();
    state.tool_rounds = 7;
    let call = ParsedToolCall { id: "1".into(), name: "get_analysis".into(), arguments: json!({}) };
    apply_model_turn(&mut state, AssistantTurn::ToolCalls(vec![call]), |_| "ok".into());
    assert_eq!(state.tool_rounds, 8);
    assert!(state.force_final);
}
```

Derive `PartialEq` on `DriveResult` for the assertions, or match instead of `assert_eq!` on `Done`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path pchronicle-web/Cargo.toml --locked --offline loop_runs_three_tools -- --exact`

Expected: `apply_model_turn` not found.

- [ ] **Step 3: Write minimal implementation**

Implement `apply_model_turn` exactly to the rules above. Append tool messages with `tool_call_id` / `tool_name`. If `name == "get_turn"`, parse `turn_id` as i64 or as number in JSON, push to `fetched_turn_ids` if absent. Set `force_final = true` when `tool_rounds >= MAX_TOOL_ROUNDS`.

When executing a batch, stop scheduling new calls once `tool_rounds == MAX_TOOL_ROUNDS`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path pchronicle-web/Cargo.toml --locked --offline -- loop_runs_three_tools unknown_tool_does_not two_invalid_json eighth_tool`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add pchronicle-web/src/agent.rs
git commit -m "$(cat <<'EOF'
feat(pchronicle-web): drive Copilot tool loop without an LLM

Encode round limits, JSON fallback failure, and unknown-tool continuation in a sync driver tests can script.

EOF
)"
```

---

### Task 5: Replace `answer()` and delete the skill router

**Files:**
- Modify: `pchronicle-web/src/agent.rs`
- Modify: `pchronicle-web/src/workspace.rs` (only if it no longer compiles: `skill_ids` / `AgentAnswer` call sites — fix in Task 6 if you leave a thin adapter for one commit; prefer updating `answer` signature here and fixing CopilotPanel in Task 6 in the same compile, or this commit includes a temporary stub `pub fn skill_ids() -> &'static [&'static str] { &[] }` that Task 6 removes. **Do not leave `skill_ids`.** Update CopilotPanel in Task 6 immediately after if this commit cannot compile alone. Prefer one compile-clean commit: this task may include a minimal CopilotPanel change that still compiles — drop `skill_ids()` usage by commenting chips out only in Task 6. So Task 5 must keep `pub fn skill_ids()` compiling OR update workspace in this task.

**Compile-clean rule:** This task updates `answer` and deletes skills. Update `CopilotPanel` enough that `cargo test` compiles: if `skill_ids` is deleted, remove the chip loop in the same commit (UI polish still Task 6).

**Interfaces:**
- Consumes: Tasks 1–4, `api::turn_detail`, `api::run_analysis`, `api::query_evidence`, existing `chat()` HTTP helper
- Produces:

```rust
pub struct AnswerRequest<'a> {
    pub config: &'a LlmConfig,
    pub user_message: &'a str,
    pub run: &'a RunSummary,
    pub analysis: &'a RunAnalysis,
    pub focused_turn_id: Option<i64>,
    pub thread: CopilotThread,
    pub on_step: Option<&'a dyn Fn(&str)>,
}

pub struct AgentAnswer {
    pub thread: CopilotThread,
    pub text: String,
    pub sql: Option<String>,
    pub truncated: bool,
    pub fetched_turn_ids: Vec<i64>,
}

pub async fn answer(request: AnswerRequest<'_>) -> Result<AgentAnswer, String>;
```

`answer` algorithm:

1. If `!config.is_configured()` return `Err("Configure an OpenAI-compatible model in Settings before asking Copilot.".into())`.
2. Clone `thread`, push the user `ThreadMessage`.
3. Build system string: Copilot rules from the spec + analysis card (`session`, `status`, `turn_count`, `event_count`, `error_count`, `total_tokens`, P95 + sample coverage). If `focused_turn_id` is `Some(n)`, append `The user is currently viewing turn #{n}. Do not assume its body; call get_turn if needed.`
4. Loop:
   - `compress_messages_for_llm` then map to OpenAI messages (`user` / `assistant` / `tool` with `tool_call_id`).
   - `chat_with_tools(config, system, messages, tools_enabled)` where `tools_enabled = !state.json_mode && !state.force_final`.
   - If HTTP error body suggests tools unsupported (status 400/422 or message contains `"tools"` / `tool_choice` / `response_format`) and `!json_mode`, set `json_mode = true` and continue without counting a tool round.
   - Other HTTP/CORS/empty errors: return `Err(...)`.
   - Parse with `parse_native_message` or `parse_json_fallback` according to `json_mode`.
   - `execute` closure: match name:
     - `get_analysis` → `format_analysis_result(request.analysis)` (do not refetch unless you already have a helper; panel always passes analysis).
     - `get_turn` → `api::turn_detail(run, id).await` then `format_turn_result`, or error string on failure.
     - `query_sql` → `api::query_evidence(sql).await` then `format_sql_result`, or error string. Remember last sql on the tool message.
     - else → `unknown_tool_result`.
   - Call `on_step` with e.g. `get_turn #12` before execute.
   - `apply_model_turn`.
   - `Done` → append assistant message (`text`); if `fetched_turn_ids` non-empty, append `trajectory_fence("Cited turns", ids)` to `text`. `trim_thread`. Return `AgentAnswer`.
   - `Failed` → return `Err(message)` or push an assistant error `ThreadMessage` and `Ok`? Spec: JSON failure and empty final are conversation errors. Return `Ok` with assistant text = failure message so it persists. HTTP failures stay `Err` (UI error bubble). Split: `DriveResult::Failed` → `Ok(AgentAnswer { text: message, ... })` so it is saved. Transport errors → `Err`.
5. Delete: `skill_ids`, `run_skill`, `decorate_skill_evidence`, `resolve_skill`, `select_action`, `Selection`, `overview_evidence` used only by skills, `evidence_context`, `include_full_turn` path, `AnswerRequest.turns`, `AnswerRequest.selected`. Keep `summarize` deleted; final text comes from the model `Final`.
6. OpenAI tools payload:

```json
{
  "type": "function",
  "function": {
    "name": "get_turn",
    "description": "Fetch one turn in the current run.",
    "parameters": {
      "type": "object",
      "properties": { "turn_id": { "type": "integer" } },
      "required": ["turn_id"]
    }
  }
}
```

Same pattern for `get_analysis` (empty properties) and `query_sql` (`sql` string). When `json_mode`, system prompt additionally requires the `{tool}` / `{final}` schema and `response_format: json_object`. When `force_final`, no tools, prompt: `Answer now from evidence already gathered. Do not call tools.`

- [ ] **Step 1: Write a failing compile/test for the new `answer` signature**

Keep loop tests. Add:

```rust
#[test]
fn unconfigured_config_is_detected() {
    let config = LlmConfig::default();
    assert!(!config.is_configured());
}
```

This already passes via existing `is_configured`. The real gate is compiling after deleting `skill_ids`. Proceed to implementation; no LLM mock in unit tests for `answer`.

- [ ] **Step 2: Confirm old skill tests will fail after deletion**

`explicit_commands_resolve_to_known_skills` must be **deleted** with `resolve_skill`.

- [ ] **Step 3: Implement `answer`, tools JSON, delete router**

Replace `chat()` to accept optional `tools: Option<Value>` and optional extra messages array. Map `ThreadMessage` to:

- User → `{role: user, content}`
- Assistant → `{role: assistant, content}`
- Tool → `{role: tool, tool_call_id, content}`

- [ ] **Step 4: Run tests**

Run: `cargo test --manifest-path pchronicle-web/Cargo.toml --locked --offline`

Expected: PASS. No `resolve_skill` test. All previous agent tests still pass.

- [ ] **Step 5: Commit**

```bash
git add pchronicle-web/src/agent.rs pchronicle-web/src/workspace.rs
git commit -m "$(cat <<'EOF'
feat(pchronicle-web): run Copilot as a BYOK tool loop

Replace the skill router with get_analysis, get_turn, and read-only SQL calls pinned to the open run.

EOF
)"
```

If workspace is not yet compiling, include the chip removal from Task 6 Step 3 in this commit so CI is green.

---

### Task 6: CopilotPanel persistence, steps, Enter, Trace fence

**Files:**
- Modify: `pchronicle-web/src/workspace.rs` (`CopilotPanel`, `ChatBubble` / `ChatMessage`)
- Modify: `pchronicle-web/assets/workbench.css` only if the skill-chip row looks empty (delete `.pc2-skill-chips` usage; leftover CSS may stay)

**Interfaces:**
- Consumes: `thread_storage_key`, `load_thread` / `save_thread`, `answer`, `AgentAnswer`
- Produces: panel behavior from the spec

Add next to `load_config`:

```rust
pub fn load_thread(run: &RunSummary) -> CopilotThread {
    // localStorage get thread_storage_key(run); default empty messages, updated_at = 0
}

pub fn save_thread(run: &RunSummary, thread: &CopilotThread) {
    let mut thread = thread.clone();
    trim_thread(&mut thread);
    // set_item
}
```

These use `web_sys` like `load_config`. Unit-test only the key (Task 1). Do not mock localStorage.

Panel:

- On mount, `messages` / thread from `load_thread(&run)`.
- Drop skill chips, drop include-full checkbox, drop `selected` from `answer`.
- Pass `focused_turn_id: expanded_turn_id` (the panel already receives `selected`; use `selected.as_ref().map(|detail| detail.summary.id)`).
- `on_step`: set a `step: Signal<String>` shown as `span.spinner` + `{step}` instead of `Selecting one read-only analysis action…`.
- Submit: if `!config.is_configured()`, set `settings(true)` and push no user message, or push the user message then show the configure error as assistant text. Spec: stop and prompt Settings. Implement: do not call `answer`; `settings.set(true)` and a one-line assistant message `Configure an OpenAI-compatible model in Settings before asking Copilot.`
- On `Ok(answer)`: replace thread with `answer.thread`, `save_thread`.
- On `Err(transport)`: keep user message, append assistant `Unable to complete analysis: {err}` with `truncated: false`, save.
- Enter without Shift submits the form (call the same submit path). Shift+Enter inserts newline. Remove the current preventDefault-only handler.
- `ChatBubble` can keep taking a small view struct. Map `ThreadRole::User` → user bubble; Assistant → assistant; Tool → a compact `pc2-action-label` line (`get_turn`) plus optional `<details>` for text, not a fake assistant bubble. Do not render trajectory tables for Tool messages.
- Final assistant `text` already includes `trajectory_fence` from Task 5; existing `parse_rich_blocks` + `on_turn` remain.

- [ ] **Step 1: No new unit test for Enter** (spec: no e2e). Manually verify the submit handler is invoked from `onkeydown` when `event.key() == Key::Enter && !event.modifiers().shift()` by triggering the same closure as `onsubmit` (extract `submit()` function used by both).

- [ ] **Step 2: Implement load/save_thread and panel wiring**

```rust
onkeydown: move |event| {
    if event.key() == Key::Enter && !event.modifiers().shift() {
        event.prevent_default();
        submit_copilot(/* ... */);
    }
}
```

`submit_copilot` must be a `move` closure shared with `form.onsubmit`.

- [ ] **Step 3: Remove chips and checkbox; show Settings empty-state when `!config().is_configured()`**

Welcome copy: ask about this trajectory; Copilot can inspect analysis, a turn, or run read-only SQL.

- [ ] **Step 4: Run tests**

Run: `cargo test --manifest-path pchronicle-web/Cargo.toml --locked --offline`

Expected: PASS (40+ tests, old skill tests gone, new agent tests present).

- [ ] **Step 5: Commit**

```bash
git add pchronicle-web/src/workspace.rs pchronicle-web/src/agent.rs
git commit -m "$(cat <<'EOF'
feat(pchronicle-web): persist Copilot chat and submit on Enter

Restore the per-run thread in the overlay, show tool steps, and jump cited turns through the existing Trace handlers.

EOF
)"
```

---

## Spec coverage

| Spec item | Task |
|---|---|
| Native tools + JSON fallback | 2, 4, 5 |
| `get_analysis` / `get_turn` / `query_sql` pinned to run | 3, 5 |
| 8 tool cap + force final | 4, 5 |
| Two illegal JSON stop | 4 |
| Unknown tool continues | 4 |
| Turn 8 KiB UTF-8 trim | 3 |
| Thread key + 200 KiB / 32 KiB | 1, 6 |
| No key → Settings | 5, 6 |
| Delete skills / chips / include-full | 5, 6 |
| `[turn:ID]` + trajectory fence on fetched ids | 5, 6 |
| Enter submit | 6 |
| No server Copilot API / no SQL parser | 5 |
| No streaming / cohort / workspace actions | (not implemented) |

## Placeholder scan

No TBD / “handle edge cases” / “similar to Task N” leftovers. Task 3 tests construct full `TurnDetail` / `RunAnalysis` literals.

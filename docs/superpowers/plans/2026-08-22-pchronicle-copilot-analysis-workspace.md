# pChronicle Copilot Analysis Workspace Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the manual SQL Analyze page with a desktop, question-driven Copilot workflow that requires explicit query confirmation and renders query evidence as an explorable table with inline column distributions.

**Architecture:** Keep all model interaction and analysis-session judgment in `pchronicle-web`. Extract the existing browser BYOK transport into a shared module, then build an Analyze-specific state machine and agent that generate structured plans without tools. The existing server remains a read-only catalog/query provider; result profiling, refinement intents, evidence digests, interpretation, and local session persistence remain deterministic frontend responsibilities.

**Tech Stack:** Rust 2021, Dioxus 0.7.9 Web, `gloo-net`, `serde`/`serde_json`, `web-sys`, `time` 0.3.55 for strict RFC 3339 parsing, existing pChronicle read-only query API, browser-local OpenAI-compatible BYOK.

**Spec:** `docs/superpowers/specs/2026-08-22-pchronicle-copilot-analysis-workspace-design.md`

## Global Constraints

- Desktop only; do not add mobile-specific product behavior.
- Every generated, edited, refined, or full-profile SQL query requires an explicit user click before `/api/query/evidence` is called.
- Do not add a backend Copilot endpoint, backend analysis decisions, backend result profiling, or new query semantics.
- Continue using `/api/query/tables` and `/api/query/evidence`; the server remains the final read-only SQL and resource-budget enforcement boundary.
- BYOK credentials stay in browser `localStorage` and go directly to the configured OpenAI-compatible endpoint.
- Persist analysis questions, scopes, plans, execution summaries, profiles, and interpretations locally; never persist complete `QueryEvidence.rows`.
- Preview profiles describe only returned rows. When `truncated` is true, never label a preview profile as the full query distribution.
- Missing measurements are unknown, not zero. Copilot observations and inferences render in separate sections.
- Do not enter Search, Queue/Samplers, TTAS/tiered memory, or `persisting-dlcapt`.
- Preserve unrelated working-tree edits, especially current changes in `pchronicle-web/src/components.rs`, `pchronicle-web/src/model.rs`, `pchronicle-web/assets/inline-trace.css`, and `crates/persisting-pchronicle/src/formats/openai_corpus.rs`.
- The current checkout has no Pinboard/Compare implementation. Implement the reusable multi-run `AnalysisScope` and URL/session entry contract, but do not recreate those unrelated features in this plan.
- Use a new `assets/analyze-workspace.css`; do not fold Analyze styles into the existing run-detail `assets/analysis.css`.

## File Map

- `pchronicle-web/src/llm.rs` — shared BYOK config, request construction, provider HTTP transport, capability-error classification.
- `pchronicle-web/src/llm_settings.rs` — shared Dioxus model settings dialog used by Trajectory Copilot and Analyze.
- `pchronicle-web/src/analysis_session.rs` — Analyze domain types, revision state machine, scope codec, persistence budgets and localStorage adapter.
- `pchronicle-web/src/result_profile.rs` — pure column discovery, type inference, histogram/Top-K/statistics, refinement intent types.
- `pchronicle-web/src/analysis_agent.rs` — structured plan generation, one repair attempt, bounded evidence digest, structured interpretation.
- `pchronicle-web/src/result_explorer.rs` — profile-enhanced evidence table, selected-column panel, visual refinement staging, identity deep links.
- `pchronicle-web/src/analysis.rs` — Analyze workspace orchestration and UI; the only module coordinating session, agent, query API, profiles, and interpretation.
- `pchronicle-web/src/tools.rs` — compatibility re-export for the existing `page=tools` route during migration.
- `pchronicle-web/src/agent.rs` — retain the single-Run tool loop; consume shared `llm` and `llm_settings` only.
- `pchronicle-web/src/workspace.rs` — route/session wiring and Run-to-Analyze context action; do not absorb Analyze internals.
- `pchronicle-web/src/api.rs` — keep existing query functions; no new endpoint.
- `pchronicle-web/src/main.rs` — module declarations.
- `pchronicle-web/assets/analyze-workspace.css` — all new Analyze page chrome, plan, interpretation, result profile, timeline, and state styling.
- `pchronicle-web/index.html` — load the new stylesheet.
- `pchronicle-web/tests/fixtures/mock-openai.mjs` — deterministic CORS-enabled local provider used only for browser acceptance.

---

### Task 1: Extract the shared BYOK client and settings dialog

**Files:**
- Create: `pchronicle-web/src/llm.rs`
- Create: `pchronicle-web/src/llm_settings.rs`
- Modify: `pchronicle-web/src/main.rs:3-10`
- Modify: `pchronicle-web/src/agent.rs:1-8,176-241,658-751`
- Modify: `pchronicle-web/src/workspace.rs:1-8,1039-1215`
- Test: inline `#[cfg(test)]` module in `pchronicle-web/src/llm.rs`

**Interfaces:**
- Produces: `llm::LlmConfig`, `load_config()`, `save_config()`, `CompletionRequest`, `CompletionError`, `complete()` and `completion_body()`.
- Produces: `llm_settings::LlmSettings` with `config`, `on_close`, and `on_save` props.
- Consumes: existing OpenAI-compatible `/chat/completions` contract and current `pchronicle_llm_config` storage key.
- Preserves: Trajectory Copilot native tool calls, JSON fallback, settings copy, default provider/model, and all current agent tests.

- [ ] **Step 1: Add failing request-body tests**

Create `llm.rs` with the shared data types and tests first. The test calls the not-yet-implemented `completion_body`:

```rust
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const STORAGE_KEY: &str = "pchronicle_llm_config";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LlmConfig {
    pub api_base: String,
    pub api_key: String,
    pub model: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompletionRequest {
    pub system: String,
    pub messages: Vec<Value>,
    pub tools: Option<Value>,
    pub response_format: Option<Value>,
    pub temperature: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_body_omits_optional_fields() {
        let body = completion_body(
            "model-a",
            CompletionRequest {
                system: "system".into(),
                messages: vec![json!({"role":"user","content":"question"})],
                tools: None,
                response_format: None,
                temperature: 0.2,
            },
        );
        assert_eq!(body["model"], "model-a");
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert!(body.get("tools").is_none());
        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn completion_body_includes_tools_and_json_contract() {
        let body = completion_body(
            "model-a",
            CompletionRequest {
                system: "system".into(),
                messages: Vec::new(),
                tools: Some(json!([{"type":"function"}])),
                response_format: Some(json!({"type":"json_object"})),
                temperature: 0.1,
            },
        );
        assert!(body.get("tools").is_some());
        assert_eq!(body["response_format"]["type"], "json_object");
    }
}
```

- [ ] **Step 2: Run the tests and verify the new function is missing**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked llm::tests::completion_body
```

Expected: compilation fails because `completion_body` is not defined.

- [ ] **Step 3: Implement the shared transport and move settings without behavior changes**

Implement the pure body builder exactly around optional fields:

```rust
pub fn completion_body(model: &str, request: CompletionRequest) -> Value {
    let mut messages = vec![json!({"role":"system", "content": request.system})];
    messages.extend(request.messages);
    let mut body = json!({
        "model": model.trim(),
        "temperature": request.temperature,
        "messages": messages,
    });
    if let Some(tools) = request.tools {
        body["tools"] = tools;
        body["tool_choice"] = json!("auto");
    }
    if let Some(response_format) = request.response_format {
        body["response_format"] = response_format;
    }
    body
}
```

Move the current `LlmConfig` default/configured implementation, `load_config`, and `save_config` from `agent.rs` into `llm.rs` without changing `STORAGE_KEY`. Define:

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct CompletionError {
    pub status: Option<u16>,
    pub message: String,
}

impl CompletionError {
    pub fn suggests_tools_unsupported(&self) -> bool {
        matches!(self.status, Some(400 | 422))
            || ["tools", "tool_choice", "response_format"]
                .iter()
                .any(|needle| self.message.to_ascii_lowercase().contains(needle))
    }
    pub fn suggests_response_format_unsupported(&self) -> bool {
        matches!(self.status, Some(400 | 415 | 422))
            && self.message.to_ascii_lowercase().contains("response_format")
    }
}

pub async fn complete(
    config: &LlmConfig,
    request: CompletionRequest,
) -> Result<Value, CompletionError> {
    let url = format!(
        "{}/chat/completions",
        config.api_base.trim().trim_end_matches('/')
    );
    let body = completion_body(&config.model, request);
    let response = gloo_net::http::Request::post(&url)
        .header("Authorization", &format!("Bearer {}", config.api_key.trim()))
        .header("Content-Type", "application/json")
        .json(&body)
        .map_err(|error| CompletionError { status: None, message: error.to_string() })?
        .send()
        .await
        .map_err(|error| CompletionError {
            status: None,
            message: format!("LLM request failed (check API base, key, and CORS): {error}"),
        })?;
    let status = response.status();
    let raw = response.text().await.map_err(|error| CompletionError {
        status: Some(status),
        message: error.to_string(),
    })?;
    if !(200..300).contains(&status) {
        return Err(CompletionError {
            status: Some(status),
            message: format!("LLM HTTP {status}: {raw}"),
        });
    }
    let value: Value = serde_json::from_str(&raw).map_err(|error| CompletionError {
        status: Some(status),
        message: format!("LLM returned invalid JSON: {error}"),
    })?;
    let message = value
        .pointer("/choices/0/message")
        .cloned()
        .ok_or_else(|| CompletionError {
            status: Some(status),
            message: "LLM returned an empty response".into(),
        })?;
    let has_content = message
        .get("content")
        .and_then(Value::as_str)
        .is_some_and(|content| !content.trim().is_empty());
    let has_tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty());
    if !has_content && !has_tool_calls {
        return Err(CompletionError {
            status: Some(status),
            message: "LLM returned an empty response".into(),
        });
    }
    Ok(message)
}
```

Move the existing `LlmSettings` RSX and copy verbatim into `llm_settings.rs`; only change imports to `crate::llm::{self, LlmConfig}`. Update `agent.rs` to call `llm::complete` and update `workspace.rs` imports/component paths. Do not alter tool-loop decisions or messages.

- [ ] **Step 4: Verify shared tests and the unchanged Trajectory Copilot suite**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked llm::tests
cargo test --manifest-path pchronicle-web/Cargo.toml --locked agent::tests
cargo fmt --manifest-path pchronicle-web/Cargo.toml -- --check
```

Expected: all tests pass; no existing Copilot test count decreases.

- [ ] **Step 5: Commit the behavior-preserving extraction**

```bash
git add pchronicle-web/src/llm.rs pchronicle-web/src/llm_settings.rs pchronicle-web/src/main.rs pchronicle-web/src/agent.rs pchronicle-web/src/workspace.rs
git commit -m "refactor(pchronicle-web): share browser LLM client"
```

---

### Task 2: Add the analysis revision state machine and local session model

**Files:**
- Create: `pchronicle-web/src/analysis_session.rs`
- Create: `pchronicle-web/src/result_profile.rs` (serializable profile data types only; Task 3 adds algorithms)
- Modify: `pchronicle-web/src/main.rs:3-12`
- Test: inline `#[cfg(test)]` module in `pchronicle-web/src/analysis_session.rs`

**Interfaces:**
- Consumes: `model::QueryCatalog`, `model::QueryEvidence`, `model::RunSummary`.
- Produces: `AnalysisScope`, `AnalysisScopeItem`, `AnalysisPlan`, `SuggestedView`, `RevisionState`, `AnalysisRevision`, `AnalysisSession`, `AnalysisInterpretation`, `AnalysisEffect`, `ExecutionSummary`.
- Produces: `load_sessions(storage_fingerprint)`, `save_sessions(storage_fingerprint, sessions)`, `clear_sessions(storage_fingerprint)`, `trim_sessions(sessions)`.
- Produces: `analysis_href(scope)` and `scope_from_query()` for dataset, root, one-run, and multi-run entry.

- [ ] **Step 1: Write failing state-transition and persistence tests**

Add these tests before the methods:

```rust
#[test]
fn generated_plan_waits_for_explicit_execution() {
    let mut revision = AnalysisRevision::draft(1, "compare failures", scope());
    revision.begin_plan_generation().unwrap();
    revision.finish_plan(1, plan()).unwrap();
    assert_eq!(revision.state, RevisionState::PlanReady);
    assert!(revision.pending_effect.is_none());

    revision.confirm_execution().unwrap();
    assert_eq!(
        revision.pending_effect,
        Some(AnalysisEffect::ExecuteSql {
            revision_id: 1,
            sql: "SELECT status, COUNT(*) FROM default.runs GROUP BY status".into(),
        })
    );
}

#[test]
fn query_result_rows_are_not_persisted() {
    let mut revision = AnalysisRevision::draft(1, "question", scope());
    revision.evidence = Some(QueryEvidence {
        rows: vec![serde_json::json!({"secret-row":"not persisted"})],
        returned_rows: 1,
        truncated: false,
        max_rows: 100,
        max_bytes: 4 * 1024 * 1024,
    });
    let encoded = serde_json::to_string(&AnalysisSession::with_revision(revision)).unwrap();
    assert!(!encoded.contains("secret-row"));
}

#[test]
fn empty_query_result_skips_interpretation() {
    let mut revision = executing_revision();
    let effect = revision.finish_query(1, empty_evidence(), Vec::new()).unwrap();
    assert_eq!(revision.state, RevisionState::Complete);
    assert_eq!(effect, None);
}
```

Also test that stale async results with the wrong `revision_id` are ignored, `QueryError` can only retry after an explicit action, 21 sessions trim to the newest 20, and the encoded session is at most 256 KiB.

Define the test fixtures in the same `tests` module so every example above is directly runnable:

```rust
fn scope() -> AnalysisScope {
    AnalysisScope {
        database: "default".into(),
        storage_path: "tmp/test/".into(),
        snapshot_id: "snapshot-a".into(),
        items: vec![AnalysisScopeItem::Dataset { name: "default".into() }],
    }
}

fn plan() -> AnalysisPlan {
    AnalysisPlan {
        id: 1,
        question: "compare failures".into(),
        intent_summary: "Compare failures by status".into(),
        scope_summary: "default dataset".into(),
        filters: Vec::new(),
        groupings: vec!["status".into()],
        measures: vec!["run count".into()],
        expected_columns: vec!["status".into(), "run_count".into()],
        suggested_view: SuggestedView::Distribution,
        sql: "SELECT status, COUNT(*) FROM default.runs GROUP BY status".into(),
        warnings: Vec::new(),
    }
}

fn empty_evidence() -> QueryEvidence {
    QueryEvidence {
        rows: Vec::new(),
        returned_rows: 0,
        truncated: false,
        max_rows: 100,
        max_bytes: 4 * 1024 * 1024,
    }
}

fn executing_revision() -> AnalysisRevision {
    let mut revision = AnalysisRevision::draft(1, "compare failures", scope());
    revision.begin_plan_generation().unwrap();
    revision.finish_plan(1, plan()).unwrap();
    revision.confirm_execution().unwrap();
    revision.take_pending_effect();
    revision
}
```

- [ ] **Step 2: Run the state tests and verify they fail**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis_session::tests
```

Expected: compilation fails because the analysis session types and methods do not exist.

- [ ] **Step 3: Implement the domain types and guarded transitions**

Use serializable scope items with full coordinates:

```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnalysisScopeItem {
    Dataset { name: String },
    Root {
        dataset: String,
        file: String,
        root_session_id: String,
    },
    Run { run: RunSummary },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisScope {
    pub database: String,
    pub storage_path: String,
    pub snapshot_id: String,
    pub items: Vec<AnalysisScopeItem>,
}
```

Define the state and effects exactly:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionState {
    Draft,
    GeneratingPlan,
    PlanReady,
    Executing,
    Interpreting,
    Complete,
    PlanError,
    QueryError,
    InterpretationError,
    Stale,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AnalysisEffect {
    ExecuteSql { revision_id: u64, sql: String },
    Interpret { revision_id: u64 },
}
```

Define the persisted contracts before implementing transitions:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestedView { Table, Distribution, Trend }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisPlan {
    pub id: u64,
    pub question: String,
    pub intent_summary: String,
    pub scope_summary: String,
    pub filters: Vec<String>,
    pub groupings: Vec<String>,
    pub measures: Vec<String>,
    pub expected_columns: Vec<String>,
    pub suggested_view: SuggestedView,
    pub sql: String,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceReference {
    pub label: String,
    pub row_index: Option<usize>,
    pub dataset: Option<String>,
    pub file: Option<String>,
    pub run_id: Option<String>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub root_session_id: Option<String>,
    pub turn_id: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnalysisInterpretation {
    pub observations: Vec<String>,
    pub inferences: Vec<String>,
    pub limitations: Vec<String>,
    pub follow_ups: Vec<String>,
    pub references: Vec<EvidenceReference>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionSummary {
    pub returned_rows: usize,
    pub truncated: bool,
    pub max_rows: usize,
    pub max_bytes: usize,
    pub executed_at_ms: u64,
    pub profiles: Vec<crate::result_profile::ColumnProfile>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisRevision {
    pub id: u64,
    pub question: String,
    pub scope: AnalysisScope,
    pub state: RevisionState,
    pub plan: Option<AnalysisPlan>,
    pub manually_edited: bool,
    pub execution: Option<ExecutionSummary>,
    pub interpretation: Option<AnalysisInterpretation>,
    pub error: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub needs_rerun: bool,
    #[serde(skip)]
    pub evidence: Option<QueryEvidence>,
    #[serde(skip)]
    pub pending_effect: Option<AnalysisEffect>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisSession {
    pub id: String,
    pub title: String,
    pub storage_fingerprint: String,
    pub revisions: Vec<AnalysisRevision>,
    pub active_revision_id: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}
```

Create `result_profile.rs` with the serializable data contracts used by
`ExecutionSummary`: `ColumnKind`, `HistogramBin`, `ValueCount`, and
`ColumnProfile { name, kind, row_count, non_null_count, missing_count,
unique_count, min, max, mean, histogram, top_values, other_count, type_counts }`.
Task 3 adds inference and aggregation functions without changing these fields.

`finish_plan(revision_id, plan)` only stores a plan and sets `PlanReady`; it never creates an effect. `confirm_execution` is the only normal path that creates `ExecuteSql`. `finish_query(revision_id, evidence, profiles)` stores rows only in the `#[serde(skip)] evidence` field, stores an `ExecutionSummary`, and creates `Interpret` only for non-empty rows. `finish_interpretation`, `fail_plan`, `fail_query`, and `fail_interpretation` also receive the revision ID. A mismatched ID returns `Ok(None)` without changing state. `take_pending_effect()` moves the effect out exactly once.

Use `SystemTime` for IDs/timestamps. Implement a separate persisted representation if `#[serde(skip)]` cannot express `needs_rerun` cleanly. Keep constants exact:

```rust
pub const MAX_ANALYSIS_SESSIONS: usize = 20;
pub const MAX_SESSION_BYTES: usize = 256 * 1024;
pub const STORAGE_PREFIX: &str = "pchronicle_analysis:";
```

Storage failure returns a user-facing warning string and never mutates the current in-memory session.

- [ ] **Step 4: Verify transitions, serialization and trimming**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis_session::tests
cargo test --manifest-path pchronicle-web/Cargo.toml --locked model::tests
```

Expected: all tests pass, including no-row persistence and explicit-confirmation tests.

- [ ] **Step 5: Commit the state boundary**

```bash
git add pchronicle-web/src/analysis_session.rs pchronicle-web/src/result_profile.rs pchronicle-web/src/main.rs
git commit -m "feat(pchronicle-web): model analysis sessions"
```

---

### Task 3: Implement deterministic result-column profiling

**Files:**
- Modify: `pchronicle-web/src/result_profile.rs`
- Modify: `pchronicle-web/src/main.rs:3-14`
- Modify: `pchronicle-web/Cargo.toml:10-22`
- Modify: `pchronicle-web/Cargo.lock`
- Test: inline `#[cfg(test)]` module in `pchronicle-web/src/result_profile.rs`

**Interfaces:**
- Consumes: `serde_json::Value` rows from `QueryEvidence`.
- Produces: `profile_rows(rows) -> Vec<ColumnProfile>`.
- Produces: `ColumnKind`, `ColumnProfile`, `HistogramBin`, `ValueCount`, `RefinementIntent`, `RefinementPredicate`.
- Uses: `time::OffsetDateTime` with `time::format_description::well_known::Rfc3339` only; do not accept locale-dependent dates.

- [ ] **Step 1: Add failing inference, histogram and stable-order tests**

```rust
#[test]
fn profiles_numeric_categorical_text_and_missing_values() {
    let rows = vec![
        json!({"latency_ms": 10, "status": "ok", "message": "short"}),
        json!({"latency_ms": 20, "status": "failed", "message": "a longer message"}),
        json!({"latency_ms": null, "status": "ok", "message": "free text three"}),
    ];
    let profiles = profile_rows(&rows);
    assert_eq!(profile(&profiles, "latency_ms").kind, ColumnKind::Number);
    assert_eq!(profile(&profiles, "latency_ms").missing_count, 1);
    assert_eq!(profile(&profiles, "status").kind, ColumnKind::Categorical);
    assert_eq!(profile(&profiles, "status").top_values[0].label, "ok");
    assert_eq!(profile(&profiles, "message").kind, ColumnKind::Text);
}

#[test]
fn numeric_histogram_handles_single_value_without_fake_range() {
    let profiles = profile_rows(&[json!({"value": 7}), json!({"value": 7})]);
    let bins = &profile(&profiles, "value").histogram;
    assert_eq!(bins.len(), 1);
    assert_eq!((bins[0].lower, bins[0].upper, bins[0].count), (7.0, 7.0, 2));
}

#[test]
fn top_values_use_label_as_stable_tie_breaker() {
    let rows = vec![json!({"kind":"b"}), json!({"kind":"a"})];
    let values = &profile(&profile_rows(&rows), "kind").top_values;
    assert_eq!(values.iter().map(|v| v.label.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
}
```

Add tests for RFC 3339 datetime, identity-column priority, boolean, object, array, mixed scalar, empty/all-null columns, negative numeric ranges, free-text length bins, Top-10 `other_count`, and non-finite input rejection through `serde_json::Number` boundaries.

Use this local helper in the test module:

```rust
fn profile<'a>(profiles: &'a [ColumnProfile], name: &str) -> &'a ColumnProfile {
    profiles.iter().find(|profile| profile.name == name).unwrap()
}
```

- [ ] **Step 2: Run profiling tests and verify they fail**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked result_profile::tests
```

Expected: compilation fails because `profile_rows` and profile types are missing.

- [ ] **Step 3: Implement stable type inference and summaries**

Add the direct dependency already present in the lock graph:

```toml
time = { version = "=0.3.55", features = ["parsing"] }
```

Use the exact enum variants and profile fields introduced in Task 2:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnKind {
    Empty,
    Number,
    Boolean,
    Categorical,
    Text,
    DateTime,
    Object,
    Array,
    Identifier,
    Mixed,
}
```

Discovery rules must follow the spec: known identity names win; all non-null values must agree for number/boolean/object/array; every string must parse as RFC 3339 for datetime; categorical requires `unique_count <= 20` and `unique_count / non_null_count <= 0.5`; remaining strings are text. Mixed values show type counts only.

Generate at most 10 equal-width numeric/time/text-length bins. For `min == max`, emit one bin. Sort Top-K by count descending then canonical label ascending. Store null/missing count separately and do not coerce null to a value.

Define visual refinements as data, not SQL:

```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefinementIntent {
    pub source_revision_id: u64,
    pub column: String,
    pub label: String,
    pub predicate: RefinementPredicate,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefinementPredicate {
    Equals { value: Value },
    NumericRange { lower: f64, upper: f64, include_upper: bool },
    Missing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnalysisRefinement {
    Filter { intent: RefinementIntent },
    FullProfile {
        source_revision_id: u64,
        column: String,
        column_kind: ColumnKind,
    },
}
```

`AnalysisRefinement::Filter` is created only when the user applies a staged
bar/bin/missing-value chip. `FullProfile` is created by the explicit
`Create full-distribution query` action. Neither variant contains SQL or calls
the query API.

- [ ] **Step 4: Run profile tests and the full frontend unit suite**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked result_profile::tests
cargo test --manifest-path pchronicle-web/Cargo.toml --locked
```

Expected: all tests pass and the lockfile stays deterministic.

- [ ] **Step 5: Commit profiling**

```bash
git add pchronicle-web/Cargo.toml pchronicle-web/Cargo.lock pchronicle-web/src/main.rs pchronicle-web/src/result_profile.rs
git commit -m "feat(pchronicle-web): profile query result columns"
```

---

### Task 4: Build the Analyze-specific structured agent

**Files:**
- Create: `pchronicle-web/src/analysis_agent.rs`
- Modify: `pchronicle-web/src/main.rs:3-16`
- Test: inline `#[cfg(test)]` module in `pchronicle-web/src/analysis_agent.rs`

**Interfaces:**
- Consumes: `llm::complete`, `LlmConfig`, `QueryCatalog`, `AnalysisScope`, prior `AnalysisPlan`, optional `AnalysisRefinement`, `ColumnProfile`, and `QueryEvidence`.
- Produces: `generate_plan(PlanRequest) -> Result<AnalysisPlan, AnalysisAgentError>`.
- Produces: `interpret(InterpretationRequest) -> Result<AnalysisInterpretation, AnalysisAgentError>`.
- Produces: `build_evidence_digest(plan: &AnalysisPlan, scope: &AnalysisScope, evidence: &QueryEvidence, profiles: &[ColumnProfile]) -> EvidenceDigest`, capped at exactly 64 KiB serialized.
- Must not import or call `crate::api`; this compile-time boundary prevents model completion from executing SQL.

- [ ] **Step 1: Add failing strict-parser and digest tests**

```rust
#[test]
fn plan_parser_accepts_only_complete_structured_content() {
    let raw = r#"{
      "intent_summary":"Compare outcomes",
      "scope_summary":"current dataset",
      "filters":[],
      "groupings":["status"],
      "measures":["run count"],
      "expected_columns":["status","run_count"],
      "suggested_view":"distribution",
      "sql":"SELECT status, COUNT(*) AS run_count FROM default.runs GROUP BY status",
      "warnings":[]
    }"#;
    let plan = parse_plan_content(raw, 7, "compare outcomes").unwrap();
    assert_eq!(plan.id, 7);
    assert_eq!(plan.question, "compare outcomes");
    assert!(plan.sql.starts_with("SELECT"));
}

#[test]
fn plan_parser_rejects_markdown_wrapped_json() {
    let raw = "```json\n{\"sql\":\"SELECT 1\"}\n```";
    assert!(parse_plan_content(raw, 1, "question").is_err());
}

#[test]
fn evidence_digest_is_bounded_and_marks_truncation() {
    let huge = "轨".repeat(80_000);
    let evidence = evidence(vec![json!({"message": huge})], true);
    let digest = build_evidence_digest(&plan(), &scope(), &evidence, &profile_rows(&evidence.rows));
    let encoded = serde_json::to_vec(&digest).unwrap();
    assert!(encoded.len() <= EVIDENCE_DIGEST_BYTES);
    assert!(digest.digest_truncated);
    assert!(digest.query_truncated);
}
```

Add interpretation parsing tests that require separate `observations`, `inferences`, `limitations`, `follow_ups`, and `references`, plus a test that the plan system prompt contains the exact “never execute SQL” constraint and serialized catalog field descriptions.

In this test module, define literal fixture constructors named `catalog()`,
`scope()`, `plan()`, and
`evidence(rows: Vec<Value>, truncated: bool)`. `catalog()` contains one
`default.runs` table and concrete `status`/`run_id` field descriptions;
`evidence()` derives `returned_rows` from `rows.len()` and uses the interactive
budgets `max_rows = 100` and `max_bytes = 4 * 1024 * 1024`. This keeps every
test independent of private fixtures in other modules.

- [ ] **Step 2: Run the agent tests and verify they fail**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis_agent::tests
```

Expected: compilation fails because parsers, prompt builders, and digest types are missing.

- [ ] **Step 3: Implement plan generation with one repair and no tools**

Define owned requests so they can move into Dioxus tasks:

```rust
pub struct PlanRequest {
    pub config: LlmConfig,
    pub catalog: QueryCatalog,
    pub scope: AnalysisScope,
    pub question: String,
    pub plan_id: u64,
    pub previous_plan: Option<AnalysisPlan>,
    pub refinement: Option<AnalysisRefinement>,
}

pub async fn generate_plan(request: PlanRequest) -> Result<AnalysisPlan, AnalysisAgentError> {
    let system = plan_system_prompt(
        &request.catalog,
        &request.scope,
        request.previous_plan.as_ref(),
        request.refinement.as_ref(),
    )?;
    let messages = vec![json!({
        "role": "user",
        "content": serde_json::to_string(&json!({"question": request.question}))?,
    })];
    let content = request_json_content(&request.config, &system, messages).await?;
    match parse_plan_content(&content, request.plan_id, &request.question) {
        Ok(plan) => Ok(plan),
        Err(first_error) => {
            let repair_messages = vec![
                json!({"role":"user", "content": content}),
                json!({
                    "role":"user",
                    "content": format!(
                        "Return one corrected AnalysisPlan JSON object only. Validation error: {first_error}"
                    ),
                }),
            ];
            let repaired = request_json_content(&request.config, &system, repair_messages).await?;
            parse_plan_content(&repaired, request.plan_id, &request.question)
        }
    }
}
```

Define `request_json_content(config, system, messages) -> Result<String,
AnalysisAgentError>` to call `llm::complete` with `tools: None`, JSON response
format, and temperature `0.1`. If and only if
`suggests_response_format_unsupported()` is true, repeat that same request once
with `response_format: None`. Extract a non-empty string from `message.content`;
do not accept tool calls. `parse_plan_content` deserializes a private
`#[serde(deny_unknown_fields)] PlanPayload`, validates every required array,
requires SQL to start with `SELECT`, `WITH`, or `EXPLAIN` after trimming, then
copies the trusted frontend `plan_id` and `question` into `AnalysisPlan`.

Define these remaining owned contracts so no UI signal crosses the async
boundary:

```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceDigest {
    pub question: String,
    pub scope: AnalysisScope,
    pub sql: String,
    pub columns: Vec<String>,
    pub profiles: Vec<ColumnProfile>,
    pub rows: Vec<Value>,
    pub returned_rows: usize,
    pub query_truncated: bool,
    pub max_rows: usize,
    pub max_bytes: usize,
    pub digest_truncated: bool,
}

pub struct InterpretationRequest {
    pub config: LlmConfig,
    pub revision_id: u64,
    pub digest: EvidenceDigest,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnalysisAgentError {
    pub message: String,
}
```

`plan_system_prompt(...) -> Result<String, AnalysisAgentError>` serializes the
catalog/scope/prior context and includes the exact sentence `Never execute SQL;
only return an AnalysisPlan proposal.` `interpretation_system_prompt()` begins
with the marker `AnalysisInterpretation` (used by the deterministic browser
fixture) and requires all five arrays in the interpretation contract.

The plan prompt must include table names, descriptions, grain, fields and types; explicit scope coordinates; prior plan/refinement when present; server budgets; and a hard rule that the response only proposes SQL. Do not expose `query_sql` or any tool payload.

- [ ] **Step 4: Implement bounded digest and structured interpretation**

Use exact constants:

```rust
pub const EVIDENCE_DIGEST_BYTES: usize = 64 * 1024;
const SQL_DIGEST_CHARS: usize = 8 * 1024;
const CELL_DIGEST_CHARS: usize = 512;
const MAX_DIGEST_ROWS: usize = 50;
```

Build the digest by retaining scope, columns, profiles, returned/truncated/budget fields first; clamp SQL and individual cells at UTF-8 character boundaries; then append rows in stable result order until the serialized byte budget would be exceeded. Set `digest_truncated` whenever SQL, cells or rows are dropped.

`interpret` uses `tools=None`, requests JSON, and performs the same single repair attempt. Its system prompt forbids adding facts not present in the digest and requires limitations whenever `query_truncated` or `digest_truncated` is true.

- [ ] **Step 5: Verify parsers, digest and no API dependency**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis_agent::tests
rg -n 'crate::api|query_evidence' pchronicle-web/src/analysis_agent.rs
```

Expected: tests pass; `rg` prints no matches.

- [ ] **Step 6: Commit the structured agent**

```bash
git add pchronicle-web/src/analysis_agent.rs pchronicle-web/src/main.rs
git commit -m "feat(pchronicle-web): generate reviewable analysis plans"
```

---

### Task 5: Replace the SQL console with the question-and-confirm workspace

**Files:**
- Create: `pchronicle-web/src/analysis.rs`
- Create: `pchronicle-web/assets/analyze-workspace.css`
- Modify: `pchronicle-web/src/tools.rs:1-111`
- Modify: `pchronicle-web/src/main.rs:3-18`
- Modify: `pchronicle-web/src/workspace.rs:90-260`
- Modify: `pchronicle-web/index.html:6-18`
- Test: `pchronicle-web/src/analysis_session.rs` state tests plus `pchronicle-web/src/analysis.rs` pure view-model tests

**Interfaces:**
- Consumes: `QueryCatalog`, `AnalysisSession`, `analysis_agent::generate_plan`, `api::query_evidence_interactive`, and `llm_settings::LlmSettings`.
- Produces: `analysis::AnalysisWorkspace`.
- Temporary result rendering: existing `components::DataTable`; Task 6 replaces it with `ResultExplorer`.
- Route compatibility: keep `?page=tools` and the Analyze rail label; no external URL breaks in this task.

- [ ] **Step 1: Add failing view-model tests for primary actions and copy**

Keep UI event policy in a pure helper:

```rust
#[test]
fn plan_ready_exposes_run_but_never_auto_runs() {
    let model = AnalysisViewModel::from_revision(&plan_ready_revision());
    assert_eq!(model.primary_action, PrimaryAction::RunAnalysis);
    assert!(!model.query_in_flight);
    assert_eq!(model.sql_disclosure_label, "Advanced · view or edit SQL");
}

#[test]
fn manual_sql_is_marked_and_still_waits_for_run() {
    let mut revision = plan_ready_revision();
    revision.edit_sql("SELECT 1".into()).unwrap();
    let model = AnalysisViewModel::from_revision(&revision);
    assert!(model.manually_edited);
    assert_eq!(revision.state, RevisionState::PlanReady);
    assert!(revision.pending_effect.is_none());
}
```

Define `plan_ready_revision()` in this test module by constructing a draft
revision with a concrete dataset scope, transitioning through
`begin_plan_generation()` and `finish_plan(revision_id, plan)`, and asserting
the resulting state before returning it. Do not depend on another module's
private test helper.

- [ ] **Step 2: Run the focused tests and verify they fail**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis::tests
```

Expected: compilation fails because `AnalysisViewModel` and `AnalysisWorkspace` are missing.

- [ ] **Step 3: Implement the workspace shell and plan card**

Expose the component with stable props:

```rust
#[component]
pub fn AnalysisWorkspace(
    catalog: Option<QueryCatalog>,
    initial_scope: Option<AnalysisScope>,
    requested_session_id: Option<String>,
    on_session_change: EventHandler<String>,
) -> Element
```

Render these states with semantic controls:

- catalog loading/error;
- unconfigured LLM with settings action while preserving textarea text;
- question textarea, scope chips and `Generate plan`;
- three editable question starters: `Compare successful and failed runs in this scope`, `Find latency outliers and the tools associated with them`, and `Summarize explicit errors by tool and model`; choosing one only fills the textarea;
- `GeneratingPlan` progress;
- plan summary rows for scope/filter/grouping/measures;
- warnings;
- closed `<details>` SQL disclosure;
- `Manually edited` state after input changes;
- `Regenerate` and one primary `Run analysis` button;
- query busy/error and same-SQL retry.

Use one active revision ID in every async closure and apply results only through revision-ID-checked session methods. On Generate, call only `analysis_agent::generate_plan`. On Run, call `confirm_execution`, consume its `ExecuteSql` effect, then call `api::query_evidence_interactive` exactly once.

Replace `tools.rs` with the compatibility export:

```rust
pub use crate::analysis::AnalysisWorkspace as ToolsWorkspace;
```

Update `workspace.rs` call props and load `analyze-workspace.css` from `index.html`. Do not modify `components.rs` or existing run-detail `analysis.css`.

The Analyze settings disclosure must say that schema, the user's question, and
a bounded evidence digest are sent directly from the browser to the configured
model endpoint. Keep the API-key copy from the existing shared settings dialog.

- [ ] **Step 4: Render query results immediately with the existing table**

After a successful query, store `QueryEvidence` in the current runtime revision and render:

```rust
if let Some(evidence) = active_revision.evidence.clone() {
    DataTable {
        evidence,
        title: Some("Analysis result".into()),
    }
}
```

Do not call interpretation yet. For empty rows, use the state machine’s `Complete` path and show a problem-rewrite action.

- [ ] **Step 5: Verify the page compiles and existing query API tests stay green**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis::tests
cargo test --manifest-path pchronicle-web/Cargo.toml --locked tools::tests
cargo test --manifest-path pchronicle-web/Cargo.toml --locked api::tests
cargo build --manifest-path pchronicle-web/Cargo.toml --locked
```

Expected: tests/build pass; if the old `tools::tests` module was removed with the console, the focused command reports zero matching tests rather than a failure.

- [ ] **Step 6: Commit the explicit-confirmation workspace**

```bash
git add pchronicle-web/src/analysis.rs pchronicle-web/src/tools.rs pchronicle-web/src/main.rs pchronicle-web/src/workspace.rs pchronicle-web/assets/analyze-workspace.css pchronicle-web/index.html
git commit -m "feat(pchronicle-web): add Copilot analysis workspace"
```

---

### Task 6: Add the profile-enhanced Result Explorer and visual refinements

**Files:**
- Create: `pchronicle-web/src/result_explorer.rs`
- Modify: `pchronicle-web/src/main.rs:3-20`
- Modify: `pchronicle-web/src/analysis.rs`
- Modify: `pchronicle-web/assets/analyze-workspace.css`
- Test: inline tests in `pchronicle-web/src/result_explorer.rs`

**Interfaces:**
- Consumes: `QueryEvidence`, `Vec<ColumnProfile>`, selected column name, and revision ID.
- Produces: `ResultExplorer` events `on_stage_filter(RefinementIntent)` and `on_prepare_refinement(AnalysisRefinement)`.
- Produces: `identity_href(row) -> Option<ResultIdentity>` where `ResultIdentity` contains `run_href` and optional `turn_href`.
- Preserves: bounded cell preview, structured JSON expansion, maximum 16 visible columns, horizontal scroll, and server budget footer.

- [ ] **Step 1: Write failing identity and profile-scope tests**

```rust
#[test]
fn complete_coordinates_create_run_and_turn_links() {
    let row = json!({
        "dataset":"captures",
        "_file_":"nested/run.json",
        "run_id":"run-1",
        "agent_id":"agent-a",
        "session_id":"session-a",
        "root_session_id":"root-a",
        "turn_id":12
    });
    let identity = identity_href(&row).unwrap();
    assert!(identity.run_href.contains("page=detail"));
    assert!(identity.run_href.contains("session_id=session-a"));
    assert!(identity.turn_href.unwrap().contains("turn=12"));
}

#[test]
fn incomplete_coordinates_do_not_guess_a_link() {
    assert_eq!(identity_href(&json!({"session_id":"only"})), None);
}

#[test]
fn truncated_results_are_labeled_as_preview() {
    assert_eq!(
        profile_scope_label(&evidence(Vec::new(), 100, true)),
        "Preview distribution · 100 returned rows · truncated"
    );
}
```

Define `evidence(rows, returned_rows, truncated)` in the test module to return
`QueryEvidence` with the supplied values and interactive query budgets. The
zero-row call above therefore uses `evidence(Vec::new(), 100, true)` to model a
server response whose retained preview rows have already been removed for the
label-only unit test.

- [ ] **Step 2: Run Result Explorer tests and verify they fail**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked result_explorer::tests
```

Expected: compilation fails because `identity_href` and `profile_scope_label` do not exist.

- [ ] **Step 3: Implement the table with inline mini profiles**

Define:

```rust
#[component]
pub fn ResultExplorer(
    evidence: QueryEvidence,
    profiles: Vec<ColumnProfile>,
    revision_id: u64,
    on_stage_filter: EventHandler<RefinementIntent>,
    on_prepare_refinement: EventHandler<AnalysisRefinement>,
) -> Element
```

Each sticky `<th>` contains column name, inferred kind, mini bars/histogram, range or unique count, and missing percentage. Clicking the header selects the column and shows a detailed panel to the right. At narrower desktop widths, CSS moves the panel below the table.

Implement a Result Explorer-local bounded cell component so the currently modified `components.rs` remains untouched. Use `crate::json_value::JsonValue` for expanded arrays/objects. Keep constants aligned with existing `DataTable`:

```rust
const MAX_COLUMNS: usize = 16;
const MAX_CELL_CHARS: usize = 180;
```

- [ ] **Step 4: Implement deterministic refinement staging**

Categorical bars emit `Equals`; numeric histogram bars emit `NumericRange`; missing indicators emit `Missing`. A click only calls `on_stage_filter`, rendering a visible chip. The separate `Apply through Copilot` action wraps the staged intent as `AnalysisRefinement::Filter` and calls `on_prepare_refinement`; `analysis.rs` then creates a new draft revision and calls `generate_plan` with the prior plan, SQL and intent.

The detailed column panel also exposes `Create full-distribution query`. It
emits `AnalysisRefinement::FullProfile { source_revision_id, column,
column_kind }`; the agent proposes an aggregate `COUNT`, Top-K, or bucket query
and the new revision stops at `PlanReady`. The action never claims that the
current preview is full and never executes a query.

Do not locally rewrite SQL and do not call `api::query_evidence_interactive` from `result_explorer.rs`.

- [ ] **Step 5: Replace temporary DataTable and verify all result states**

In `analysis.rs`, compute profiles immediately after query success:

```rust
let profiles = profile_rows(&evidence.rows);
active_revision.finish_query(evidence, profiles)?;
```

Render `ResultExplorer` for non-empty evidence, a dedicated empty result for zero rows, and the query error state without clearing the last confirmed plan.

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked result_profile::tests
cargo test --manifest-path pchronicle-web/Cargo.toml --locked result_explorer::tests
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis::tests
cargo build --manifest-path pchronicle-web/Cargo.toml --locked
```

Expected: tests and build pass.

- [ ] **Step 6: Commit Result Explorer**

```bash
git add pchronicle-web/src/result_explorer.rs pchronicle-web/src/result_profile.rs pchronicle-web/src/analysis.rs pchronicle-web/src/main.rs pchronicle-web/assets/analyze-workspace.css
git commit -m "feat(pchronicle-web): explore result distributions"
```

---

### Task 7: Add automatic evidence interpretation and follow-up revisions

**Files:**
- Modify: `pchronicle-web/src/analysis.rs`
- Modify: `pchronicle-web/src/analysis_agent.rs`
- Modify: `pchronicle-web/src/analysis_session.rs`
- Modify: `pchronicle-web/src/result_explorer.rs`
- Modify: `pchronicle-web/assets/analyze-workspace.css`
- Test: inline tests in `analysis_session.rs` and `analysis_agent.rs`

**Interfaces:**
- Consumes: `AnalysisEffect::Interpret`, `build_evidence_digest`, `analysis_agent::interpret`.
- Produces: rendered `AnalysisInterpretation` with separate observations, inferences, limitations, references and follow-ups.
- Produces: `new_follow_up(question)` that creates a new revision sharing the immutable scope snapshot and prior-plan context.

- [ ] **Step 1: Add failing interpretation-state tests**

```rust
#[test]
fn query_rows_become_visible_before_interpretation_finishes() {
    let mut revision = executing_revision();
    let evidence = evidence(vec![json!({"status":"failed"})], false);
    let effect = revision.finish_query(revision.id, evidence.clone(), profile_rows(&evidence.rows)).unwrap();
    assert!(revision.evidence.is_some());
    assert_eq!(revision.state, RevisionState::Interpreting);
    assert_eq!(effect, Some(AnalysisEffect::Interpret { revision_id: revision.id }));
}

#[test]
fn interpretation_failure_keeps_query_evidence() {
    let mut revision = interpreting_revision();
    revision.fail_interpretation(revision.id, "provider unavailable".into()).unwrap();
    assert_eq!(revision.state, RevisionState::InterpretationError);
    assert!(revision.evidence.is_some());
}

#[test]
fn follow_up_creates_a_new_unexecuted_revision() {
    let session = complete_session();
    let next = session.new_follow_up("only failed runs").unwrap();
    assert_eq!(next.state, RevisionState::Draft);
    assert!(next.evidence.is_none());
    assert!(next.plan.is_none());
}
```

Define `scope`, `plan`, `evidence`, and `executing_revision` locally as in Task
2. Define `interpreting_revision` by completing a one-row query on that
executing revision, and define `complete_session` by applying a concrete
`AnalysisInterpretation` to it. These fixtures must transition through public
methods; do not mutate the state enum directly.

- [ ] **Step 2: Run the focused tests and verify they fail**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis_session::tests::query_rows_become_visible_before_interpretation_finishes
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis_session::tests::interpretation_failure_keeps_query_evidence
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis_session::tests::follow_up_creates_a_new_unexecuted_revision
```

Expected: at least one test fails because interpretation/follow-up transitions are incomplete.

- [ ] **Step 3: Trigger interpretation asynchronously after rendering evidence**

When `finish_query` returns `Interpret`, construct the digest synchronously, update the signal so Result Explorer renders, and only then spawn `analysis_agent::interpret`. Fence the completion with session and revision IDs. Do not remove evidence during `Interpreting` or `InterpretationError`.

Skip the model call for zero rows. For a successful interpretation, set `Complete`; for provider/parser errors, set `InterpretationError` and expose `Retry interpretation` without rerunning SQL.

- [ ] **Step 4: Render epistemically separated interpretation blocks**

Use distinct headings and styles:

```text
Observed in this result
Possible explanation
Coverage and limitations
Continue investigating
```

References with complete row identity use the same `ResultIdentity` deep links as the table. Unknown references render as plain labels. If query/digest truncation is true and the model omits limitations, prepend a deterministic frontend limitation notice.

Clicking a follow-up starts plan generation for a new revision; it never runs SQL. A user can edit the suggested question before generating by choosing `Edit question` instead.

- [ ] **Step 5: Verify evidence survives model failures**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis_session::tests
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis_agent::tests
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis::tests
cargo build --manifest-path pchronicle-web/Cargo.toml --locked
```

Expected: all tests pass; interpretation failure tests assert that rows/profiles remain available.

- [ ] **Step 6: Commit interpretation**

```bash
git add pchronicle-web/src/analysis.rs pchronicle-web/src/analysis_agent.rs pchronicle-web/src/analysis_session.rs pchronicle-web/src/result_explorer.rs pchronicle-web/assets/analyze-workspace.css
git commit -m "feat(pchronicle-web): interpret analysis evidence"
```

---

### Task 8: Wire session recovery, revision history and Run context entry

**Files:**
- Modify: `pchronicle-web/src/analysis.rs`
- Modify: `pchronicle-web/src/analysis_session.rs`
- Modify: `pchronicle-web/src/workspace.rs:70-390,675-735,1311-1385`
- Modify: `pchronicle-web/assets/analyze-workspace.css`
- Test: inline tests in `analysis_session.rs` and `workspace.rs`

**Interfaces:**
- Consumes: session storage adapter and `AnalysisScope::from_catalog/from_run/from_runs`.
- Produces: URL parameter `analysis_session=<id>`; preserves legacy `page=tools`.
- Produces: `analysis_href(&AnalysisScope)` usable by future Pinboard/Compare callers.
- Adds: `Analyze this run` to Run Detail; root/run coordinates are explicit scope chips.

- [ ] **Step 1: Add failing scope codec and stale-restore tests**

```rust
#[test]
fn multi_run_scope_round_trips_through_analyze_url() {
    let scope = AnalysisScope::from_runs(&catalog(), vec![run("left"), run("right")]);
    let href = analysis_href(&scope);
    let decoded = scope_from_query(href.split_once('?').unwrap().1).unwrap();
    assert_eq!(decoded.items, scope.items);
}

#[test]
fn restored_session_has_summaries_but_requires_rows_to_be_rerun() {
    let restored = restore_session(&serde_json::to_string(&complete_session()).unwrap()).unwrap();
    let revision = restored.active_revision().unwrap();
    assert!(revision.evidence.is_none());
    assert!(revision.needs_rerun);
}

#[test]
fn catalog_snapshot_change_marks_unexecuted_plan_stale() {
    let mut session = plan_ready_session("snapshot-a");
    session.reconcile_catalog("snapshot-b");
    assert_eq!(session.active_revision().unwrap().state, RevisionState::Stale);
}
```

Define local `catalog()`, `run(label)`, `scope()`, `plan()`,
`complete_session()`, and `plan_ready_session(snapshot_id)` fixtures with full
`RunSummary` coordinates. `complete_session()` must serialize an execution
summary and interpretation but also hold runtime evidence before serialization,
so the restore assertion proves the rows were discarded.

- [ ] **Step 2: Run scope/session tests and verify they fail**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis_session::tests::multi_run_scope_round_trips_through_analyze_url
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis_session::tests::restored_session_has_summaries_but_requires_rows_to_be_rerun
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis_session::tests::catalog_snapshot_change_marks_unexecuted_plan_stale
```

Expected: tests fail until URL codec and reconciliation are implemented.

- [ ] **Step 3: Integrate local session loading and bounded saving**

On Analyze mount:

1. derive the storage fingerprint from catalog database + storage path;
2. load the requested session ID if present;
3. otherwise create a session from incoming scope or catalog scope;
4. reconcile `snapshot_id`;
5. save after plan/query/interpretation/history mutations;
6. show a non-blocking notice when storage is unavailable.

Render a compact recent-session menu in the page header and a revision timeline below the active question. Old revisions show question, state, timestamp and returned row count. Selecting a restored completed revision displays saved summaries/interpretation plus `Rerun to restore rows`; it never reconstructs fake rows.

`Clear analysis history` removes only the current storage fingerprint’s analysis keys and asks for confirmation inside the Analyze page. It does not remove `pchronicle_llm_config` or `pchronicle_copilot:*`.

- [ ] **Step 4: Wire App route and Run context action**

Add App signals for `analysis_session_id` and optional `analysis_seed_scope`.
`analysis_href(scope)` produces a bootstrap URL with
`page=tools&analysis_scope=<percent-encoded JSON>`. On first mount,
`scope_from_query(search)` validates and consumes that bootstrap scope, creates
and persists a session, then `history.replaceState` rewrites the URL to
`page=tools&analysis_session=<id>`. Extend `sync_workspace_url` so subsequent
Analyze navigation retains only `analysis_session=<id>`; scope contents live in
the persisted session, not in a permanently huge URL.

Add to `RunDetailWorkspace` props:

```rust
on_analyze: EventHandler<RunSummary>
```

Render a secondary `Analyze this run` button next to `Ask Copilot`. Its handler builds `AnalysisScope::from_run(catalog, run)`, clears the requested session ID, seeds Analyze, and sets `page` to `tools`. The Analysis workspace immediately renders the run/root scope chips but still waits for a natural-language question.

Keep `analysis_href(&AnalysisScope)` public and tested for future multi-run/Compare callers. Do not add absent Pinboard/Compare source files.

- [ ] **Step 5: Verify URL, session and existing workspace behavior**

Run:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked analysis_session::tests
cargo test --manifest-path pchronicle-web/Cargo.toml --locked workspace::tests
cargo test --manifest-path pchronicle-web/Cargo.toml --locked
cargo build --manifest-path pchronicle-web/Cargo.toml --locked
```

Expected: all tests pass; existing Runs/Detail URLs still round-trip.

- [ ] **Step 6: Commit session integration**

```bash
git add pchronicle-web/src/analysis.rs pchronicle-web/src/analysis_session.rs pchronicle-web/src/workspace.rs pchronicle-web/assets/analyze-workspace.css
git commit -m "feat(pchronicle-web): persist analysis sessions"
```

---

### Task 9: Add deterministic browser fixture and complete desktop verification

**Files:**
- Create: `pchronicle-web/tests/fixtures/mock-openai.mjs`
- Modify: `pchronicle-web/assets/analyze-workspace.css`
- Modify: implementation files only when verification exposes a reproducible defect
- Test: full `pchronicle-web` unit/build suite and browser acceptance

**Interfaces:**
- Provides: local mock OpenAI-compatible endpoint at `127.0.0.1:9988/v1/chat/completions` with CORS.
- Consumes: release pChronicle server and `tmp/test/` data.
- Verifies: plan generation without query, explicit query execution, profiles/refinement, interpretation failure isolation, local recovery, console health.

- [ ] **Step 1: Create the deterministic mock provider**

Use this complete fixture:

```javascript
import http from "node:http";

const plan = {
  intent_summary: "Compare run outcomes by tool",
  scope_summary: "Current visible analysis scope",
  filters: [],
  groupings: ["status", "tool_name"],
  measures: ["run count", "average latency", "error rate"],
  expected_columns: ["status", "tool_name", "avg_latency_ms", "error_rate", "run_count"],
  suggested_view: "distribution",
  sql: "SELECT 'success' AS status, 'read_file' AS tool_name, 284.0 AS avg_latency_ms, 0.0 AS error_rate, 48 AS run_count UNION ALL SELECT 'failed', 'shell', 912.0, 0.25, 12 UNION ALL SELECT 'success', 'shell', 521.0, 0.05, 31",
  warnings: []
};

const interpretation = {
  observations: ["The returned rows contain more than one status and tool group."],
  inferences: ["Tool mix may help explain the observed outcome difference."],
  limitations: ["This interpretation is limited to the returned query evidence."],
  follow_ups: ["Only compare failed runs"],
  references: []
};

const server = http.createServer((request, response) => {
  response.setHeader("Access-Control-Allow-Origin", "*");
  response.setHeader("Access-Control-Allow-Headers", "authorization,content-type");
  response.setHeader("Access-Control-Allow-Methods", "POST,OPTIONS");
  if (request.method === "OPTIONS") {
    response.writeHead(204);
    response.end();
    return;
  }
  let raw = "";
  request.on("data", chunk => { raw += chunk; });
  request.on("end", () => {
    const body = JSON.parse(raw || "{}");
    const system = body.messages?.[0]?.content || "";
    const payload = system.includes("AnalysisInterpretation") ? interpretation : plan;
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(JSON.stringify({
      choices: [{ message: { role: "assistant", content: JSON.stringify(payload) } }]
    }));
  });
});

server.listen(9988, "127.0.0.1", () => {
  process.stdout.write("mock-openai http://127.0.0.1:9988/v1\n");
});
```

- [ ] **Step 2: Run fresh code-quality and unit verification**

Run:

```bash
git diff --check
cargo fmt --manifest-path pchronicle-web/Cargo.toml -- --check
cargo test --manifest-path pchronicle-web/Cargo.toml --locked
cargo build --manifest-path pchronicle-web/Cargo.toml --locked
```

Expected: zero failures. Record warnings separately; do not attribute unrelated dirty-file warnings to this feature.

- [ ] **Step 3: Build embedded assets and release CLI**

Run:

```bash
just chronicle-web-build
cargo build --release -p persisting-pchronicle-cli
```

Expected: both commands exit 0.

- [ ] **Step 4: Launch deterministic browser test services**

In separate sessions, run:

```bash
node pchronicle-web/tests/fixtures/mock-openai.mjs
./target/release/pchronicle serve --storage tmp/test/ --listen 127.0.0.1:9973
```

Open `http://127.0.0.1:9973/?page=tools`. Through Settings configure API base `http://127.0.0.1:9988/v1`, key `test-only`, and model `mock`.

- [ ] **Step 5: Verify the complete desktop workflow in the browser**

Perform and record these assertions:

1. Enter a problem and click `Generate plan`; a plan card appears and no `/api/query/evidence` request has occurred.
2. Advanced SQL is closed initially; opening/editing it shows `Manually edited` and still performs no query.
3. Regenerate the fixture plan and click `Run analysis`; exactly one query request occurs.
4. The result renders even while interpretation is pending.
5. Every visible column has a mini profile; selecting a column updates the detailed profile.
6. Clicking a category/bar creates a refinement chip but no query; `Apply through Copilot` creates a new plan awaiting confirmation.
7. Truncated results say `Preview distribution`; no total result count is invented.
8. Observations, possible explanations, limitations and follow-ups render separately.
9. Clicking a follow-up generates a new plan but does not run SQL.
10. Refresh restores the analysis session summary and shows `Rerun to restore rows` instead of persisted rows.
11. `Analyze this run` from Run Detail opens Analyze with visible run/root scope.
12. Browser console contains no new errors or warnings.

- [ ] **Step 6: Stop temporary services and commit the verified fixture/polish**

Stop only the test processes started on ports 9973 and 9988. Do not stop the user’s existing 9966 process.

```bash
git add pchronicle-web/tests/fixtures/mock-openai.mjs pchronicle-web/assets/analyze-workspace.css pchronicle-web/src/analysis.rs pchronicle-web/src/analysis_agent.rs pchronicle-web/src/analysis_session.rs pchronicle-web/src/result_explorer.rs pchronicle-web/src/result_profile.rs pchronicle-web/src/llm.rs pchronicle-web/src/llm_settings.rs pchronicle-web/src/agent.rs pchronicle-web/src/tools.rs pchronicle-web/src/main.rs pchronicle-web/src/workspace.rs pchronicle-web/index.html pchronicle-web/Cargo.toml pchronicle-web/Cargo.lock
git commit -m "test(pchronicle-web): verify Copilot analysis workflow"
```

Before committing, inspect `git diff --cached --name-status` and unstage any unrelated pre-existing modifications.

---

## Final Verification

Run fresh commands after the final implementation commit:

```bash
git diff --check
cargo fmt --manifest-path pchronicle-web/Cargo.toml -- --check
cargo test --manifest-path pchronicle-web/Cargo.toml --locked
cargo build --manifest-path pchronicle-web/Cargo.toml --locked
just chronicle-web-build
cargo build --release -p persisting-pchronicle-cli
```

Then review `git status --short` and distinguish this feature’s files from the user’s pre-existing dirty changes. Do not claim the whole working tree is clean unless it actually is.

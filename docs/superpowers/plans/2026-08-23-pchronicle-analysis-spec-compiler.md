# AnalysisSpec Compiler Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (this session) or superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Analyze compile an `AnalysisSpec` into SQL from live schema, with a single Analyze button and no LLM-written SQL on the main path.

**Architecture:** `ChronicleQueryEngine` introspects DataFusion tables for `/api/query/tables`. A pure `compile(spec, schema, scope)` in `persisting-pchronicle` emits SQL. CLI `POST /api/analysis/compile` runs compile + EXPLAIN. The WASM Analyze workspace generates/repairs Spec only, then executes compiled SQL through existing evidence query.

**Tech Stack:** Rust, DataFusion, Axum, Dioxus 0.7, existing analysis session state machine.

## Global Constraints

- SQL is a compilation artifact; the model must not write SQL on the main path.
- Catalog field names must equal engine schema field names; no `run_id_explicit` ghost column.
- v1 measures are first-class columns only; no JSON extraction, no run status, no token measure.
- Repair loop revises Spec at most twice; never replays failed SQL.
- Main button is Analyze; SQL is folded read-only; handwritten SQL is an escape hatch without Spec repair.
- Do not modify TTAS, queue, search, or dlcapt.
- Do not commit unless the user asks.

---

### Task 1: Live schema for `/api/query/tables`

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/query_engine.rs`
- Modify: `crates/persisting-pchronicle/src/query.rs`
- Modify: `crates/persisting-pchronicle-cli/src/server/mod.rs` (`query_tables`, `QueryTableSummary` owned strings)
- Test: `crates/persisting-pchronicle-cli/src/server/tests.rs`
- Update: existing assertions that expect unqualified `runs` / `run_id_explicit`

**Produces:** `ChronicleQueryEngine::introspect_tables() -> Result<Vec<IntrospectedTable>>`

- [x] Failing test: qualified `dataset.runs` contains `trajectory_id_explicit`, not `run_id_explicit`
- [x] Implement introspection; skip `information_schema` and bare public aliases
- [x] Wire `query_tables` to introspection + description map by table suffix
- [x] Update callers/tests; run `cargo test -p persisting-pchronicle-cli --lib query_tables`

### Task 2: `compile()` pure function

**Files:**
- Create: `crates/persisting-pchronicle/src/analysis_compile.rs`
- Modify: `crates/persisting-pchronicle/src/lib.rs` (`pub mod analysis_compile`)
- Test: `crates/persisting-pchronicle/src/analysis_compile.rs` (`mod tests`)

**Produces:** `compile(spec, schema, scope) -> Result<CompiledQuery, CompileError>`

- [x] Reject unknown intent, status/token/json measures, unknown columns
- [x] Snapshot SQL for each of the five intents
- [x] Deterministic whitespace-normalized SQL

### Task 3: `POST /api/analysis/compile`

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/server/mod.rs`
- Test: `crates/persisting-pchronicle-cli/src/server/tests.rs`
- Update: `crates/persisting-pchronicle-cli/tests/server_http_contract.rs` if the route matrix is exhaustive

**Produces:** compile + EXPLAIN; `snapshot_id` mismatch rejected; `engine_detail` truncated

### Task 4: Analyze workspace uses Spec

**Files:**
- Modify: `pchronicle-web/src/analysis_session.rs`, `analysis_agent.rs`, `analysis.rs`, `api.rs`
- Test: existing `analysis_session` / `analysis_agent` / `analysis` unit tests plus repair-loop test

**Produces:** single Analyze button; folded SQL; Spec repair ≤2; old plan.sql sessions stale; new starters

### Task 5: Verify

Run targeted tests, not workspace-wide. Confirm ghost column gone and Analyze copy has no Generate plan / Retry analysis.

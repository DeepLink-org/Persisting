# pChronicle Judge Removal and Panic Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove pChronicle's complete judgment vertical slice, eliminate production `unwrap` and `expect` calls from the pChronicle library, and make strict Clippy the default local and CI policy.

**Architecture:** Remove the judgment capability at every owning and consuming boundary instead of feature-gating or deprecating it. Harden the remaining pChronicle production paths with explicit errors or infallible synchronization primitives, then enforce both workspace warning denial and a production-only panic lint.

**Tech Stack:** Rust 2021, Cargo, Clippy, Tokio, Axum, DataFusion, Lance, Dioxus, mdBook, GitHub Actions, just.

## Global Constraints

- Existing `judgments.lance` directories must never be deleted or migrated.
- No compatibility stubs, deprecated judgment aliases, empty judgment fields, or always-failing judgment endpoints remain.
- Tests may retain assertion-oriented `unwrap` and `expect`; the production panic lint targets `persisting-pchronicle --lib` only.
- Search, TTAS, Queue, samplers, and `persisting-dlcapt` remain out of scope.
- `persisting-dlcapt` keeps its separate strict workflow and is excluded from the active-workspace Clippy command.
- No lint suppression is added for the twelve pChronicle production findings or pVisor's `type_complexity` finding.

---

## File Map

- `crates/persisting-pchronicle/src/{judgment.rs,judge_service.rs,judgment_summary.rs}`: delete judgment persistence, orchestration, and aggregation.
- `crates/persisting-pchronicle/src/operations/trajectory/{judge.rs,judge_stats.rs}`: delete typed judgment adapters.
- `crates/persisting-pchronicle/src/{lib.rs,messages.rs,layout/coords.rs,layout/mod.rs,operations/trajectory/mod.rs}`: remove public API, protocol, path, and stats integration.
- `crates/persisting-pchronicle/src/store/catalog/{discovery.rs,tests.rs}`: retain generic derived-Lance exclusion without judgment-specific names.
- `crates/persisting-pchronicle/Cargo.toml` and `Cargo.lock`: remove pChronicle's direct HTTP-client dependency.
- `crates/persisting-pchronicle-cli/src/server/{mod.rs,explorer.rs,tests.rs}` and `crates/persisting-pchronicle-cli/tests/server_http_contract.rs`: remove server and Explorer judgment contracts.
- `pchronicle-web/src/{model.rs,api.rs,agent.rs,workspace.rs,components.rs}` and `pchronicle-web/assets/workbench.css`: remove the Web judgment consumer and UI.
- `docs/src/pchronicle/**` and affected `docs/src/rfcs/*.md`: stop documenting judgment as an active capability.
- `crates/persisting-pchronicle/src/{convert/actf.rs,formats/actf.rs,formats/openai_corpus.rs,revision.rs}`: replace conversion and serialization panics.
- `crates/persisting-pchronicle/src/store/{index_build_gate.rs,root_write_lock.rs}` and `store/catalog/provider.rs`: replace synchronization and plan-selection panics.
- `crates/persisting-pvisor/src/cli/run.rs`: name the Chronicle sink tuple.
- `justfile` and `.github/workflows/ci.yml`: enforce strict Clippy and the pChronicle production panic lint.

---

### Task 1: Remove the Backend Judgment Vertical Slice

**Files:**
- Delete: `crates/persisting-pchronicle/src/judgment.rs`
- Delete: `crates/persisting-pchronicle/src/judge_service.rs`
- Delete: `crates/persisting-pchronicle/src/judgment_summary.rs`
- Delete: `crates/persisting-pchronicle/src/operations/trajectory/judge.rs`
- Delete: `crates/persisting-pchronicle/src/operations/trajectory/judge_stats.rs`
- Modify: `crates/persisting-pchronicle/src/lib.rs`
- Modify: `crates/persisting-pchronicle/src/messages.rs`
- Modify: `crates/persisting-pchronicle/src/layout/coords.rs`
- Modify: `crates/persisting-pchronicle/src/layout/mod.rs`
- Modify: `crates/persisting-pchronicle/src/operations/trajectory/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/catalog/discovery.rs`
- Modify: `crates/persisting-pchronicle/src/store/catalog/tests.rs`
- Modify: `crates/persisting-pchronicle/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/persisting-pchronicle-cli/src/server/mod.rs`
- Modify: `crates/persisting-pchronicle-cli/src/server/explorer.rs`
- Modify: `crates/persisting-pchronicle-cli/src/server/tests.rs`
- Modify: `crates/persisting-pchronicle-cli/tests/server_http_contract.rs`

**Interfaces:**
- Removes: all `Judge*`, `Judgment*`, `TrajectoryJudge*`, `SessionJudgeStats`, judgment path helpers, `judge_async`, `judge_stats_async`, and `/api/judgments`.
- Preserves: trajectory append, replay, stats, materialize, extract, revisions, catalog discovery, and generic derived-Lance filtering.

- [ ] **Step 1: Replace the read-only judgment test with a failing removed-route contract**

In `crates/persisting-pchronicle-cli/src/server/tests.rs`, replace the judgment read/write tests with this focused contract:

```rust
#[tokio::test]
async fn removed_judgments_route_returns_not_found() {
    use tower::ServiceExt;

    let root = json_dataset_root();
    let response = router(root.to_string_lossy().to_string())
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/judgments?agent_id=model-json&session_id=json-session")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    std::fs::remove_dir_all(root).unwrap();
}
```

- [ ] **Step 2: Run the route contract and verify RED**

Run:

```bash
cargo test -p persisting-pchronicle-cli removed_judgments_route_returns_not_found --locked
```

Expected: FAIL because the existing `/api/judgments` GET route returns a non-404 response.

- [ ] **Step 3: Delete judgment ownership from pChronicle**

Delete the five judgment implementation files. Remove their module declarations and re-exports from `lib.rs`; remove `judgment` from the crate-level ownership documentation. Remove the judgment types from `messages.rs`, including the `judge` field on `TrajectoryStatsResponse`.

Make `stats_async` return the ordinary response directly:

```rust
Ok(TrajectoryStatsResponse {
    dataset: layers.event_log_path,
    storage: request.storage,
    agent_id: request.agent_id,
    session_id: request.session_id,
    row_count: layers.event_rows,
    manifest_revision: RawEventLanceStore.stats(&session).await?.manifest_revision,
    duplicate_event_ids,
    status: if layers.event_rows > 0 { "ok" } else { "empty" }.into(),
    note: format!(
        "Canonical Lance event log: {} row(s){projection_note}",
        layers.event_rows
    ),
})
```

Remove `StoryCoords::lance_judgment_path`, `story_lance_judgment_path`, and their test. Remove pChronicle's direct `reqwest` dependency from its manifest and let Cargo refresh the package dependency list in `Cargo.lock`.

- [ ] **Step 4: Remove the CLI Server and Explorer contracts**

Remove `JudgeRow` and `read_judge_rows` imports, the `/api/judgments` route, `session_judgments`, and the `judgments` handler. Change Explorer functions to consume only runs, turns, and events:

```rust
pub(crate) fn run_page(
    mut records: Vec<RunSummary>,
    query: &ExplorerRunsQuery,
) -> RunExplorerPage

pub(crate) fn analyze(
    run: RunSummary,
    turns: &[StorylineTurn],
    events: &[EventRecord],
) -> RunAnalysis

pub(crate) fn turn_page(
    turns: &[StorylineTurn],
    events: &[EventRecord],
    q: Option<&str>,
    source: Option<&str>,
    offset: usize,
    limit: usize,
) -> ExplorerPage<TurnSummary>

pub(crate) fn turn_detail(
    item: &StorylineTurn,
    events: &[EventRecord],
) -> TurnDetail
```

Remove judgment counts, average scores, verdicts, and per-turn judgment arrays from the corresponding serialized response structs and fixtures. Remove the obsolete judgment integration assertion in `server_http_contract.rs`.

- [ ] **Step 5: Preserve generic derived-Lance discovery coverage**

Rename the catalog fixture from `judgments.lance` to `derived-metrics.lance`, and generalize the discovery comment:

```rust
// Derived Lance datasets are sidecars of a canonical Run, not trajectory
// sources. Never descend into their internal metadata and register it as an
// outer file source.
```

The test must still assert that only `trajectory.json` is registered.

- [ ] **Step 6: Run backend formatting, references, and targeted tests**

Run:

```bash
cargo fmt --all -- --check
! git grep -n -E 'Judge(Row|Scope|Method|Sample|Score|Stats|Rubric|Dialogue|Trajectory)|Judgment|judge_(async|stats|trajectory)|judgments\.lance|story_lance_judgment_path' -- crates/persisting-pchronicle crates/persisting-pchronicle-cli
cargo test -p persisting-pchronicle --lib --locked
cargo test -p persisting-pchronicle-cli --lib --tests --locked
```

Expected: all commands pass; the source scan prints no matches.

- [ ] **Step 7: Commit the backend removal**

```bash
git add Cargo.lock crates/persisting-pchronicle crates/persisting-pchronicle-cli
git commit -m "refactor: remove pchronicle judgment capability"
```

---

### Task 2: Remove the pChronicle Web Judgment Consumer

**Files:**
- Modify: `pchronicle-web/src/model.rs`
- Modify: `pchronicle-web/src/api.rs`
- Modify: `pchronicle-web/src/agent.rs`
- Modify: `pchronicle-web/src/workspace.rs`
- Modify: `pchronicle-web/src/components.rs`
- Modify: `pchronicle-web/assets/workbench.css`

**Interfaces:**
- Removes: `Judgment`, `api::judgments`, `judgment_review`, judgment props/state, and score/verdict/rubric UI.
- Preserves: run analysis, turn inspection, read-only SQL evidence, failure/latency/tool/cohort skills, and LLM-assisted analysis.

- [ ] **Step 1: Verify the Web source policy is RED**

Run:

```bash
! git grep -n -i -E 'judge|judgment' -- pchronicle-web/src pchronicle-web/assets
```

Expected: FAIL and print existing model, API, agent, workspace, component, CSS, and test references.

- [ ] **Step 2: Remove judgment data from the Web model and API**

Remove `Judgment`, judgment fields from `RunExplorerItem`, `RunAnalysis`, `TurnSummary`, and `TurnDetail`, and remove `api::judgments`. Keep all remaining wire fields unchanged.

- [ ] **Step 3: Remove judgment-aware agent behavior**

Remove `Judgment` imports, the `judgments` field from `AnswerRequest`, the `judgment_review` skill ID and match arm, and judgment arguments from `evidence_context` and `run_skill`. The remaining request shape is:

```rust
pub struct AnswerRequest<'a> {
    pub config: &'a LlmConfig,
    pub user_message: &'a str,
    pub run: &'a RunSummary,
    pub analysis: &'a RunAnalysis,
    pub turns: &'a [TurnSummary],
    pub selected: Option<&'a TurnDetail>,
    pub include_full_turn: bool,
}
```

Update agent tests so available skills and evidence assertions cover only retained sources.

- [ ] **Step 4: Remove judgment state and presentation from the workspace**

Remove judgment resource loading, refresh wiring, props, verdict/rubric helpers, score cards, per-turn judgment panels, and judgment-only tests. Remove CSS selectors that no retained component uses. Keep run overview, timeline, evidence, metrics, and Copilot layouts intact.

- [ ] **Step 5: Verify the standalone Web package**

Run:

```bash
cargo fmt --manifest-path pchronicle-web/Cargo.toml -- --check
! git grep -n -i -E 'judge|judgment' -- pchronicle-web/src pchronicle-web/assets
cargo test --manifest-path pchronicle-web/Cargo.toml --locked
cargo check --manifest-path pchronicle-web/Cargo.toml --locked
```

Expected: all commands pass and the source scan is empty.

- [ ] **Step 6: Commit the Web removal**

```bash
git add pchronicle-web
git commit -m "refactor: remove judgment from pchronicle web"
```

---

### Task 3: Update Active pChronicle Documentation

**Files:**
- Modify: `docs/src/pchronicle/concepts/facts-and-projections.md`
- Modify: `docs/src/pchronicle/concepts/facts-and-projections.zh.md`
- Modify: `docs/src/pchronicle/design/catalog.md`
- Modify: `docs/src/pchronicle/design/catalog.zh.md`
- Modify: `docs/src/pchronicle/design/trajectory-storage.md`
- Modify: `docs/src/pchronicle/design/trajectory-storage.zh.md`
- Modify: `docs/src/rfcs/0002-events-format.md`
- Modify: `docs/src/rfcs/0003-pchronicle-ownership.md`
- Modify: `docs/src/rfcs/0005-pchronicle-revision-lineage.md`

**Interfaces:**
- Removes: documentation claims that pChronicle executes, persists, serves, or displays judgments.
- Preserves: the generic distinction between canonical facts, rebuildable projections, and revision lineage.

- [ ] **Step 1: Verify the documentation policy is RED**

Run:

```bash
! git grep -n -i -E 'judge|judgment' -- docs/src
```

Expected: FAIL and list the current capability claims.

- [ ] **Step 2: Remove active judgment claims without weakening generic architecture**

Use neutral derived-data examples such as redaction, augmentation, enrichment, and export. In the ownership RFC remove the judgment persistence row and remove `judge` from the list of pChronicle workflows. In the revision RFC remove `judge` from the built-in kind examples while keeping extensible revision kinds. In the catalog docs describe the server as read-only without referring to a removed write API.

- [ ] **Step 3: Verify docs and commit**

Run:

```bash
! git grep -n -i -E 'judge|judgment' -- docs/src
git diff --check -- docs/src
```

Expected: both commands pass.

```bash
git add docs/src
git commit -m "docs: remove pchronicle judgment capability"
```

---

### Task 4: Eliminate pChronicle Production `unwrap` and `expect`

**Files:**
- Modify: `crates/persisting-pchronicle/src/convert/actf.rs`
- Modify: `crates/persisting-pchronicle/src/formats/actf.rs`
- Modify: `crates/persisting-pchronicle/src/formats/openai_corpus.rs`
- Modify: `crates/persisting-pchronicle/src/revision.rs`
- Modify: `crates/persisting-pchronicle/src/store/catalog/provider.rs`
- Modify: `crates/persisting-pchronicle/src/store/index_build_gate.rs`
- Modify: `crates/persisting-pchronicle/src/store/root_write_lock.rs`

**Interfaces:**
- Produces: zero `clippy::unwrap_used` and `clippy::expect_used` diagnostics for the default-feature pChronicle library.
- Preserves: existing conversion schemas, catalog plans, serialized revision representation, and single-index-build admission.

- [ ] **Step 1: Run the production panic lint and verify RED**

Run:

```bash
cargo clippy -p persisting-pchronicle --lib --locked -- \
  -D clippy::unwrap_used -D clippy::expect_used
```

Expected: FAIL with findings in ACTF conversion, ACTF validation, OpenAI corpus conversion, revision serialization, catalog planning, index admission, and root-lock registration.

- [ ] **Step 2: Replace conversion assumptions with explicit errors**

Use `ok_or_else(...)?` for ACTF provenance and serialized-object access. Bind ACTF observation IDs with `if let Some(referenced_id)` instead of checking and then expecting. Convert an OpenAI corpus row with:

```rust
let row = raw.as_object().ok_or_else(|| {
    Error::Other(format!(
        "OpenAI corpus {} row {} must be an object",
        relative_path, ordinal
    ))
})?;
```

Use existing error types and preserve the path/row context in every new message.

- [ ] **Step 3: Make revision serialization fallible**

Precompute both JSON string columns before `RecordBatch::try_new`:

```rust
let parent_revision_ids = rows
    .iter()
    .map(|row| serde_json::to_string(&row.parent_revision_ids))
    .collect::<serde_json::Result<Vec<_>>>()?;
let output_refs = rows
    .iter()
    .map(|row| serde_json::to_string(&row.output_refs))
    .collect::<serde_json::Result<Vec<_>>>()?;
```

Pass those vectors to `StringArray::from` without any unwrap.

- [ ] **Step 4: Remove storage-control panics**

Select the single catalog plan with a checked error:

```rust
1 => plans.pop().ok_or_else(|| {
    DataFusionError::Internal("Catalog planned one source but produced no plan".into())
})?,
```

Replace the index-build semaphore with a process-wide `tokio::sync::Mutex<()>`; `lock_owned().await` is infallible and exactly models single admission. Recover a poisoned root registry with:

```rust
let mut locks = locks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
```

- [ ] **Step 5: Verify GREEN and run pChronicle tests**

Run:

```bash
cargo fmt --all -- --check
cargo clippy -p persisting-pchronicle --lib --locked -- \
  -D clippy::unwrap_used -D clippy::expect_used
cargo test -p persisting-pchronicle --lib --locked
```

Expected: all commands pass with zero production panic diagnostics.

- [ ] **Step 6: Commit panic hardening**

```bash
git add crates/persisting-pchronicle
git commit -m "refactor: remove pchronicle production unwraps"
```

---

### Task 5: Enable Strict Clippy Locally and in CI

**Files:**
- Modify: `crates/persisting-pvisor/src/cli/run.rs`
- Modify: `justfile`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Produces: strict active-workspace Clippy and a repeatable pChronicle production panic check.
- Preserves: the independent strict `persisting-dlcapt` workflow.

- [ ] **Step 1: Run strict active-workspace Clippy and verify RED**

Run:

```bash
cargo clippy --workspace --exclude persisting-dlcapt --all-targets --locked -- -D warnings
```

Expected: FAIL on the Chronicle sink tuple in `persisting-pvisor/src/cli/run.rs` with `clippy::type_complexity`.

- [ ] **Step 2: Name the pVisor sink tuple**

Add this module-level alias near the Chronicle imports:

```rust
type ChronicleSinks = (
    Arc<dyn TrajectoryEventSink>,
    Arc<dyn crate::EventSink>,
    Option<ChronicleWriter>,
    Option<Arc<dyn persisting_events::ChronicleControl>>,
);
```

Use `let (sink, event_sink, writer, chronicle_control): ChronicleSinks = ...` without a lint suppression.

- [ ] **Step 3: Make local lint commands strict**

Define the recipes so `lint-rust` runs both guards:

```make
lint-rust: clippy-deny clippy-pchronicle-panics

clippy-deny:
    cargo clippy --workspace --exclude persisting-dlcapt --all-targets --locked -- -D warnings

clippy-pchronicle-panics:
    cargo clippy -p persisting-pchronicle --lib --locked -- -D warnings -D clippy::unwrap_used -D clippy::expect_used

clippy:
    just lint-rust
```

Remove comments that describe strict Clippy as unsafe before cleanup.

- [ ] **Step 4: Mirror both guards in CI**

Replace the ordinary Rust Clippy step with:

```yaml
- name: Rust clippy
  run: |
    cargo clippy --workspace --exclude persisting-dlcapt --all-targets --locked -- -D warnings
    cargo clippy -p persisting-pchronicle --lib --locked -- -D warnings -D clippy::unwrap_used -D clippy::expect_used
```

- [ ] **Step 5: Verify strict lint locally**

Run:

```bash
cargo fmt --all -- --check
just lint-rust
```

Expected: both commands pass with no warnings.

- [ ] **Step 6: Commit strict lint enforcement**

```bash
git add crates/persisting-pvisor/src/cli/run.rs justfile .github/workflows/ci.yml
git commit -m "ci: deny active workspace clippy warnings"
```

---

### Task 6: Final Cross-Boundary Verification

**Files:**
- Verify only; modify a prior task's owning files if a failure exposes an omission.

**Interfaces:**
- Confirms: the complete removal, panic policy, tests, and strict lint policy work together.

- [ ] **Step 1: Verify removal and dependency boundaries**

Run:

```bash
! git grep -n -i -E 'judge|judgment' -- crates/persisting-pchronicle crates/persisting-pchronicle-cli pchronicle-web/src pchronicle-web/assets docs/src
! rg -n '^reqwest\s*=' crates/persisting-pchronicle/Cargo.toml
cargo tree -p persisting-pchronicle --depth 1 --locked
```

Expected: both scans pass; the direct dependency tree does not list `reqwest` as a pChronicle dependency. Transitive HTTP dependencies brought by Lance are allowed.

- [ ] **Step 2: Run all targeted tests**

Run:

```bash
cargo test -p persisting-pchronicle --lib --locked
cargo test -p persisting-pchronicle-cli --lib --tests --locked
cargo test --manifest-path pchronicle-web/Cargo.toml --locked
```

Expected: all tests pass with zero failures.

- [ ] **Step 3: Run all formatting and lint gates**

Run:

```bash
cargo fmt --all -- --check
cargo fmt --manifest-path pchronicle-web/Cargo.toml -- --check
just lint-rust
git diff --check
```

Expected: all commands exit zero and Clippy prints no warnings.

- [ ] **Step 4: Review the final diff and status**

Run:

```bash
git status --short
git diff --stat HEAD~5..HEAD
git log -5 --oneline
```

Confirm that unrelated untracked user files remain untouched and every tracked change belongs to this design.

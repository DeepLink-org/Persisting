# Persisting Replay Adapter Module Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Physically move every Agent-specific parser and executor out of `adapter/mod.rs` into its versioned Agent module without changing replay behavior or public contracts.

**Architecture:** `adapter/mod.rs` remains the static dispatcher and owns only shared context, helpers, and the existing Mini/SWE SDK bridge. Each Agent module owns its native parsing, prefix preparation, execution, reconstruction helpers, terminal interpretation, and focused tests.

**Tech Stack:** Rust 2024, serde/serde_json, Cargo test and Clippy.

**Spec:** `docs/superpowers/specs/2026-08-22-persisting-replay-adapter-module-split-design.md`

## Global Constraints

- Preserve `ReplayPlan`, `AdapterPlan`, `PlaybackRequest`, `ReplayOutcome`, and all serialized request/result contracts.
- Preserve every artifact filename and runtime command/environment variable.
- Do not redesign `run_sdk_bridge`; it remains shared by Mini and SWE.
- Use explicit imports in Agent modules; do not retain `use super::*`.
- Do not modify Gateway, pChronicle, Queue, Search, TTAS, or `persisting-dlcapt`.
- Stage only replay adapter files and this plan.

---

### Task 1: Move OpenHands implementation

**Files:**
- Modify: `crates/persisting-replay/src/adapter/mod.rs`
- Modify: `crates/persisting-replay/src/adapter/openhands.rs`

**Interfaces:**
- Consumes: `RunContext`, `check_boundary`, `prepared_outcome`, `agent_command`, `MAX_TOOL_OUTPUT_BYTES`, `run_process`, and common IO/error/model types.
- Produces: the existing `openhands::build` and `openhands::execute` signatures with all OpenHands implementation private to `openhands.rs`.

- [ ] **Step 1: Record the focused characterization baseline**

Run:

```bash
cargo test -p persisting-replay openhands_ -- --nocapture
cargo test -p persisting-replay --test replay_contract openhands_ -- --nocapture
```

Expected: all selected tests pass before movement.

- [ ] **Step 2: Move the OpenHands parser and executor**

Move these functions unchanged into `openhands.rs`, below the existing public-to-parent entrypoints:

```text
build_openhands_plan
openhands_action_signature
openhands_reconstructed_tool_metadata
openhands_reconstructed_tool_arguments
event_id
run_openhands
openhands_fatal_controller_marker
openhands_observation_content
openhands_complete_batches
prepend_openhands_runtime_tools
```

Change the entrypoints to call module-local functions:

```rust
pub(super) fn build(request: &PlaybackRequest) -> Result<AdapterPlan, ReplayError> {
    build_openhands_plan(request).map(AdapterPlan::Openhands)
}

pub(super) fn execute(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    run_openhands(plan, context, journal)
}
```

Move the five `openhands_*` unit tests from the root test module into a local `#[cfg(test)] mod tests` in `openhands.rs`.

- [ ] **Step 3: Verify OpenHands after movement**

Run the two Step 1 commands again, then:

```bash
cargo check -p persisting-replay
```

Expected: all commands exit zero.

- [ ] **Step 4: Commit the OpenHands split**

```bash
git add crates/persisting-replay/src/adapter/mod.rs crates/persisting-replay/src/adapter/openhands.rs
git commit -m "refactor(replay): move OpenHands adapter implementation"
```

### Task 2: Move Mini implementation

**Files:**
- Modify: `crates/persisting-replay/src/adapter/mod.rs`
- Modify: `crates/persisting-replay/src/adapter/mini_swe_agent.rs`

**Interfaces:**
- Consumes: shared `check_boundary`, `prepared_outcome`, and `run_sdk_bridge`.
- Produces: module-local Mini parsing/preparation with unchanged `mini_swe_agent::build` and `mini_swe_agent::execute` entrypoints.

- [ ] **Step 1: Record the Mini characterization baseline**

```bash
cargo test -p persisting-replay mini_ -- --nocapture
cargo test -p persisting-replay --test replay_contract mini_ -- --nocapture
```

Expected: all selected tests pass.

- [ ] **Step 2: Move Mini-specific code**

Move these functions unchanged into `mini_swe_agent.rs`:

```text
mini_reasoning
mini_batch_signature
build_mini_plan
mini_submission_in_prefix
mini_calls
mini_observation
run_mini
```

Keep `run_sdk_bridge` in `adapter/mod.rs`; expose it as `pub(super)` so the
module-local `run_mini` can call:

```rust
run_sdk_bridge(plan, context, journal, AgentKind::MiniSweAgent)
```

Move `mini_submit_is_rejected_only_inside_the_selected_prefix` into the Mini
module test block. Move version-probe and portable-runtime tests to
`adapter/runtime.rs`, because they test runtime resolution rather than native
Mini trajectory behavior.

- [ ] **Step 3: Verify and commit Mini**

```bash
cargo test -p persisting-replay mini_ -- --nocapture
cargo test -p persisting-replay --test replay_contract mini_ -- --nocapture
cargo check -p persisting-replay
git add crates/persisting-replay/src/adapter/mod.rs crates/persisting-replay/src/adapter/mini_swe_agent.rs crates/persisting-replay/src/adapter/runtime.rs
git commit -m "refactor(replay): move Mini adapter implementation"
```

Expected: tests and check pass before the commit.

### Task 3: Move SWE implementation

**Files:**
- Modify: `crates/persisting-replay/src/adapter/mod.rs`
- Modify: `crates/persisting-replay/src/adapter/swe_agent.rs`

**Interfaces:**
- Consumes: shared `check_boundary`, `prepared_outcome`, and `run_sdk_bridge`.
- Produces: module-local SWE parser, asset resolution, and prefix preparation.

- [ ] **Step 1: Record the SWE characterization baseline**

```bash
cargo test -p persisting-replay --test replay_contract swe_ -- --nocapture
```

Expected: the SWE total-budget contract test passes.

- [ ] **Step 2: Move SWE-specific code**

Move these functions unchanged into `swe_agent.rs`:

```text
build_swe_plan
resolve_swe_problem_asset
run_swe
```

The module-local executor continues to call exactly:

```rust
run_sdk_bridge(plan, context, journal, AgentKind::SweAgent)
```

Do not move or rewrite any Mini/SWE result parsing in `run_sdk_bridge` during
this task.

- [ ] **Step 3: Verify and commit SWE**

```bash
cargo test -p persisting-replay --test replay_contract swe_ -- --nocapture
cargo check -p persisting-replay
git add crates/persisting-replay/src/adapter/mod.rs crates/persisting-replay/src/adapter/swe_agent.rs
git commit -m "refactor(replay): move SWE adapter implementation"
```

Expected: tests and check pass before the commit.

### Task 4: Move Claude implementation and tests

**Files:**
- Modify: `crates/persisting-replay/src/adapter/mod.rs`
- Modify: `crates/persisting-replay/src/adapter/claude_code.rs`

**Interfaces:**
- Consumes: shared `check_boundary`, `required_str`, `agent_command`, and process/IO/error types.
- Produces: all Claude parsing, tool execution, reconstruction, resume cleanup, and Claude unit tests inside `claude_code.rs`.

- [ ] **Step 1: Record the Claude characterization baseline**

```bash
cargo test -p persisting-replay claude_ -- --nocapture
cargo test -p persisting-replay stale_observations_ -- --nocapture
cargo test -p persisting-replay prepare_only_executes_no_historical_tool -- --nocapture
cargo test -p persisting-replay bash_ -- --nocapture
cargo test -p persisting-replay wildcard_ -- --nocapture
```

Expected: all selected tests pass.

- [ ] **Step 2: Move all Claude-specific implementation**

Move the Claude code ranges beginning with `claude_boundary_tool_use_ids` and
`build_claude_plan`, plus `run_claude` through its native tool/reconstruction
helpers, into `claude_code.rs`. The resulting module must own these groups:

```text
Claude canonical-message and active-chain parsing
Claude ToolUse/ToolResult batch parsing
Claude tool policy and historical tool execution
Bash/Edit/Read/Glob/Grep replay helpers and wildcard traversal
Claude native session rebuilding and continuation cleanup
Resume Transport attachment, UUID, nonce, and parent-chain validation
Claude max-turn terminal-result validation
```

Move every root unit test whose name starts with `claude_`, `stale_`, `bash_`,
or `wildcard_`, plus `prepare_only_executes_no_historical_tool`, into the
Claude module test block. Keep `direct_agents_keep_model_credentials_but_claude_tools_do_not`
in `adapter/mod.rs` because it tests the shared environment policy.

- [ ] **Step 3: Remove obsolete root imports and verify structural boundaries**

Remove imports and constants from `adapter/mod.rs` that are now used only by
Claude. Confirm the forbidden functions no longer exist in the root:

```bash
rg -n 'fn (build_(claude|mini|openhands|swe)_plan|run_(claude|mini|openhands|swe))' crates/persisting-replay/src/adapter/mod.rs
```

Expected: no matches.

- [ ] **Step 4: Verify and commit Claude**

Run every Step 1 command, then:

```bash
cargo check -p persisting-replay
cargo fmt --check -p persisting-replay
git add crates/persisting-replay/src/adapter/mod.rs crates/persisting-replay/src/adapter/claude_code.rs
git commit -m "refactor(replay): move Claude adapter implementation"
```

Expected: all commands exit zero.

### Task 5: Full regression and scope verification

**Files:**
- Verify only; modify adapter files only if a verification failure was introduced by this refactor.

**Interfaces:**
- Consumes: all four physically split adapters.
- Produces: evidence that the refactor preserved behavior and repository scope.

- [ ] **Step 1: Run full replay verification**

```bash
cargo fmt --check -p persisting-replay
cargo test -p persisting-replay
cargo clippy -p persisting-replay --all-targets -- -D warnings
git diff --check
```

Expected: every command exits zero; the test output includes all replay unit,
contract, and doc tests with zero failures.

- [ ] **Step 2: Inspect the final module sizes and boundaries**

```bash
wc -l crates/persisting-replay/src/adapter/*.rs
rg -n 'fn (build_(claude|mini|openhands|swe)_plan|run_(claude|mini|openhands|swe))' crates/persisting-replay/src/adapter/mod.rs
git status --short
```

Expected: all four Agent files contain substantial implementations, the `rg`
command returns no matches, and no Gateway/pChronicle/storyline or `.workbuddy`
path is staged by this work.

- [ ] **Step 3: Commit any verification-only import cleanup**

If Step 1 required an adapter import or visibility cleanup, stage only those
adapter files and commit:

```bash
git add crates/persisting-replay/src/adapter
git commit -m "refactor(replay): finish physical adapter split"
```

If no cleanup was required, do not create an empty commit.

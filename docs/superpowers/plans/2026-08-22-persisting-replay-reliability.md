# Persisting Replay Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ]) syntax for tracking.

**Goal:** Make prepare, replay, and continuation semantics reliable across all four supported Agents while bounding child processes and publishing an explicit v3 result contract.

**Architecture:** The common engine owns phase transitions, state/output lifecycle, results, journaling, and process supervision. Static Agent adapters own version-pinned native parsing and execution; behavioral coverage lands before the monolithic adapter is split.

**Tech Stack:** Rust 2021, serde/serde_json, clap, tokio/axum/reqwest, Unix process groups through libc, and pinned Python runners for mini-swe-agent and SWE-agent.

**Spec:** docs/superpowers/specs/2026-08-22-persisting-replay-reliability-design.md

## Global Constraints

- Keep Claude Code at 2.1.220, mini-swe-agent at 2.4.6, OpenHands at 0.53.0, and SWE-agent at 1.1.0.
- Do not add dynamic adapter loading, Gateway capture, pChronicle storage, or model-traffic capture.
- Do not modify TTAS, Queue, Search, or persisting-dlcapt.
- max_steps means total Agent action/model steps including the replay prefix.
- Write and observe a failing regression test before every production behavior change.
- Preserve unrelated .workbuddy and worktree changes.

---

## File Map

- crates/persisting-replay/src/model.rs: request modes and v3 result/status types.
- crates/persisting-replay/src/config.rs: strict TOML/JSON mapping.
- crates/persisting-replay/src/process.rs: bounded process-group supervisor.
- crates/persisting-replay/src/adapter/: static dispatch and Agent-specific plans.
- crates/persisting-replay/src/engine.rs: validated phase state machine and result publication.
- crates/persisting-replay/src/journal.rs: conservative ambiguity detection.
- crates/persisting-replay/assets/: native replay runners.
- crates/persisting-replay/tests/replay_contract.rs: fake-runtime integration contract.
- crates/persisting-pvisor/src/cli/: public CLI and error output.
- pVisor README and replay docs: migration documentation.

---

### Task 1: Explicit replay modes and request compatibility

**Files:**
- Modify: crates/persisting-replay/src/model.rs
- Modify: crates/persisting-replay/src/config.rs
- Modify: crates/persisting-replay/src/lib.rs
- Modify: crates/persisting-pvisor/src/cli/replay.rs
- Modify: crates/persisting-pvisor/src/cli/mod.rs

**Interfaces:**
- Produces ReplayMode with PrepareOnly, ReplayOnly, and ReplayAndContinue.
- Produces PlaybackRequest.mode and PlaybackRequest.allow_stale_observations.

- [ ] **Step 1: Write failing mode tests**

Add config tests using the desired API:

    assert_eq!(prepare.into_request(cwd)?.mode, ReplayMode::PrepareOnly);
    assert_eq!(replay.into_request(cwd)?.mode, ReplayMode::ReplayOnly);
    assert!(both.into_request(cwd).is_err());

Add clap tests accepting each flag, rejecting both, and proving managed replay propagates prepare-only and allow-stale-observations.

- [ ] **Step 2: Verify RED**

    cargo test -p persisting-replay config::tests
    cargo test -p persisting-pvisor cli::replay

Expected: compile/test failure because the new mode and flags do not exist.

- [ ] **Step 3: Implement the request contract**

Add a serde snake_case ReplayMode enum. Replace PlaybackRequest.replay_only. Add strict prepare_only and allow_stale_observations fields to TOML and v1 JSON. Map legacy replay_only=true to corrected ReplayOnly, reject both booleans, and default to live continuation. Add conflicting clap flags and managed-command propagation.

- [ ] **Step 4: Update request literals and verify GREEN**

Use mode=PrepareOnly and allow_stale_observations=false in existing prepare-only tests, then rerun Step 2.

- [ ] **Step 5: Commit**

    git commit -m "feat(replay): separate prepare replay and continuation modes"

---

### Task 2: V3 results and failure locations

**Files:**
- Modify: crates/persisting-replay/src/model.rs
- Modify: crates/persisting-replay/src/error.rs
- Modify: crates/persisting-replay/src/engine.rs
- Modify: crates/persisting-pvisor/src/cli/replay.rs

**Interfaces:**
- Produces ReplayPhase, ReplayQuality, AgentStatus, and ReplayFailure.
- Produces ExecutionReport containing ReplayResult and exit_code.

- [ ] **Step 1: Write failing serialization tests**

Assert representative JSON contains:

    assert_eq!(value["schema_version"], "sandbox-playback.result/v3");
    assert_eq!(value["phase"], "replayed");
    assert_eq!(value["quality"], "degraded");
    assert_eq!(value["agent_status"], "not_started");
    assert_eq!(value["failure"], Value::Null);

Add a CLI helper test requiring run_id, state_dir, and output_dir in failure JSON.

- [ ] **Step 2: Verify RED**

    cargo test -p persisting-replay model::tests
    cargo test -p persisting-pvisor cli::replay::tests::failure_json_keeps_run_locations

- [ ] **Step 3: Implement typed v3 results**

Add snake-case enums for Prepared/Replayed/Continued, Verified/Degraded, and Completed/MaxSteps/Failed/NotStarted. Replace string statuses, set RESULT_SCHEMA_VERSION to v3, add both roots and optional structured failure, and retain partial artifacts on runtime failure.

- [ ] **Step 4: Update CLI and verify GREEN**

Print ExecutionReport.result for success and execution failure, returning its exit code. Pre-execution errors use a structured envelope containing every known location.

- [ ] **Step 5: Commit**

    git commit -m "feat(replay): publish structured v3 execution results"

---

### Task 3: Exact runtime versions and portable runtime paths

**Files:**
- Create: crates/persisting-replay/src/adapter/mod.rs
- Create: crates/persisting-replay/src/adapter/runtime.rs
- Modify/delete: crates/persisting-replay/src/adapter.rs

**Interfaces:**
- Produces resolve_launch_spec(PlaybackRequest).
- Produces parse_version(AgentKind, output) with exact matching.

- [ ] **Step 1: Write failing exact-version tests**

    assert_eq!(parse_version(ClaudeCode, "2.1.220 (Claude Code)"), Some("2.1.220"));
    assert_eq!(parse_version(ClaudeCode, "12.1.220"), None);
    assert_eq!(parse_version(Openhands, "0.53.0\n"), Some("0.53.0"));
    assert_eq!(parse_version(Openhands, "warning 0.53.0 actual 0.54.0"), None);

Canonicalize the expected mini Python path before equality assertions.

- [ ] **Step 2: Verify RED**

    cargo test -p persisting-replay adapter::runtime::tests::version_probes_require_exact_banners

Expected: the old substring matcher accepts the wrong Claude banner.

- [ ] **Step 3: Extract and implement runtime parsing**

Move LaunchSpec, runtime manifest handling, safe relative paths, version probes, and mini Python discovery/configuration into adapter/runtime.rs. Require an exact first Claude version token, exact trimmed metadata output for OpenHands/SWE-agent, and the exact mini banner.

- [ ] **Step 4: Verify GREEN**

    cargo test -p persisting-replay adapter::runtime
    cargo clippy -p persisting-replay --all-targets -- -D warnings

- [ ] **Step 5: Commit**

    git commit -m "refactor(replay): isolate exact agent runtime resolution"

---

### Task 4: Bounded process-group supervisor

**Files:**
- Create: crates/persisting-replay/src/process.rs
- Modify: crates/persisting-replay/src/lib.rs

**Interfaces:**
- Produces run_process(ProcessSpec) returning ProcessOutput.
- ProcessOutput carries status, bounded stdout/stderr tails, byte totals, truncation, timeout, and background cleanup state.

- [ ] **Step 1: Read test guidance and write regression tests**

Read superpowers/test-driven-development/writing-good-tests.md. Add Unix tests that produce 8 MiB with a 64 KiB retained cap, start sleep 30 in the background, and time out a foreground process. Assert complete log draining, bounded retained bytes, prompt return, and no surviving process-group member.

- [ ] **Step 2: Verify RED**

    cargo test -p persisting-replay process::tests -- --nocapture

- [ ] **Step 3: Implement streaming supervision**

Use a dedicated Unix process group. Reader threads stream all chunks to an owner-only log and retain only the configured amount. Poll the leader, enforce timeout, terminate the negative PGID with TERM then KILL, reap the leader, and kill a group whose pipes remain open after leader exit. Do not use read_to_end, Command::output, or wait_with_output.

- [ ] **Step 4: Verify GREEN**

    cargo test -p persisting-replay process::tests -- --nocapture
    cargo clippy -p persisting-replay --all-targets -- -D warnings

- [ ] **Step 5: Commit**

    git commit -m "feat(replay): supervise child process groups with bounded output"

---

### Task 5: Verified Claude replay by default

**Files:**
- Create: crates/persisting-replay/src/adapter/claude.rs
- Modify: crates/persisting-replay/src/adapter/mod.rs
- Modify: crates/persisting-replay/src/engine.rs
- Modify: crates/persisting-replay/src/process.rs

**Interfaces:**
- Produces private ClaudePlan and phase-oriented prepare/replay/continue functions.
- Consumes allow_stale_observations and the process supervisor.

- [ ] **Step 1: Write failing stale-observation tests**

Use a prefix containing Agent(Explore), TaskOutput, and Task/Todo state calls. Assert default validation fails before execution. With opt-in, assert Degraded quality and per-call degradation=stale_source_observation. Assert Find is rejected at plan time.

- [ ] **Step 2: Verify RED**

    cargo test -p persisting-replay adapter::claude::tests::stale_observations_fail_closed_by_default
    cargo test -p persisting-replay adapter::claude::tests::stale_observations_are_explicitly_degraded

- [ ] **Step 3: Split Claude code and enforce quality**

Move Claude parsing, chain reconstruction, tools, and continuation into adapter/claude.rs. Classify tools as fresh, stale-opt-in, or unsupported. Prepare-only keeps the selected source prefix without execution; replay-only executes and rebuilds without starting the bridge; live mode continues from replayed state.

- [ ] **Step 4: Integrate supervision**

Replace Bash readers and Claude wait_with_output with run_process. Retain at most 4 MiB for observations, keep full owner-only logs, propagate truncation, and report terminated background descendants as an error observation.

- [ ] **Step 5: Verify and commit**

    cargo test -p persisting-replay adapter::claude
    cargo test -p persisting-replay claude_resume
    cargo test -p persisting-replay claude_bridge
    git commit -m "fix(replay): fail closed on stale Claude observations"

---

### Task 6: True mini-swe-agent and SWE-agent replay-only execution

**Files:**
- Create: crates/persisting-replay/src/adapter/mini_swe.rs
- Create: crates/persisting-replay/src/adapter/swe_agent.rs
- Modify: crates/persisting-replay/src/adapter/mod.rs
- Modify: crates/persisting-replay/assets/mini_swe_agent_runner.py
- Modify: crates/persisting-replay/assets/swe_agent_runner.py
- Create: crates/persisting-replay/tests/replay_contract.rs
- Create: crates/persisting-replay/tests/fixtures/fake_agent_runtime.py

**Interfaces:**
- Runner request gains mode and max_steps.
- Runner result gains phase, agent_status, replayed_steps, and continued_steps.

- [ ] **Step 1: Write failing fake-runtime tests**

Prove prepare-only starts no runtime; replay-only executes exactly after_step actions and zero live calls; live mode performs no more than max_steps-after_step live calls; replay-only without runtime fails before a workspace marker is written.

- [ ] **Step 2: Verify RED**

    cargo test -p persisting-replay --test replay_contract mini_replay_only_executes_prefix_without_live_model
    cargo test -p persisting-replay --test replay_contract swe_max_steps_caps_total_actions

- [ ] **Step 3: Update mini runner**

After the historical loop and observation write, save and return a structured replay result in replay-only mode. Call continuation only in live mode. Preserve prefix n_calls and set the native step_limit to total max_steps.

- [ ] **Step 4: Update SWE-agent runner with a bounded loop**

Pass mode and max_steps. Wrap the pinned DefaultAgent so its run calls setup, then step no more than the total budget while saving after each step. ReplayThenLiveModel supplies the source prefix. Stop before the first live query in replay-only; emit MaxSteps when live mode reaches the cap. Use SWE-agent 1.1.0 APIs setup, step, save_trajectory, get_trajectory_data, and AgentRunResult. Reject retry-agent configs before side effects.

- [ ] **Step 5: Split adapters and use supervision**

Move parsers and runner result interpretation into focused modules. Replace Command::output with run_process and parse a structured runner result file.

- [ ] **Step 6: Verify and commit**

    cargo test -p persisting-replay --test replay_contract mini_
    cargo test -p persisting-replay --test replay_contract swe_
    cargo test -p persisting-replay adapter::mini_swe
    cargo test -p persisting-replay adapter::swe_agent
    git commit -m "fix(replay): execute SDK prefixes without unwanted continuation"

---

### Task 7: OpenHands replay boundary and fatal status

**Files:**
- Create: crates/persisting-replay/src/adapter/openhands.rs
- Modify: crates/persisting-replay/src/adapter/mod.rs
- Modify: crates/persisting-replay/src/engine.rs
- Modify: crates/persisting-replay/tests/replay_contract.rs

**Interfaces:**
- Produces private OpenHandsPlan and typed terminal status.

- [ ] **Step 1: Write failing integration tests**

Use a fake entrypoint to prove replay-only executes the prefix and emits no live action. Add a zero-exit log containing Error while running the agent; require Failed status, nonzero report exit code, and a retained partial trajectory artifact.

- [ ] **Step 2: Verify RED**

    cargo test -p persisting-replay --test replay_contract openhands_replay_only_stops_at_boundary
    cargo test -p persisting-replay --test replay_contract openhands_fatal_status_is_not_success

- [ ] **Step 3: Split OpenHands and stop at the boundary**

Move parsing/output code into adapter/openhands.rs. In replay-only, run the pinned ReplayManager with the selected-prefix iteration limit and validate exactly after_step complete pairs with no live action. In live mode map and verify the total budget without a silent offset.

- [ ] **Step 4: Supervise and type terminal states**

Replace wait_with_output with run_process. Map fatal markers to Failed, exact maximum-iteration termination to MaxSteps, otherwise Completed. Publish a valid partial artifact before returning failure.

- [ ] **Step 5: Verify and commit**

    cargo test -p persisting-replay adapter::openhands
    cargo test -p persisting-replay --test replay_contract openhands_
    git commit -m "fix(replay): separate OpenHands replay from continuation"

---

### Task 8: Conservative journal and output lifecycle

**Files:**
- Modify: crates/persisting-replay/src/journal.rs
- Modify: crates/persisting-replay/src/engine.rs
- Modify: crates/persisting-replay/tests/replay_contract.rs

**Interfaces:**
- Produces ordered journal-state inspection that rejects every nonterminal tool-started run.

- [ ] **Step 1: Write failing lifecycle tests**

Cover: finished tool without terminal run is ambiguous; prepare-only interruption is retryable; invalid input does not create an explicit output run directory; failed execution publishes result paths and partial artifacts.

- [ ] **Step 2: Verify RED**

    cargo test -p persisting-replay journal::tests::finished_tool_without_terminal_run_is_ambiguous
    cargo test -p persisting-replay engine::tests::validation_does_not_consume_output_run_id

- [ ] **Step 3: Implement conservative state inspection**

Treat v3 success/failure terminal events as terminal. If any tool_started exists without one, report ambiguity regardless of tool_finished. Hold the exclusive lock from before state/output allocation through result publication.

- [ ] **Step 4: Reorder validation and finalization**

Validate paths, runtime, exact version, and complete plan before output allocation. After allocation, route errors through a finalizer that writes available artifacts, a v3 failed result, and a terminal journal event.

- [ ] **Step 5: Verify and commit**

    cargo test -p persisting-replay journal::tests
    cargo test -p persisting-replay engine::tests
    cargo test -p persisting-replay --test replay_contract failure_
    git commit -m "fix(replay): reject ambiguous same-sandbox retries"

---

### Task 9: Typed static adapter dispatch

**Files:**
- Modify: crates/persisting-replay/src/adapter/mod.rs
- Modify: all four Agent adapter modules
- Modify: crates/persisting-replay/src/model.rs

**Interfaces:**
- Produces AdapterPlan variants for all four Agents and common phase records.
- Removes common-engine indexing into Agent-specific ReplayPlan.native values.

- [ ] **Step 1: Write a failing dispatch test**

Build all four fixture plans and use only agent, after_step, calls, and source_sha256 common accessors. It must fail before AdapterPlan exists.

- [ ] **Step 2: Introduce typed dispatch**

Delegate common accessors with explicit enum matches. Keep native JSON private to each plan. Replace old shared build_plan/run with engine-driven prepare/replay/continue dispatch. Leave adapter/mod.rs containing common types, declarations, and static dispatch.

- [ ] **Step 3: Verify and commit**

    cargo test -p persisting-replay
    cargo clippy -p persisting-replay --all-targets -- -D warnings
    git commit -m "refactor(replay): split versioned agent adapters"

---

### Task 10: Documentation, migration, and release verification

**Files:**
- Modify: crates/persisting-pvisor/README.md
- Modify: docs/src/pvisor/guides/sandbox-replay.md
- Modify: docs/src/pvisor/guides/sandbox-replay.zh.md
- Modify: docs/src/pvisor/reference/cli.md
- Modify: replay smoke TOML fixtures

**Interfaces:**
- Documents three modes, v1 request compatibility, v3 output, total-step budget, stale opt-in, and fatal status.

- [ ] **Step 1: Write failing help assertions**

Assert replay help contains prepare-only, corrected replay-only wording, allow-stale-observations, and the total-step definition for max-steps.

- [ ] **Step 2: Verify RED**

    cargo test -p persisting-pvisor cli::tests::replay_help_describes_phase_modes

- [ ] **Step 3: Update English/Chinese docs and fixtures**

State that old non-Claude replay_only callers that only constructed a prefix must use prepare_only; v3 replay-only always executes the selected prefix and requires a runtime. Document v3 fields and change runtime-free smoke fixtures to prepare-only.

- [ ] **Step 4: Run final verification**

    cargo fmt --check
    cargo test -p persisting-replay
    cargo test -p persisting-pvisor cli::replay
    cargo test -p persisting-pvisor cli::tests::standalone_cli_is_small_and_run_can_be_explicit
    cargo clippy -p persisting-replay --all-targets -- -D warnings
    cargo clippy -p persisting-pvisor --lib --bin pvisor -- -D warnings
    git diff --check

Expected: every command exits zero. Workspace-wide tests remain outside acceptance because excluded subsystems are out of scope.

- [ ] **Step 5: Inspect scope and commit**

Confirm no excluded subsystem or .workbuddy path is staged, then commit documentation and fixtures with:

    git commit -m "docs(replay): document reliable phase and result contracts"

---

## Plan Self-review

- Every acceptance criterion maps to a task and focused regression test.
- Mode and result contracts land before adapter behavior consumes them.
- Process supervision is verified independently before replacing child paths.
- Behavioral tests precede final modularization, making Task 9 a green refactor.
- No task modifies an AGENTS.md-excluded subsystem.

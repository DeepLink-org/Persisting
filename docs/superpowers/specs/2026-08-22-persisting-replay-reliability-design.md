# Persisting Replay Reliability and Adapter Design

## Status

Approved in conversation on 2026-08-22.

## Context

`persisting-replay` reconstructs an Agent-native trajectory at a complete tool
batch boundary, re-executes the selected prefix in a fresh workspace, and can
continue the Agent from the resulting observations. The current implementation
supports Claude Code 2.1.220, mini-swe-agent 2.4.6, OpenHands 0.53.0, and
SWE-agent 1.1.0.

The initial implementation proved the native-resume approach, especially the
Claude Resume Transport cleanup, but exposes several incompatible meanings
through one `replay_only` boolean. It also centralizes parsing, execution,
process management, and result extraction in one large adapter module. Some
public controls are consequently advisory rather than enforced: non-Claude
`replay_only` does not execute tools, SWE-agent ignores `max_steps`, and an
OpenHands controller failure can still produce a successful result.

This design makes the replay contract explicit and fail-closed while retaining
the existing four version-pinned Agent integrations.

## Goals

1. Give prepare, replay, and continuation distinct, consistent public
   semantics across every Agent.
2. Ensure advertised limits and terminal statuses are enforced rather than
   silently ignored.
3. Bound child-process memory use and reliably clean up process groups.
4. Make stale observations an explicit opt-in degradation.
5. Return enough failure context to locate partial artifacts and ambiguous
   state.
6. Split Agent-specific behavior from the common replay lifecycle without
   introducing a dynamic plugin system.
7. Add contract-level integration coverage around fake runtimes and committed
   trajectory fixtures.

## Non-goals

- Supporting additional Agent versions or Agent families.
- Dynamically loading replay adapters.
- Guaranteeing that a replayed next action is identical to the source action.
- Resuming a partially executed, side-effecting prefix in the same sandbox.
- Adding Gateway capture, pChronicle storage, or a model-traffic audit.
- Changing TTAS, Queue, Search, or `persisting-dlcapt`.

## Public execution modes

Replace the internal `replay_only: bool` with a `ReplayMode` enum:

```rust
pub enum ReplayMode {
    PrepareOnly,
    ReplayOnly,
    ReplayAndContinue,
}
```

The CLI exposes the modes as follows:

- `--prepare-only` parses the source trajectory and constructs the selected
  native prefix. It executes no historical tool and starts no live Agent. An
  Agent runtime is optional.
- `--replay-only` parses the trajectory, executes the complete selected tool
  prefix, writes fresh observations into the reconstructed native context, and
  stops before the first new model request. It requires an Agent runtime for
  every Agent.
- Supplying neither flag performs prepare, replay, and live continuation. It
  requires an Agent runtime.
- `--prepare-only` and `--replay-only` conflict.

The existing v1 JSON request and TOML `replay_only = true` retain their field
shape but adopt the corrected `ReplayOnly` meaning. Callers that relied on the
old non-Claude prepare-only behavior must migrate to `prepare_only = true` or
`--prepare-only`. Deserializers reject both booleans being true.

## Step budget

`max_steps` is the total number of Agent action/model steps, including the
selected replay prefix. Every live adapter must either enforce that definition
or reject the request as unsupported before executing tools.

- Claude passes `max_steps - prefix_model_turns` as `--max-turns`.
- mini-swe-agent initializes its native call counter from the prefix and uses
  `max_steps` as the Agent `step_limit`.
- OpenHands maps the total limit to the runtime iteration control and verifies
  the resulting action count. Adapter tests lock down any framework-specific
  offset.
- SWE-agent receives `max_steps` in its runner request and applies the
  remaining live-step budget through the version-pinned Agent configuration.

`max_steps <= prefix_model_turns` is rejected for live continuation. It is
valid in `ReplayOnly` mode when it equals the selected prefix length.

## Result protocol

Output advances from `sandbox-playback.result/v2` to
`sandbox-playback.result/v3`. A successful or failed result contains:

```text
phase: prepared | replayed | continued
quality: verified | degraded
agent_status: completed | max_steps | failed | not_started
```

Definitions:

- `phase` is the furthest successfully completed replay phase.
- `quality` describes whether every reconstructed observation came from a
  supported execution path. It is independent of observation equality.
- `agent_status` describes live continuation only. Prepare and replay-only
  results use `not_started`.

The result also contains `run_id`, `state_dir`, `output_dir`, produced
artifacts, replayed call count, prefix step count, continued step count, and
optional structured failure information. A failed live Agent returns a nonzero
CLI exit code even when a partial native trajectory is available. Partial
artifacts remain listed in the failure result.

The CLI continues to accept v1 JSON requests. The implementation emits only v3
results; it does not maintain two output code paths.

## Degraded observations

The default invariant is that every observation inserted into the reconstructed
prefix was produced from the fresh workspace by a supported execution path.

Claude Code tools such as `Agent` and `TaskOutput` currently cannot satisfy
that invariant because their original results are copied from the source
trajectory. Such a call inside the selected prefix fails validation by default.

`--allow-stale-observations` permits these calls for research and migration
workflows. Each copied observation records a degradation reason and source call
ID. The overall result has `quality: degraded`; it can never be reported as
`verified`. Synthetic Task/Todo acknowledgements are subject to the same rule.
Unsupported tools remain errors rather than synthetic successes.

## Adapter architecture

The common engine owns configuration validation, directory allocation,
journaling, phase transitions, artifact publication, process supervision, and
result serialization. Agent modules own native trajectory parsing, typed plan
data, prefix construction, historical execution, continuation launch, and
continued-trajectory interpretation.

The source layout becomes:

```text
src/
  adapter/
    mod.rs
    claude.rs
    mini_swe.rs
    openhands.rs
    swe_agent.rs
    runtime.rs
  process.rs
  engine.rs
  model.rs
```

`adapter::mod` defines an internal dispatch enum over four concrete adapters.
This is static dispatch: adding a supported Agent still requires a code change
and a pinned profile. The design deliberately avoids object-safe dynamic
plugins.

Each adapter exposes the same phase-oriented operations conceptually:

```rust
trait ReplayAdapter {
    type Plan;

    fn build_plan(&self, request: &PlaybackRequest) -> Result<Self::Plan>;
    fn prepare(&self, plan: &Self::Plan, context: &RunContext) -> Result<Prepared>;
    fn replay(
        &self,
        plan: &Self::Plan,
        prepared: Prepared,
        context: &RunContext,
        journal: &mut Journal,
    ) -> Result<Replayed>;
    fn continue_run(
        &self,
        plan: &Self::Plan,
        replayed: Replayed,
        context: &RunContext,
        journal: &mut Journal,
    ) -> Result<Continued>;
}
```

The concrete implementation may use an internal enum instead of a public Rust
trait where associated plan types make dispatch clearer. The invariant is that
Agent-specific native `serde_json::Value` does not leak into the common engine.
Each module wraps native data in a private plan type and validates it before a
later phase can consume it.

Runtime manifest parsing and exact version probing live in `adapter/runtime.rs`.
Every Agent has a version-banner parser. A parser must return one exact semantic
version; substring containment is not sufficient.

## Process supervision

All spawned historical tools and Agent runners use a common process supervisor.
It provides:

- a dedicated process group on Unix;
- a wall-clock timeout and cancellation path;
- concurrent stdout and stderr draining;
- streaming logs written to an owner-only file;
- a bounded in-memory tail used for observation content and error
  classification;
- an explicit truncation flag and total byte counters;
- process-group termination followed by child reaping on timeout, cancellation,
  or unsupported background-process survival.

The supervisor never collects unbounded output with `read_to_end`, `output`, or
`wait_with_output`. Once the configured observation limit is reached it
continues draining to the log while retaining no additional in-memory bytes.

Historical Claude Bash does not support persistent background work in this
version. After the shell exits, surviving members of its process group are
terminated and reported as a degraded/error observation according to the
native command outcome. This prevents a background child from holding output
pipes indefinitely or mutating later replay steps asynchronously.

Agent continuation processes may run their own managed descendants, but the
whole process group is still terminated when the top-level continuation
reaches timeout or cancellation.

## Journal and directory lifecycle

Configuration, input trajectory, runtime manifest, exact runtime version, and
the complete plan are validated before allocating the unique output directory.
Validation failure therefore does not consume a caller-selected run ID.

The state lock is acquired before any replay side effect. Journal events record
phase transitions and tool starts/finishes. Same-sandbox recovery is permitted
only when the previous journal contains no `tool_started` event. If any tool
started and the run lacks a terminal event, the state is ambiguous regardless
of whether a corresponding `tool_finished` was synced: the engine always
restarts from the first tool and cannot prove that repeating completed effects
is safe.

A failed execution writes a v3 result whenever the state and output locations
are writable. The CLI also includes the generated run ID and paths in its JSON
error envelope if result publication itself fails.

## Agent-specific terminal behavior

- Claude Resume Transport keeps the existing nonce, boundary observation hash,
  canonical prefix hash, and fail-closed cleanup. The bridge remains local and
  authenticated.
- OpenHands controller fatal markers set `agent_status: failed` and a nonzero
  exit result. A maximum-iteration terminal state sets `agent_status:
  max_steps` and is not confused with an infrastructure failure.
- mini-swe-agent and SWE-agent runners publish structured terminal metadata
  rather than forcing Rust to infer status exclusively from free-form logs.

## Testing strategy

Development follows test-first cycles. The permanent suite includes:

1. Mode contract tests proving that prepare-only executes zero tools,
   replay-only executes the selected prefix for all four Agents, and live mode
   performs continuation.
2. A fake SWE-agent runtime proving `max_steps` reaches the runner and caps live
   model calls.
3. Claude tests proving opaque calls fail by default and opt-in runs produce a
   degraded result with per-call reasons.
4. OpenHands tests proving fatal controller markers produce a failed Agent
   status and nonzero CLI result while retaining partial artifacts.
5. Exact version-parser tests covering prefixes, suffixes, warnings, and wrong
   versions that contain the expected digits.
6. Process tests producing output beyond the memory limit and starting a
   background child. Tests assert bounded retained bytes, complete log draining,
   prompt return, and no surviving process-group member.
7. Journal tests proving that any interrupted side-effecting run is ambiguous
   and a prepare-only interruption is safely repeatable.
8. CLI integration tests that execute the committed smoke fixtures with fake
   version-pinned runtimes and validate v3 results and artifacts.
9. A portable mini-swe-agent runtime-path test that compares canonical paths on
   macOS and Linux.

Real third-party Agent installations and model calls remain outside ordinary
CI. They are exposed as ignored/profile tests for release qualification.

## Documentation and migration

The pVisor README, replay guide, and CLI reference will document the three
modes, the corrected meaning of `replay_only`, the stale-observation opt-in,
the total-step budget, and v3 result fields. The migration note will call out
that old non-Claude `replay_only = true` callers that only wanted prefix
construction must switch to `prepare_only = true`.

## Acceptance criteria

- All four Agents implement the same three mode semantics.
- `max_steps` is enforced or rejected before side effects for every Agent.
- A normal run cannot return verified quality after copying a source
  observation.
- A fatal Agent terminal state cannot return a successful CLI exit code.
- Child output memory is bounded and background descendants cannot stall the
  replay engine.
- Exact version profiles reject substring-only matches.
- Failed runs identify their run and artifact locations.
- Targeted replay and pVisor CLI tests pass on macOS and Linux.
- Clippy passes for `persisting-replay` and the touched pVisor CLI targets.

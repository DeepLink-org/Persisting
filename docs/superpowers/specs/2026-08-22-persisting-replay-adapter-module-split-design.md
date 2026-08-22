# Persisting Replay Adapter Module Split Design

## Objective

Replace the current facade-only Agent adapter modules with physical module
boundaries. Each Agent module will own its parser, preparation logic, execution
logic, native reconstruction helpers, and Agent-specific tests. This is a
behavior-preserving refactor.

## Scope

In scope:

- `crates/persisting-replay/src/adapter/mod.rs`
- `crates/persisting-replay/src/adapter/claude_code.rs`
- `crates/persisting-replay/src/adapter/mini_swe_agent.rs`
- `crates/persisting-replay/src/adapter/openhands.rs`
- `crates/persisting-replay/src/adapter/swe_agent.rs`
- `crates/persisting-replay/src/adapter/runtime.rs` only when an import or
  visibility adjustment is required by the move

Out of scope:

- Changes to `ReplayPlan`, `AdapterPlan`, public request/result schemas, replay
  modes, artifact names, or runtime behavior
- Redesign of the Mini/SWE SDK bridge
- Deduplication of comparison, artifact, status, or process configuration code
- Gateway, pChronicle, Queue, Search, TTAS, and `persisting-dlcapt`

## Module Ownership

### `adapter/mod.rs`

The module root owns only:

- child-module declarations;
- `RunContext`;
- the public `build_plan` and `run` static dispatch functions;
- helpers used by at least two Agent modules;
- the shared Mini/SWE SDK bridge that is explicitly outside this refactor.

It must not contain `build_claude_plan`, `build_mini_plan`,
`build_openhands_plan`, `build_swe_plan`, `run_claude`, `run_mini`, `run_swe`,
or `run_openhands`.

### Agent modules

Each Agent module owns its version-pinned native parsing, boundary extraction,
prefix construction, replay/continuation orchestration, native reconstruction,
terminal-state interpretation, and focused unit tests.

The only callable module surface from `adapter/mod.rs` remains:

```rust
pub(super) fn build(request: &PlaybackRequest) -> Result<AdapterPlan, ReplayError>;

pub(super) fn execute(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError>;
```

Agent-specific helpers remain private to their module. Shared helpers use
`pub(super)` only when a child module must call them.

## Migration Strategy

Move one Agent at a time. After each move, compile and run that Agent's focused
tests before moving the next Agent. Start with OpenHands, then Mini, SWE, and
finally Claude; this leaves the largest and most coupled implementation until
the common helper boundary is established.

Tests move with the implementation they cover. Tests for common environment,
process, or dispatch behavior remain in `adapter/mod.rs` or their existing
common module.

No logic is rewritten during movement. Necessary edits are limited to module
paths, imports, visibility, and calls through `super`.

## Acceptance Criteria

- All four Agent modules contain their actual parser and executor implementations.
- `adapter/mod.rs` contains none of the eight Agent-specific build/run functions
  listed above.
- The four dispatch entrypoints retain their current signatures.
- `cargo fmt --check -p persisting-replay` passes.
- `cargo test -p persisting-replay` passes.
- `cargo clippy -p persisting-replay --all-targets -- -D warnings` passes.
- `git diff --check` passes for the refactor.
- No excluded subsystem or unrelated concurrent change is staged.

## Risks and Controls

The primary risk is accidental behavioral change while resolving imports or
visibility. Mechanical moves are therefore separated by Agent and verified
incrementally. A second risk is creating a new generic abstraction merely to
make the files compile; the explicit out-of-scope rules prohibit that. Shared
code stays shared unless it already has a clear multi-Agent use.

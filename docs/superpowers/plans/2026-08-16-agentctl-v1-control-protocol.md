# AgentCtl v1 Control Protocol Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the unpublished AgentCtl protocol with a fully documented v1 `Hello + Sync` contract while preserving conservative multi-client checkpoints.

**Architecture:** The shared crate owns one small wire enum pair and a synchronous runtime client. pVisor owns Session state, checkpoint quorum, and snapshots; pPilot adapts its runtime bridge to periodically report `AgentState`. Process declarations, Effect declarations, directive sequences, and all Agent ABI compatibility surfaces are removed.

**Tech Stack:** Rust, Serde/JSON, Unix domain sockets, Tokio, Cargo tests.

## Global Constraints

- `AGENTCTL_VERSION` is exactly `1` because the protocol has not been formally released.
- The complete Control wire contract and its normative documentation live in `crates/persisting-agentctl/src/protocol.rs`.
- The only request variants are `Hello` and `Sync`; the only response variants are `Welcome`, `Synced`, and `Error`.
- Pilot and Agent runtime clients use the same protocol and are not distinguished by a wire role.
- A checkpoint waits for every live runtime Session frozen at checkpoint start.
- New and replacement Sessions are rejected while a checkpoint is active.
- Missing, stale, or ambiguous participant state never produces checkpoint success.
- Debug/PTY/login messages, Sessions, and authorization are out of scope and do not appear as placeholder Control variants.
- Remove all Agent ABI environment aliases, deprecated modules, methods, constants, and types.
- Do not touch TTAS, tiered tensor memory, Queue, samplers, Search, or `persisting-dlcapt`.

---

### Task 1: Define and document the v1 wire contract and client SDK

**Files:**
- Modify: `crates/persisting-agentctl/src/protocol.rs`
- Modify: `crates/persisting-agentctl/src/client.rs`
- Modify: `crates/persisting-agentctl/src/lib.rs`

**Interfaces:**
- Produces: `AgentRequest`, `AgentResponse`, `AgentState`, `AgentDirective`, `AgentErrorCode`, and `AGENTCTL_VERSION = 1`.
- Produces: `AgentCtlClientConfig::from_environment(environment, client_id)` and `AgentCtlClientConfig::from_current_environment(client_id)`.
- Produces: `AgentCtlClient::connect() -> anyhow::Result<AgentDirective>`, `AgentCtlClient::sync(AgentState) -> anyhow::Result<AgentDirective>`, and `AgentCtlClient::sync_interval_ms() -> Option<u64>`.
- Produces: `AgentCtlResponseError { code: AgentErrorCode, message: String }`, downcastable from the client SDK's `anyhow::Error`.

- [ ] **Step 1: Add exact wire-format tests to `protocol.rs`**

Add tests that serialize and deserialize the approved v1 shapes, including these exact values:

```rust
#[test]
fn hello_has_the_v1_wire_shape() {
    let request = AgentRequest::Hello {
        version: 1,
        token: "secret".into(),
        client_id: "worker-1".into(),
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::json!({
            "type": "hello",
            "version": 1,
            "token": "secret",
            "client_id": "worker-1"
        })
    );
}

#[test]
fn quiesced_sync_and_quiesce_response_roundtrip() {
    let request = AgentRequest::Sync {
        version: 1,
        session_id: "session-1".into(),
        state: AgentState::Quiesced {
            checkpoint_id: "checkpoint-7".into(),
        },
    };
    let response = AgentResponse::Synced {
        directive: AgentDirective::Quiesce {
            checkpoint_id: "checkpoint-7".into(),
            deadline_unix_ms: Some(1_786_890_000_000),
        },
    };
    for value in [
        serde_json::to_value(&request).unwrap(),
        serde_json::to_value(&response).unwrap(),
    ] {
        assert!(value.get("type").is_some());
    }
    assert_eq!(
        serde_json::from_value::<AgentRequest>(serde_json::to_value(request).unwrap()).unwrap(),
        request
    );
    assert_eq!(
        serde_json::from_value::<AgentResponse>(serde_json::to_value(response).unwrap()).unwrap(),
        response
    );
}
```

Also assert that `Continue` omits optional fields, all four `AgentErrorCode`
values serialize in snake case, and `AGENTCTL_VERSION == 1`.

- [ ] **Step 2: Run the protocol tests and verify they fail against v2**

Run: `cargo test -p persisting-agentctl protocol::tests -- --nocapture`

Expected: compilation fails because `AgentRequest::Hello`, `AgentState`, and
the v1 response variants do not yet exist.

- [ ] **Step 3: Replace `protocol.rs` with the normative v1 contract**

Keep the existing `AGENTCTL_*` environment names and bounded-frame constant,
delete every `LEGACY_AGENT_ABI_*` and `AGENT_ABI_*` item, and define exactly:

```rust
pub const AGENTCTL_VERSION: u32 = 1;

#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRequest {
    Hello { version: u32, token: String, client_id: String },
    Sync { version: u32, session_id: String, state: AgentState },
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentState {
    Active,
    Idle,
    Quiesced { checkpoint_id: String },
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentDirective {
    Continue,
    Quiesce { checkpoint_id: String, deadline_unix_ms: Option<u64> },
    Shutdown { reason: Option<String> },
}

#[serde(rename_all = "snake_case")]
pub enum AgentErrorCode {
    InvalidRequest,
    Unauthorized,
    VersionMismatch,
    Conflict,
}

#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentResponse {
    Welcome { session_id: String, sync_interval_ms: u64, directive: AgentDirective },
    Synced { directive: AgentDirective },
    Error { code: AgentErrorCode, message: String },
}
```

Derive `Debug`, `Clone`, `PartialEq`, `Eq`, `Serialize`, and `Deserialize` for
every type; additionally derive `Copy` for `AgentErrorCode`. Add module docs
that contain the protocol scope, one-shot transport, `Hello -> Sync` examples,
state semantics, checkpoint invariants, cooperative-evidence warning, error
categories, and the separate future Debug-plane boundary. Document every
public variant and non-obvious field.

- [ ] **Step 4: Re-run the protocol tests and localize the remaining failure**

Run: `cargo test -p persisting-agentctl protocol::tests -- --nocapture`

Expected: the missing-v1-type failures are gone and compilation now stops only
in `client.rs`, because that file still consumes the removed v2 types. The full
Task 1 test pass occurs after Step 7.

- [ ] **Step 5: Add client configuration and response-error tests**

In `client.rs`, add tests that build a map with only the four
`PERSISTING_AGENTCTL_*` keys and call:

```rust
let config = AgentCtlClientConfig::from_environment(&environment, "worker-1")
    .unwrap()
    .unwrap();
assert_eq!(config.client_id, "worker-1");
assert_eq!(config.token, "secret");
```

Add a second test proving a map containing only `PERSISTING_AGENT_ABI_ENDPOINT`
returns `Ok(None)`. Add a unit test that constructs
`AgentCtlResponseError { code: AgentErrorCode::Conflict, message: "busy".into() }`
and verifies both fields and its `Display` text.

- [ ] **Step 6: Replace the client SDK with `connect + sync`**

Change configuration to:

```rust
pub struct AgentCtlClientConfig {
    pub endpoint: PathBuf,
    pub token: String,
    pub client_id: String,
}
```

Remove role/name parameters and all fallback lookups for Agent ABI variables.
Store `session_id: Option<String>` and `sync_interval_ms: Option<u64>` in the
client. Implement:

```rust
pub fn connect(&mut self) -> anyhow::Result<AgentDirective>;
pub fn sync(&mut self, state: AgentState) -> anyhow::Result<AgentDirective>;
pub fn session_id(&self) -> Option<&str>;
pub fn sync_interval_ms(&self) -> Option<u64>;
```

`connect` sends `AgentRequest::Hello`, saves the Session ID and interval from
`AgentResponse::Welcome`, and returns its directive. `sync` requires a Session,
sends `AgentRequest::Sync`, and returns the directive from
`AgentResponse::Synced`. Delete heartbeat, process, checkpoint-ack, operation,
effect, and `checkpoint_directive` methods.

Implement `AgentCtlResponseError` with `Display` and `std::error::Error`.
Convert `AgentResponse::Error { code, message }` into that concrete error so a
caller can downcast and branch on `code`; do not parse human messages.

- [ ] **Step 7: Remove source-compatibility exports and run crate tests**

Delete the deprecated `abi` module and export only `AgentCtlClient`,
`AgentCtlClientConfig`, and `AgentCtlResponseError` from `client.rs`.

Run: `cargo test -p persisting-agentctl --lib`

Expected: all `persisting-agentctl` library tests pass.

- [ ] **Step 8: Commit the shared protocol and client**

```bash
git add crates/persisting-agentctl/src/protocol.rs crates/persisting-agentctl/src/client.rs crates/persisting-agentctl/src/lib.rs
git commit -m "refactor(agentctl): define minimal v1 control protocol"
```

### Task 2: Replace the pVisor AgentCtl server state machine

**Files:**
- Modify: `crates/persisting-pvisor/src/agentctl.rs`
- Modify: `crates/persisting-pvisor/tests/agentctl_contract.rs`

**Interfaces:**
- Consumes: the v1 protocol and client APIs from Task 1.
- Produces: `AgentClientSnapshot { client_id, state, last_sync_unix_ms, stale }`.
- Produces: `AgentCtlSnapshot { run_id, attempt_id, directive, clients }`.
- Produces: `AgentCtlControl::request_quiesce(checkpoint_id, deadline_unix_ms) -> anyhow::Result<()>`.
- Preserves: `AgentCtlControl::continue_execution()`, `request_shutdown(reason)`, `snapshot()`, and the Run-scoped environment projection.

- [ ] **Step 1: Rewrite the external contract test for two runtime clients**

Replace the existing process/effect lifecycle test with a test that creates two
clients using `AgentCtlClientConfig::from_environment(&server.environment(), id)`.
Connect both, sync `Active`, request checkpoint `checkpoint-1`, and assert:

```rust
assert!(matches!(
    first.sync(AgentState::Idle).unwrap(),
    AgentDirective::Quiesce { ref checkpoint_id, .. } if checkpoint_id == "checkpoint-1"
));
first.sync(AgentState::Quiesced {
    checkpoint_id: "checkpoint-1".into(),
}).unwrap();
let snapshot = server.control().snapshot();
assert!(matches!(
    snapshot.clients.iter().find(|client| client.client_id == "first").unwrap().state,
    AgentState::Quiesced { ref checkpoint_id } if checkpoint_id == "checkpoint-1"
));
assert!(matches!(
    snapshot.clients.iter().find(|client| client.client_id == "second").unwrap().state,
    AgentState::Active
));
second.sync(AgentState::Quiesced {
    checkpoint_id: "checkpoint-1".into(),
}).unwrap();
assert!(server.control().snapshot().clients.iter().all(|client| matches!(
    &client.state,
    AgentState::Quiesced { checkpoint_id } if checkpoint_id == "checkpoint-1"
)));
```

- [ ] **Step 2: Run the contract test and verify it fails against the v2 server**

Run: `cargo test -p persisting-pvisor --test agentctl_contract -- --nocapture`

Expected: compilation fails on the removed v2 client/server interfaces.

- [ ] **Step 3: Reduce snapshots and internal Session state**

Replace the public snapshots with:

```rust
pub struct AgentClientSnapshot {
    pub client_id: String,
    pub state: AgentState,
    pub last_sync_unix_ms: Option<u64>,
    pub stale: bool,
}

pub struct AgentCtlSnapshot {
    pub run_id: String,
    pub attempt_id: String,
    pub directive: AgentDirective,
    pub clients: Vec<AgentClientSnapshot>,
}
```

Make `ClientSession` contain only `client_id`, `state`, `last_seen_unix_ms`, and
`last_sync_unix_ms`. Make `AgentCtlState` contain only Run/Attempt identity,
token, current directive, and `HashMap<String, ClientSession>`. Keep
`AGENTCTL_MAX_SESSIONS`; delete process/operation limits and storage.

- [ ] **Step 4: Implement conservative directive transitions**

Make `request_quiesce` lock state, require the current directive to be
`Continue`, remove Sessions already stale at that instant, require at least one
remaining Session, and then publish `Quiesce`. While it is active, Sessions are
never removed by timeout. `continue_execution` publishes `Continue` and
`request_shutdown` publishes `Shutdown`; neither returns a sequence.

Add a unit test that directly ages one internal Session beyond
`SYNC_INTERVAL_MS * 3`, calls `request_quiesce`, and proves the stale Session is
removed before the participant set freezes. Add a second test proving an aged
participant remains present after quiescence begins.

- [ ] **Step 5: Dispatch `Hello` and `Sync` directly**

Replace wrapper/body dispatch with direct matching on `AgentRequest`.

`Hello` must:

1. reject a non-v1 request with `VersionMismatch`;
2. reject the wrong token with `Unauthorized`;
3. reject an empty `client_id` with `InvalidRequest`;
4. reject every new/replacement Session during `Quiesce` with `Conflict`;
5. outside checkpoint, remove a stale Session with the same `client_id`;
6. reject a still-live duplicate or the Session limit with `Conflict`;
7. insert an initially `Active` Session and return `Welcome`.

`Sync` must validate version and Session ID before mutation. `Active` and `Idle`
are valid under any directive. `Quiesced { checkpoint_id }` is valid only when
the current directive is `Quiesce` with the same ID; otherwise return
`Conflict`. A valid sync updates Session state and timestamps, then returns
`Synced` with the current directive. Repeating the same matching `Quiesced`
state must succeed.

- [ ] **Step 6: Return typed protocol errors and remove legacy environment projection**

Introduce a private failure carrying `AgentErrorCode` and message. Map parsing
and framing failures to `InvalidRequest`, auth failures to `Unauthorized`,
version failures to `VersionMismatch`, and state/session conflicts to
`Conflict`. Make the server's environment contain only:

```text
PERSISTING_AGENTCTL_ENDPOINT
PERSISTING_AGENTCTL_TOKEN
PERSISTING_AGENTCTL_VERSION=1
PERSISTING_AGENTCTL_TRANSPORT=unix
```

- [ ] **Step 7: Cover conflict, retry, and multi-client behavior**

In `agentctl.rs` unit tests, verify invalid token and version codes, duplicate
live client conflict, stale replacement outside checkpoint, rejected `Hello`
during checkpoint, mismatched checkpoint conflict, idempotent matching
quiescence, and a frozen disconnected participant that is not silently
removed.

- [ ] **Step 8: Run pVisor AgentCtl tests**

Run:

```bash
cargo test -p persisting-pvisor --lib agentctl::tests -- --nocapture
cargo test -p persisting-pvisor --test agentctl_contract -- --nocapture
```

Expected: all AgentCtl server and external contract tests pass.

- [ ] **Step 9: Commit the v1 server**

```bash
git add crates/persisting-pvisor/src/agentctl.rs crates/persisting-pvisor/tests/agentctl_contract.rs
git commit -m "refactor(pvisor): serve AgentCtl v1 sync protocol"
```

### Task 3: Simplify checkpoint integration and persisted observations

**Files:**
- Modify: `crates/persisting-pvisor/src/pvisor.rs`
- Modify: `crates/persisting-pvisor/src/lib.rs`
- Modify: `crates/persisting-pvisor/src/bundle.rs`
- Modify: `crates/persisting-pvisor/tests/fixtures/bundles/v1-minimal.json`

**Interfaces:**
- Consumes: reduced `AgentCtlSnapshot` and `AgentState` from Tasks 1-2.
- Produces: `RunHandle::checkpoint` that waits only on the frozen clients'
  matching quiesced state and always publishes `Continue` after success or
  abandonment.
- Removes: public Agent ABI method and all process/operation snapshot exports.

- [ ] **Step 1: Replace the checkpoint barrier unit test**

Construct a snapshot with no clients and assert the barrier is false. Add one
`Active` client and keep it false. Change that client to:

```rust
AgentState::Quiesced { checkpoint_id: "cp".into() }
```

and assert true. Change the ID to `other` and assert false. Delete every open
Effect assertion because the v1 protocol has no Effect journal.

- [ ] **Step 2: Run the barrier test and verify it fails against old snapshots**

Run: `cargo test -p persisting-pvisor --lib logical_checkpoint_barrier -- --nocapture`

Expected: compilation fails until the checkpoint code consumes the reduced
snapshot.

- [ ] **Step 3: Update live checkpoint behavior**

Remove the deprecated `RunHandle::agent_abi` method and all open-operation
wording. Call `request_quiesce(...)?`, then poll until every frozen snapshot
client has `AgentState::Quiesced` with the requested ID. Do not silently drop or
claim success for a stale participant. Let the deadline produce the error:

```text
checkpoint <id> timed out waiting for all AgentCtl clients to quiesce
```

Publish `Continue` after checkpoint creation succeeds or the wait/creation is
abandoned. Reduce `empty_agentctl_snapshot` to Run ID, Attempt ID, `Continue`,
and an empty client vector.

- [ ] **Step 4: Remove obsolete public exports and bundle fields**

Export only `AgentClientSnapshot`, `AgentCtlControl`, `AgentCtlServer`, and
`AgentCtlSnapshot` from pVisor's server module; re-export the shared v1 protocol
types from `persisting-agentctl`. Remove process/operation snapshot fields,
directive sequences, and legacy Agent ABI constants from all constructors.

Remove `#[serde(alias = "agent_abi")]` from `RunBundle::agentctl`. Update the v1
fixture to use `"agentctl"` and the reduced snapshot shape. Rename the fixture
test to describe a minimal v1 bundle without legacy normalization. Remove
`PERSISTING_AGENT_ABI_*` keys from the environment summary.

- [ ] **Step 5: Run targeted checkpoint and bundle tests**

Run:

```bash
cargo test -p persisting-pvisor --lib logical_checkpoint_barrier -- --nocapture
cargo test -p persisting-pvisor --lib bundle::tests -- --nocapture
```

Expected: checkpoint and bundle tests pass with no process/operation fields.

- [ ] **Step 6: Commit checkpoint and observation cleanup**

```bash
git add crates/persisting-pvisor/src/pvisor.rs crates/persisting-pvisor/src/lib.rs crates/persisting-pvisor/src/bundle.rs crates/persisting-pvisor/tests/fixtures/bundles/v1-minimal.json
git commit -m "refactor(pvisor): simplify AgentCtl checkpoint observations"
```

### Task 4: Adapt pPilot's runtime bridge to unified Sync

**Files:**
- Modify: `crates/persisting-ppilot/src/runtime_bridge.rs`
- Modify: `crates/persisting-ppilot/src/lib.rs`

**Interfaces:**
- Consumes: `AgentCtlClient::connect`, `sync`, `sync_interval_ms`,
  `AgentState`, and `AgentDirective`.
- Produces: `PilotRuntimeBridge::start(client, cancellation)`,
  `set_active() -> anyhow::Result<()>`, `set_idle()`, `directive()`,
  `snapshot()`, and `finish()`.
- Removes: process registration, lifecycle setters, Effect begin/complete/open
  methods, directive sequences, and open-Effect snapshot data.

- [ ] **Step 1: Add pure bridge-transition tests**

Extract or introduce a small private directive application helper. Test these
transitions without network I/O:

```rust
let mut state = BridgeState::new(AgentDirective::Continue);
state.agent_state = AgentState::Idle;
assert!(apply_directive(
    &mut state,
    AgentDirective::Quiesce {
        checkpoint_id: "cp".into(),
        deadline_unix_ms: None,
    },
));
assert_eq!(
    state.agent_state,
    AgentState::Quiesced { checkpoint_id: "cp".into() }
);
assert!(!state.accepting_work);
```

Also verify `Active + Quiesce` stays `Active` while refusing new work,
`Quiesced + Continue` becomes `Idle`, and `Shutdown` disables admission.

- [ ] **Step 2: Run bridge tests and verify they fail against lifecycle state**

Run: `cargo test -p persisting-ppilot --lib runtime_bridge::tests -- --nocapture`

Expected: compilation fails until `BridgeState` uses `AgentState`.

- [ ] **Step 3: Reduce bridge state and public methods**

Use:

```rust
struct BridgeState {
    agent_state: AgentState,
    accepting_work: bool,
    directive: AgentDirective,
    quiesce_deadline_unix_ms: Option<u64>,
    warnings: Vec<String>,
}
```

Start in `Active`. Remove registration from `start`, remove all Effect methods,
and replace `set_lifecycle` with
`set_active(&self) -> anyhow::Result<()>` and `set_idle(&self)`. `set_active`
returns an error and leaves the state unchanged while the current directive is
not `Continue`. `set_idle` turns an already-observed `Quiesce` directly into
the matching `Quiesced` state.

- [ ] **Step 4: Drive directives through `sync`**

Have the periodic loop send the current `AgentState` through
`AgentCtlClient::sync`. Apply the returned directive. When applying `Quiesce`
turns `Idle` into `Quiesced`, immediately issue one additional `Sync` so pVisor
receives the checkpoint confirmation without waiting a full interval. A repeat
`Quiesce` for an already matching `Quiesced` state must not loop.

Use the interval saved by `connect`, clamped to at least 20 ms. Rename all
heartbeat/Agent ABI warnings to AgentCtl sync warnings. Keep `finish` waiting
for `Continue` or its deadline before stopping the periodic loop.

- [ ] **Step 5: Reduce bridge diagnostics and exports**

Make `snapshot` contain only `state` and `directive`. Remove
`checkpoint_directive` from pPilot's public re-exports.

- [ ] **Step 6: Run pPilot tests and commit**

Run:

```bash
cargo test -p persisting-ppilot --lib runtime_bridge::tests -- --nocapture
cargo test -p persisting-ppilot --lib
```

Expected: all pPilot library tests pass.

```bash
git add crates/persisting-ppilot/src/runtime_bridge.rs crates/persisting-ppilot/src/lib.rs
git commit -m "refactor(ppilot): use AgentCtl v1 sync state"
```

### Task 5: Update user documentation and verify the complete migration

**Files:**
- Modify: `crates/persisting-agentctl/README.md`
- Modify: `crates/persisting-pvisor/README.md`
- Modify: `docs/src/pvisor/reference/cli.md`

**Interfaces:**
- Documents: v1 environment, `Hello + Sync`, three client states, conservative
  multi-client checkpoint, cooperative evidence boundary, and future separate
  Debug plane.
- Validates: no v2/Agent ABI/process/Effect protocol surface remains.

- [ ] **Step 1: Add a documentation-content regression scan**

Run the following before edits and retain the output as the checklist:

```bash
rg -n "Agent ABI|PERSISTING_AGENT_ABI|AgentClientRole|AgentLifecycleState|RegisterProcess|CheckpointQuiesced|EffectBegin|EffectComplete|directive_seq|open operations|process declarations|operation declarations|VERSION=2" \
  crates/persisting-agentctl crates/persisting-ppilot crates/persisting-pvisor docs/src/pvisor \
  --glob '*.rs' --glob '*.md' --glob '*.json'
```

Expected: matches identify the remaining obsolete Control-protocol vocabulary;
Supervisor protocol sequence fields are unrelated and must remain unchanged.

- [ ] **Step 2: Rewrite AgentCtl documentation**

Document the four `PERSISTING_AGENTCTL_*` variables with version 1. Show:

```rust
let Some(config) = AgentCtlClientConfig::from_current_environment("worker-1")?
else {
    return Ok(());
};
let mut client = AgentCtlClient::new(config);
let directive = client.connect()?;
let directive = client.sync(AgentState::Active)?;
```

Explain that all live runtime clients participate in checkpoint, `Sync` carries
`Active`, `Idle`, or checkpoint-specific `Quiesced`, and reports are
cooperative. Remove process/Effect inventory claims. Mention that future
terminal login is a separately authorized Debug plane rather than a Control
message extension.

- [ ] **Step 3: Update pVisor README and CLI reference**

Change version 2 to version 1; describe `Hello + Sync`; state that checkpoint
freezes and waits for every live runtime Session; remove directive-generation,
open-Effect, declared-process, and compatibility wording. Keep filesystem,
network, Gateway, and Run Bundle material otherwise unchanged.

- [ ] **Step 4: Run the cleanup scan again**

Run the Step 1 `rg` command again.

Expected: no obsolete AgentCtl Control-protocol matches remain. Matches in
`supervisor.rs` for the separate Supervisor protocol are allowed and reviewed
manually.

- [ ] **Step 5: Format and run targeted verification**

Run:

```bash
cargo fmt --all -- --check
cargo test -p persisting-agentctl --lib
cargo test -p persisting-ppilot --lib
cargo test -p persisting-pvisor --lib
cargo test -p persisting-pvisor --test agentctl_contract
cargo check -p persisting-agentctl -p persisting-ppilot -p persisting-pvisor
git diff --check
```

Expected: every command succeeds. Do not expand validation to excluded
workspace subsystems.

- [ ] **Step 6: Review the final diff and commit documentation**

Confirm the final diff contains no Debug implementation, generic extension
fields, v2 decoder, or compatibility shim.

```bash
git add crates/persisting-agentctl/README.md crates/persisting-pvisor/README.md docs/src/pvisor/reference/cli.md
git commit -m "docs: document AgentCtl v1 control plane"
```

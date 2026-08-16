# AgentCtl v1 Control Protocol Design

## Status

Approved design for replacing the unpublished AgentCtl protocol with a small,
fully documented v1 control contract.

## Purpose

AgentCtl is an optional, cooperative control channel between pVisor and the
runtime clients inside one Run. A runtime client may be pPilot or an Agent; the
protocol treats both identically.

The control protocol has only three responsibilities:

1. authenticate a runtime client and create a Session;
2. exchange client state and the current pVisor directive periodically;
3. coordinate a safe, multi-client checkpoint.

AgentCtl declarations are not enforcement evidence. In particular, an Agent's
claim that it is idle or quiesced does not prove that every process or external
effect has stopped. pVisor continues to derive authoritative process facts from
the execution provider and operating system rather than from AgentCtl.

## Goals

- Replace the current six-request protocol with `Hello` and `Sync`.
- Preserve cooperative heartbeat, shutdown, and safe checkpoint behavior.
- Support any number of runtime clients without distinguishing Pilot and Agent
  roles on the wire.
- Make retries idempotent and checkpoint failure conservative.
- Start the public contract at version 1 because AgentCtl has not been released.
- Keep every control wire type and its complete protocol documentation in one
  dedicated `protocol.rs` file.
- Leave a clean boundary for a future interactive Debug plane without adding
  terminal concepts to the Control plane.

## Non-goals

- Process registration or process inventory.
- Declaring, journaling, sequencing, or classifying external Effects.
- Treating Agent-reported state as authoritative enforcement evidence.
- Implementing terminal login, PTY forwarding, or other Debug-plane behavior.
- Preserving the unpublished v2 wire contract, Agent ABI names, or source-level
  compatibility aliases.

## Protocol Shape

The transport remains a bounded, newline-delimited JSON request/response
exchange over the Run-local Unix socket. Each connection carries one request
and one response. Because each request is independently framed, both request
variants carry the protocol version.

The normative Rust contract belongs in
`crates/persisting-agentctl/src/protocol.rs`. The following sketch fixes the
intended public shape; implementation details such as derives and field
visibility follow the crate's existing conventions.

```rust
pub const AGENTCTL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRequest {
    Hello {
        version: u32,
        token: String,
        client_id: String,
    },
    Sync {
        version: u32,
        session_id: String,
        state: AgentState,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentState {
    Active,
    Idle,
    Quiesced { checkpoint_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentDirective {
    Continue,
    Quiesce {
        checkpoint_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deadline_unix_ms: Option<u64>,
    },
    Shutdown {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentErrorCode {
    InvalidRequest,
    Unauthorized,
    VersionMismatch,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentResponse {
    Welcome {
        session_id: String,
        sync_interval_ms: u64,
        directive: AgentDirective,
    },
    Synced {
        directive: AgentDirective,
    },
    Error {
        code: AgentErrorCode,
        message: String,
    },
}
```

There is deliberately no `AgentClientRole`. Pilot and Agent clients obey the
same checkpoint rules, and `client_id` is sufficient to identify them for
diagnostics and Session replacement.

There is deliberately no directive sequence. A checkpoint ID uniquely
correlates a `Quiesce` directive with a `Quiesced` report, and the server checks
the report against the one active checkpoint.

## State and Directive Semantics

### Client states

- `Active` means the client may be starting, running work, or draining work
  after receiving `Quiesce`. It is not yet at a safe checkpoint boundary.
- `Idle` means the client has no current work but may accept work until it
  observes `Quiesce`.
- `Quiesced { checkpoint_id }` means the client observed that exact directive,
  stopped admitting new work, drained in-flight work, and will remain stopped
  until it observes `Continue` or `Shutdown`.

Starting, running, quiescing, and stopping are not separate wire states.
Connection establishment, `Active`, the current directive, and process exit
already express those transitions.

### Server directives

- `Continue` allows the runtime client to admit and run work.
- `Quiesce` asks the client to stop admitting work, drain in-flight work, and
  report `Quiesced` for the supplied checkpoint ID before the optional
  deadline.
- `Shutdown` asks the client to terminate. AgentCtl does not require a shutdown
  acknowledgement because pVisor observes authoritative process exit through
  the execution provider.

## Wire Examples

A client opens a Session:

```json
{"type":"hello","version":1,"token":"run-secret","client_id":"worker-1"}
```

```json
{"type":"welcome","session_id":"session-1","sync_interval_ms":1000,"directive":{"kind":"continue"}}
```

The client reports ordinary activity and receives the current directive:

```json
{"type":"sync","version":1,"session_id":"session-1","state":{"kind":"active"}}
```

```json
{"type":"synced","directive":{"kind":"quiesce","checkpoint_id":"checkpoint-7","deadline_unix_ms":1786890000000}}
```

After reaching the safe boundary, the same client confirms that checkpoint
through its next `Sync` rather than a separate acknowledgement message:

```json
{"type":"sync","version":1,"session_id":"session-1","state":{"kind":"quiesced","checkpoint_id":"checkpoint-7"}}
```

Repeated `Sync` requests with the same quiesced state are valid and idempotent.

## Session Lifecycle

`Hello` authenticates with the Run-scoped token and creates a random Session
ID. The Session ID authenticates subsequent `Sync` requests on the protected
Run-local transport. A non-empty `client_id` identifies the runtime client and
must be unique among live Sessions.

Each successful `Sync` refreshes the Session's last-seen timestamp. Outside a
checkpoint, a Session becomes stale after missing three sync intervals. A
subsequent `Hello` with the same `client_id` may replace that stale Session.
`Hello` for an already-live `client_id` returns `Conflict`.

The server includes its current directive in both `Welcome` and `Synced`, so a
new or reconnecting client cannot begin work without observing the current
control state.

## Multi-client Checkpoint

Checkpoint coordination follows this sequence:

1. Before starting, pVisor removes Sessions already stale under the normal
   three-interval rule.
2. pVisor publishes one `Quiesce` directive and freezes the set of all currently
   live runtime Sessions as the checkpoint participants.
3. Each participant learns the directive from `Synced`, stops admitting new
   work, drains in-flight work, and reports `Quiesced` with the same checkpoint
   ID. A client that was `Idle` may do this immediately.
4. pVisor waits until every frozen participant has reported the matching state.
5. pVisor performs the checkpoint only after all participants are quiesced.
6. After the checkpoint succeeds or is explicitly abandoned, pVisor publishes
   `Continue`. Clients remain quiesced until they observe that directive.

While a checkpoint is active, all new `Hello` requests and all Session
replacement attempts return `Conflict`. This keeps the participant set stable.
A participant is not silently removed if it misses sync intervals during the
checkpoint. If every participant does not report the matching state before the
deadline, the checkpoint fails rather than claiming an unsafe success.

A new `Quiesced` report whose checkpoint ID does not match the active
`Quiesce` directive returns `Conflict` and does not mutate Session state. Once
the server has accepted a Session's `Quiesced` state, repeating that exact
state remains valid after pVisor publishes `Continue` or `Shutdown`; this is
how a still-quiesced client learns that it may resume or must terminate. A
different checkpoint ID still returns `Conflict`.

## Error Handling

The protocol has one error response and four stable error categories:

- `InvalidRequest`: malformed values or a request that is invalid independent
  of current server state;
- `Unauthorized`: invalid Run token or Session ID;
- `VersionMismatch`: any request whose version is not exactly 1;
- `Conflict`: a valid request that conflicts with a live Session or active
  checkpoint.

Malformed JSON, empty frames, oversized frames, and transport timeouts do not
mutate protocol state. The server returns `Error` when it can safely form a
response and closes the one-shot connection afterward. Human-readable messages
provide context, but clients branch only on `AgentErrorCode`.

## Code and Documentation Boundaries

`crates/persisting-agentctl/src/protocol.rs` is the single source of truth for
the Control wire contract. It contains:

- protocol and frame-size constants;
- endpoint, token, version, and transport environment names;
- `AgentRequest`, `AgentResponse`, `AgentState`, `AgentDirective`, and
  `AgentErrorCode`;
- module-level documentation for the interaction model, safety boundary,
  checkpoint invariants, and wire examples;
- item-level documentation for every public variant and field whose semantics
  are not self-evident.

It contains no client transport, Session store, pVisor state machine, or Debug
protocol implementation. The synchronous runtime SDK remains in
`crates/persisting-agentctl/src/client.rs`, while the server and authoritative
Run integration remain in `crates/persisting-pvisor/src/agentctl.rs`.

## Debug-plane Boundary

A future interactive login capability belongs to a separate Debug plane. It
may be managed by the AgentCtl service, but it will use a separate protocol
module, Session type, and bidirectional transport. PTY creation, terminal input
and output, window resizing, signals, and exit status must never become
`AgentRequest`, `AgentResponse`, `AgentState`, or `AgentDirective` variants.

The Run token injected into a runtime client is not sufficient user
authorization for terminal access. A future design must have pVisor authorize
the user and issue a short-lived, single-purpose Debug Session. Debug Sessions
do not participate in runtime heartbeat or checkpoint quorum. Whether an
existing Debug Session blocks or is terminated by checkpoint is intentionally
left to that feature's own design.

No Debug protocol file or placeholder wire variant is added in the v1 change.
The extension point is the separation between the AgentCtl service and its
Control protocol, not an unused field in the Control messages.

## Migration

AgentCtl has not been formally released, so this is a direct replacement rather
than a compatibility migration:

- set `AGENTCTL_VERSION` to 1;
- remove the v2 request, response, lifecycle, process, and operation types;
- remove all deprecated Agent ABI constants, environment aliases, and type
  aliases;
- stop exporting `PERSISTING_AGENT_ABI_*` variables from pVisor;
- update pPilot, the shared client, pVisor, tests, and documentation together;
- provide no v2 decoder, adapter, feature flag, or transition period.

## Validation

Targeted tests cover:

1. exact JSON serialization and deserialization for every v1 message shape;
2. successful `Hello`, invalid token, version mismatch, and duplicate
   `client_id`;
3. `Active` and `Idle` synchronization and current-directive delivery;
4. a multi-client checkpoint that does not proceed until every frozen
   participant reports the matching `Quiesced` state;
5. immediate quiescence from `Idle`;
6. idempotent repeated quiesced reports;
7. rejection of a mismatched checkpoint ID;
8. rejection of new and replacement Sessions during checkpoint;
9. checkpoint timeout when a participant disconnects;
10. client resumption only after observing `Continue`;
11. stale Session replacement outside checkpoint;
12. shutdown delivery without an Agent-reported stopping state.

Validation uses targeted tests for `persisting-agentctl`, pPilot's runtime
bridge, and pVisor's AgentCtl contract. Workspace subsystems excluded by the
repository instructions are not part of acceptance.

## Acceptance Criteria

- `protocol.rs` exposes only the types required for `Hello`, `Sync`, client
  state, server directives, and errors.
- The public protocol version is 1 and no Agent ABI compatibility surface
  remains.
- Process registration and cooperative Effect tracking are absent from the
  wire protocol, client SDK, pVisor Session state, and pPilot bridge.
- All live runtime clients at checkpoint start must quiesce before pVisor may
  checkpoint.
- Loss or ambiguity causes checkpoint failure, never false success.
- The complete wire contract and its safety limitations can be understood by
  reading `protocol.rs` alone.
- No Debug-plane message or placeholder is added to the Control protocol.

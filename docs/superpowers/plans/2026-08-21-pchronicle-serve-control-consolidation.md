# pChronicle Serve and Control Consolidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Host the authenticated pChronicle Control protocol as an optional `serve` component and migrate pPilot/pVisor from the removed standalone `control` command.

**Architecture:** `serve` resolves either one `--storage` Dataset or a multi-Dataset `--config`, pre-binds the independently requested Warehouse, Control, and Gateway listeners, emits one unified readiness record, and supervises all enabled services with shared shutdown. Control keeps its existing TCP protocol and durable stores; only its process ownership and readiness adapter change.

**Tech Stack:** Rust, Tokio TCP/process primitives, Clap derive validation, Serde JSON, Axum Warehouse/Gateway servers, existing `ChronicleControl` protocol and pChronicle stores.

**Spec:** `docs/superpowers/specs/2026-08-21-pchronicle-serve-control-consolidation-design.md`

## Global Constraints

- `--storage` and `--config` are mutually exclusive and exactly one is required.
- `--listen`, `--control`, and `--gateway` independently enable Warehouse, Control, and Gateway; at least one is required.
- Omitting `--listen` must create no Warehouse HTTP listener.
- `--control` requires `--storage`; `--open` requires `--listen`.
- Warehouse and Control remain loopback-only; Control remains token-authenticated TCP, not HTTP.
- Existing Run control, Attempt registry, and trajectory storage formats must not change.
- Do not add automatic Warehouse refresh after Control writes.
- Remove both the `pchronicle control` command and `ChronicleControlProcessClient` compatibility alias.
- Preserve all unrelated changes in the shared dirty worktree. Do not create implementation commits from files that already contain user changes; use test checkpoints instead.

---

### Task 1: Unified serve readiness protocol

**Files:**
- Modify: `crates/persisting-events/src/control.rs`
- Test: `crates/persisting-events/src/control.rs`

**Interfaces:**
- Produces: `CHRONICLE_SERVE_READY_VERSION: u32`, `ChronicleServeControlReady`, and `ChronicleServeReady`.
- `ChronicleServeReady` has optional `warehouse_endpoint`, `control`, `gateway_endpoint`, and `gateway_admin_endpoint` members with disabled members omitted by Serde.

- [ ] **Step 1: Write failing readiness serialization tests**

Add tests asserting that a Control-only value serializes without Warehouse/Gateway keys and that decoding rejects unknown fields:

```rust
let ready = ChronicleServeReady {
    version: CHRONICLE_SERVE_READY_VERSION,
    warehouse_endpoint: None,
    control: Some(ChronicleServeControlReady {
        endpoint: "127.0.0.1:4000".into(),
        auth_token: "secret".into(),
    }),
    gateway_endpoint: None,
    gateway_admin_endpoint: None,
};
let value = serde_json::to_value(ready).unwrap();
assert!(value.get("warehouse_endpoint").is_none());
assert_eq!(value["control"]["endpoint"], "127.0.0.1:4000");
```

- [ ] **Step 2: Run the focused test and observe the missing-type failure**

Run: `cargo test -p persisting-events serve_ready --lib`

Expected: compilation fails because `ChronicleServeReady` does not exist.

- [ ] **Step 3: Add the readiness types**

Implement:

```rust
pub const CHRONICLE_SERVE_READY_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChronicleServeControlReady {
    pub endpoint: String,
    pub auth_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChronicleServeReady {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warehouse_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ChronicleServeControlReady>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_admin_endpoint: Option<String>,
}
```

- [ ] **Step 4: Run the focused tests**

Run: `cargo test -p persisting-events serve_ready --lib`

Expected: all readiness tests pass.

### Task 2: Embeddable shutdown-aware Control service

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/control.rs`
- Test: `crates/persisting-pchronicle-cli/src/control.rs`

**Interfaces:**
- Consumes: existing `RunControlStore`, `AttemptRegistry`, and Control request handlers.
- Produces: `PreparedControl::bind(storage: &str, listen: SocketAddr) -> Result<PreparedControl>`, `PreparedControl::ready() -> ChronicleServeControlReady`, and `PreparedControl::serve(self, shutdown: impl Future<Output = ()>) -> Result<()>`.

- [ ] **Step 1: Add a failing lifecycle test**

Bind a prepared Control service to `127.0.0.1:0`, start it with a oneshot shutdown future, send a `Ping` envelope using the advertised token, assert `Pong`, signal shutdown, and require the serving future to finish within five seconds.

- [ ] **Step 2: Run the focused test and observe the missing API failure**

Run: `cargo test -p persisting-pchronicle-cli control::tests --lib`

Expected: compilation fails because `PreparedControl` is undefined.

- [ ] **Step 3: Extract listener preparation from `run_control`**

Move store opening, listener binding, endpoint discovery, and token generation into `PreparedControl::bind`. Keep `serve_connection`, `decode_request`, `handle_request`, and response mapping semantically unchanged.

Use this ownership boundary:

```rust
pub(super) struct PreparedControl {
    listener: tokio::net::TcpListener,
    endpoint: SocketAddr,
    auth_token: String,
    control: Arc<RunControlStore>,
    attempts: Arc<AttemptRegistry>,
}
```

Implement a shutdown-aware accept loop:

```rust
loop {
    tokio::select! {
        _ = &mut shutdown => break,
        accepted = self.listener.accept() => {
            let (stream, _) = accepted.context("accept pChronicle control client")?;
            stream.set_nodelay(true).context("configure pChronicle control socket")?;
            let control = Arc::clone(&self.control);
            let attempts = Arc::clone(&self.attempts);
            let auth_token = self.auth_token.clone();
            tokio::spawn(async move {
                if let Err(error) = serve_connection(stream, control, attempts, auth_token).await {
                    eprintln!("pChronicle control request failed: {error:#}");
                }
            });
        }
    }
}
Ok(())
```

Do not write readiness from this module; the `serve` supervisor owns stdout.

- [ ] **Step 4: Run Control unit and process protocol tests**

Run: `cargo test -p persisting-pchronicle-cli control::tests --lib`

Expected: lifecycle, authentication, frame-limit, and request mapping tests pass.

### Task 3: Optional service CLI and unified supervisor

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs`
- Test: `crates/persisting-pchronicle-cli/src/tests.rs`
- Test: `crates/persisting-pchronicle-cli/tests/server_http_contract.rs`

**Interfaces:**
- Consumes: `PreparedControl` and `ChronicleServeReady` from Tasks 1-2.
- Produces: optional `ServeArgs.listen`, optional `ServeArgs.control`, mutually exclusive Dataset source arguments, `resolve_serve_config`, and a supervisor for any non-empty combination of Warehouse/Control/Gateway.

- [ ] **Step 1: Replace old CLI expectations with failing matrix tests**

Assert these parse successfully:

```text
serve --storage /tmp/data --control 127.0.0.1:0
serve --storage /tmp/data --listen 127.0.0.1:0
serve --storage /tmp/data --gateway gateway.toml
serve --config warehouse.toml --listen 127.0.0.1:0
```

Assert these fail in Clap or startup validation:

```text
serve --storage /tmp/data
serve --config warehouse.toml
serve --storage a --config b --listen 127.0.0.1:0
serve --config warehouse.toml --control 127.0.0.1:0
serve --storage /tmp/data --open --control 127.0.0.1:0
```

Also change the product command list expectation to omit `control`.

- [ ] **Step 2: Run the CLI tests and observe failures against the old defaults**

Run: `cargo test -p persisting-pchronicle-cli serve_cli --lib`

Expected: failures show mandatory `--config`, default `--listen`, and missing `--storage`/`--control` support.

- [ ] **Step 3: Implement the new `ServeArgs` contract and Dataset resolution**

Use Clap conflicts/requires plus startup validation. `resolve_serve_config` loads `--config` or creates:

```rust
server::ChronicleServerConfig::mounted(vec![DatasetMount::new(
    DEFAULT_DATASET_NAME,
    storage,
)?])?
```

Validate loopback addresses only for enabled Warehouse and Control listeners. Preserve current Gateway Dataset selection rules, with the automatic storage mount acting as `default`.

- [ ] **Step 4: Add failing readiness and no-implicit-HTTP tests**

Start `serve --storage <temp> --control 127.0.0.1:0`, parse stdout as `ChronicleServeReady`, assert `warehouse_endpoint` is absent, connect to Control and receive `Pong`, and assert stderr does not contain the token. Add a Gateway-only test using pre-bound ephemeral Gateway/admin ports and assert no Warehouse endpoint is published.

- [ ] **Step 5: Implement pre-binding, readiness, and shared supervision**

Change dispatch to pass both output streams:

```rust
Command::Serve(args) => run_serve(args, stdout, stderr).await,
```

Prepare every enabled component before serializing one `ChronicleServeReady` line to stdout. Run enabled services under one stop signal. On external shutdown, stop and drain all services. If a service returns before shutdown, cancel siblings, drain them, and return the original error or an explicit `"<service> stopped unexpectedly"` error. Always finish the Gateway capture writer after Gateway stops.

- [ ] **Step 6: Run the serve and HTTP contract tests**

Run: `cargo test -p persisting-pchronicle-cli serve --lib`

Run: `cargo test -p persisting-pchronicle-cli --test server_http_contract`

Expected: new service matrix and existing read-only HTTP contracts pass.

### Task 4: Process-client and launcher migration

**Files:**
- Modify: `crates/persisting-events/src/control.rs`
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs`
- Modify: `crates/persisting-pvisor/src/pvisor.rs`
- Modify: `crates/persisting-pvisor/src/cli/trajectory.rs`
- Modify: `crates/persisting-ppilot/src/cli.rs`
- Modify: `crates/persisting-ppilot/src/coordination.rs`
- Test: `crates/persisting-pchronicle-cli/tests/control_process.rs`

**Interfaces:**
- Consumes: the Task 1 unified ready envelope and Task 3 Control-only serve mode.
- Produces: `ChronicleServeProcessClient` implementing the unchanged `ChronicleControl` trait.

- [ ] **Step 1: Rename the integration test to the new process adapter and run it red**

Change imports and construction to:

```rust
let client = ChronicleServeProcessClient::spawn(
    env!("CARGO_BIN_EXE_pchronicle"),
    root.path().to_string_lossy(),
).await?;
```

Run: `cargo test -p persisting-pchronicle-cli --test control_process`

Expected: compilation fails because `ChronicleServeProcessClient` does not exist.

- [ ] **Step 2: Replace the process adapter**

Rename the public type and debug label, spawn arguments, and readiness parsing. The child command is exactly:

```rust
Command::new(&binary)
    .arg("serve")
    .arg("--storage")
    .arg(&root_uri)
    .arg("--control")
    .arg("127.0.0.1:0")
```

Decode `ChronicleServeReady`, require `version == CHRONICLE_SERVE_READY_VERSION`, require `control`, validate its loopback endpoint, then perform the existing `Ping` handshake. Remove `ChronicleControlReady` and do not retain a `ChronicleControlProcessClient` alias.

- [ ] **Step 3: Migrate every pPilot/pVisor launch site**

Replace imports and constructors in the four listed launcher files. Keep binary and storage configuration names unchanged because they still identify the pChronicle executable and durable root.

- [ ] **Step 4: Remove the standalone command**

Delete `Command::Control`, `ControlArgs`, and its dispatch arm. Keep `control.rs` as the embedded service module. Update command-tree tests to ensure `control` is rejected as an unknown subcommand.

- [ ] **Step 5: Run process and consumer tests**

Run: `cargo test -p persisting-pchronicle-cli --test control_process`

Run: `cargo test -p persisting-ppilot coordination --lib`

Run: `cargo test -p persisting-pvisor --lib`

Expected: process protocol, coordination, and pVisor tests pass without invoking the removed subcommand.

### Task 5: Documentation and final verification

**Files:**
- Modify: `crates/persisting-pchronicle-cli/README.md`
- Modify: `docs/src/pchronicle/reference/cli.md`
- Modify: `docs/src/pchronicle/guides/serve.md`
- Modify: `docs/src/pchronicle/guides/serve.zh.md`
- Modify: `docs/src/pchronicle/guides/serve-gateway.md`
- Modify: `docs/src/pchronicle/guides/serve-gateway.zh.md`
- Modify: examples returned by `rg -l 'pchronicle serve --config' docs examples crates/persisting-pchronicle-cli/README.md` where the command intends to start Warehouse HTTP

**Interfaces:**
- Documents the final Task 3 CLI and Task 4 migration with no deprecated command.

- [ ] **Step 1: Update reference and guide commands**

Add explicit `--listen 127.0.0.1:8080` to commands that intend to start Warehouse HTTP. Document Control-only and combined examples, the `default` mount created by `--storage`, unified ready stdout, Gateway-only behavior, and the `--config`/`--storage` conflict. Remove statements that Control is a separate executable mode.

- [ ] **Step 2: Scan for stale public names**

Run:

```text
rg -n 'pchronicle control|ChronicleControlProcessClient|ChronicleControlReady|serve --config[^\n]*$' crates docs examples
```

Expected: no stale product/API references; remaining `serve --config` examples include an explicit service option on the same or following command lines.

- [ ] **Step 3: Run formatting and static checks**

Run: `cargo fmt --all -- --check`

Run: `cargo clippy -p persisting-events -p persisting-pchronicle-cli -p persisting-ppilot -p persisting-pvisor --all-targets -- -D warnings`

Expected: both commands exit successfully.

- [ ] **Step 4: Run scoped regression suites**

Run: `cargo test -p persisting-events`

Run: `cargo test -p persisting-pchronicle-cli`

Run: `cargo test -p persisting-ppilot`

Run: `cargo test -p persisting-pvisor`

Expected: all non-environment-dependent tests pass; any pre-existing opt-in test remains explicitly reported as ignored.

- [ ] **Step 5: Build and smoke-test the release binary**

Run: `cargo build -p persisting-pchronicle-cli --release`

Start a release Control-only server against a temporary storage root, parse the ready envelope, perform a protocol `Ping`, verify that no Warehouse endpoint is present, terminate it, and move temporary artifacts to trash.

Expected: release build succeeds, Control responds with `Pong`, no Warehouse socket is advertised, and the child exits cleanly.

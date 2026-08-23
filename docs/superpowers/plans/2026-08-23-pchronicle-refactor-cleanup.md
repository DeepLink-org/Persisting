# pChronicle Refactor and Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Converge pChronicle onto one fenced, bounded, durable canonical append path, align its public event contract with actual storage behavior, and simplify optional features only after compatibility and benchmark gates pass.

**Architecture:** A process-scoped `RawEventAppendService` owns appender reuse, micro-batching, manifest publication, byte admission, acknowledgement classification, and metrics. Control and embedded Gateway use clients of that service; pVisor and pPilot send a required Run-lease writer context and a configured target name. Canonical events remain the fact source, while Storyline and all optional query/format features remain downstream and independently reversible.

**Tech Stack:** Rust, Tokio, Lance, Arrow, DataFusion, Axum, Serde, Clap, existing pChronicle manifest/CAS and test fixtures.

**Spec:** `docs/superpowers/specs/2026-08-23-pchronicle-refactor-cleanup-design.md`

## Global Constraints

- Do not modify TTAS, tiered memory, queue/sampler, search, or `persisting-dlcapt`.
- Do not replace the event manifest, Storyline three-table model, content object reader, or Storyline `CURRENT` pointer.
- Do not add exactly-once append, `event_id` deduplication, `ingest_id`, or a receipt index.
- Do not add a commit cursor in this phase: manifest `fact_rows` is run-global,
  existing replay offsets are session-relative, and no production cursor
  consumer exists.
- Protocol V3 must fail closed; never serde-default a missing writer lease into unfenced append.
- Task 1 adds V3 beside V2 so intermediate commits compile. V2 remains the only
  active producer path until Task 8; Task 9 bumps the protocol and deletes all
  V2 append scaffolding in the same Phase A series.
- Existing event datasets remain readable and are never rewritten in place.
- Optional cleanup ships only after its benchmark or compatibility gate passes.
- Keep every optional cleanup in a separate commit so it can be reverted alone.
- Use targeted tests; do not make excluded subsystem failures part of acceptance.
- Do not commit unless the user asks.

---

## Phase A — Canonical Append Convergence

### Task 1: Add the Control V3 append contract without cutting over

**Files:**
- Modify: `crates/persisting-events/src/control.rs`
- Modify: `crates/persisting-events/src/lib.rs`
- Modify: `crates/persisting-pchronicle-cli/src/control.rs` (temporary exhaustive match)
- Test: `crates/persisting-events/src/control.rs` (`mod tests`)

**Interfaces:**
- Produces: `TrajectoryWriterLease`, `TrajectoryAppendReceipt`,
  `TrajectoryAppendOutcome`, `TrajectoryAppendRequestV3`, and
  `ChronicleControl::append_trajectory_v3` beside the existing V2 API.
- Consumes: existing `EventRecord`, `ChronicleControl`, process client, and memory client.

- [ ] **Step 1: Write failing protocol round-trip tests**

```rust
#[test]
fn trajectory_append_v3_requires_writer_and_target() {
    let request = TrajectoryAppendRequestV3 {
        target: "default".into(),
        agent_id: "agent-a".into(),
        session_id: "session-a".into(),
        root_session_id: Some("run-a".into()),
        writer: TrajectoryWriterLease {
            run_id: "run-a".into(),
            attempt_id: "attempt-a".into(),
            lease_epoch: 7,
        },
        ack_timeout_ms: Some(5_000),
        records: vec![fixture_event()],
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["target"], "default");
    assert_eq!(value["writer"]["lease_epoch"], 7);
    assert!(value.get("storage").is_none());
}

#[test]
fn trajectory_append_v3_does_not_accept_v2_storage_shape() {
    let legacy = serde_json::json!({
        "storage": "/tmp/arbitrary",
        "agent_id": "a",
        "session_id": "s",
        "records": []
    });
    assert!(serde_json::from_value::<TrajectoryAppendRequestV3>(legacy).is_err());
}
```

- [ ] **Step 2: Run the tests and verify the old V2 shape fails the new assertions**

Run: `cargo test -p persisting-events --features control trajectory_append_v3 -- --nocapture`

Expected: FAIL because the V3 types and `target` field do not exist.

- [ ] **Step 3: Add the V3 wire types without bumping the envelope version yet**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryWriterLease {
    pub run_id: String,
    pub attempt_id: String,
    pub lease_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryAppendReceipt {
    pub accepted_records: usize,
    pub fact_version: u64,
    pub manifest_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryAppendRejectionKind {
    InvalidRequest,
    UnknownTarget,
    Fenced,
    ResourceExhausted,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryAppendIndeterminateKind {
    DeadlineExceeded,
    ConnectionLost,
    WorkerFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TrajectoryAppendOutcome {
    Committed { receipt: TrajectoryAppendReceipt },
    Rejected { kind: TrajectoryAppendRejectionKind, message: String },
    Indeterminate { kind: TrajectoryAppendIndeterminateKind, message: String },
}
```

Add `TrajectoryAppendRequestV3` with `target`, required `writer`, and optional
`ack_timeout_ms`. Add `TrajectoryAppendResponseV3` carrying `target`, `run_id`,
`session_id`, and `TrajectoryAppendOutcome`; it does not echo a URI/path. Keep
the existing V2 types unchanged for the still-compiling producers.

- [ ] **Step 4: Add the V3 trait method and request variant**

Update `ChronicleServeProcessClient` serialization and
`MemoryChronicleControl::append_trajectory_v3`. Add an
`AppendTrajectoryV3` wire variant while keeping V2. The memory implementation
keeps a deterministic fact/manifest revision counter per `(target, run_id)` and
must not silently accept a missing writer.

Add an exhaustive CLI match arm that returns a temporary “V3 append service not
enabled” error. Task 7 replaces only that arm with the real service call. This
is migration scaffolding, not a fallback from V3 to the old writer.

In the process client, failure before a connection is established is
`Rejected(Unavailable)`. Once request transmission begins, a write/read timeout
or lost connection is conservatively synthesized as `Indeterminate`; callers
must not infer non-admission from a missing server response.

- [ ] **Step 5: Add validation tests for zero epoch, empty attempt ID, empty batches, and zero timeout**

Validate these at the protocol boundary before opening a socket. The requested
timeout may be omitted but, when present, must be greater than zero. Reject an
empty `records` list as `invalid_request`; do not invent a committed receipt for
a no-op append.

- [ ] **Step 6: Run the additive protocol and downstream compile checks**

Run:

```bash
cargo test -p persisting-events --features control --locked
cargo check -p persisting-pchronicle-cli -p persisting-pvisor -p persisting-ppilot --locked
```

Expected: PASS with both V2 and V3 shapes compiling; V3 never invokes the V2
append implementation.

- [ ] **Step 7: Commit the protocol unit when authorized**

```bash
git add crates/persisting-events/src/control.rs crates/persisting-events/src/lib.rs crates/persisting-pchronicle-cli/src/control.rs
git commit -m "refactor(events): define fenced trajectory append v3"
```

### Task 2: Version the event manifest and support one legacy fence handoff

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/events/manifest.rs`
- Modify: `crates/persisting-pchronicle/src/store/events/mod.rs`
- Test: `crates/persisting-pchronicle/src/store/events/manifest.rs`
- Document: `docs/src/pchronicle/design/trajectory-storage.md`
- Document: `docs/src/pchronicle/design/trajectory-storage.zh.md`

**Interfaces:**
- Produces internally: `EventFenceDomain::{AutoEpoch, RunLease}` and domain-aware activation.
- Consumes: Task 1's lease epoch and Control-derived writer ID.

- [ ] **Step 1: Write failing manifest migration tests**

Add these tests beside the existing activation/fencing tests:

```rust
#[tokio::test]
async fn legacy_manifest_allows_one_run_lease_handoff() { /* assert CAS handoff */ }

#[tokio::test]
async fn run_lease_manifest_rejects_unfenced_auto_activation() { /* assert conflict */ }

#[tokio::test]
async fn run_lease_manifest_rejects_lower_epoch_and_same_epoch_other_writer() {
    /* assert StaleFence and EpochAlreadyOwned */
}

#[tokio::test]
async fn unknown_future_manifest_version_is_rejected() { /* fail closed */ }
```

The legacy fixture must omit the new fields so this test also proves backward
deserialization.

- [ ] **Step 2: Run the focused manifest tests and verify failure**

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib store::events::manifest --locked`

Expected: FAIL because the manifest has no version or fence domain.

- [ ] **Step 3: Add format and domain fields with explicit legacy defaults**

```rust
const EVENT_MANIFEST_FORMAT_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventFenceDomain {
    AutoEpoch,
    RunLease,
}

pub(super) struct EventManifest {
    #[serde(default = "legacy_manifest_format_version")]
    pub format_version: u32,
    #[serde(default = "legacy_fence_domain")]
    pub fence_domain: EventFenceDomain,
    // existing fields remain unchanged
}
```

Do not infer a Run-lease domain from the numeric epoch of an old manifest.
Reject a manifest version newer than the implementation understands.

- [ ] **Step 4: Implement domain-aware activation**

Add `activate_with_domain`. Treat a manifest without a domain as `AutoEpoch`.
Permit `AutoEpoch -> RunLease` exactly once under manifest CAS. Reject
`requested_fence: None` after the domain becomes `RunLease`. Do not add a third
domain for embedded Gateway: it remains an `AutoEpoch` writer unless its root
has explicitly moved to Run-lease ownership.

- [ ] **Step 5: Propagate domain into `RawEventLanceAppender` constructors**

Keep `RawEventLanceAppender::fenced(fence)` as the Run-lease constructor and add
`auto_epoch(writer_id)` for the service's stable process writer. Keep `Default`
only as an offline compatibility wrapper that generates the writer ID once per
appender; do not let either auto constructor open a Run-lease-domain dataset.

- [ ] **Step 6: Run manifest and event-store tests**

Run:

```bash
cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib store::events::manifest --locked
cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib store::events::tests --locked
```

Expected: PASS, including legacy manifest fixtures.

- [ ] **Step 7: Update the storage design documentation and commit when authorized**

Document the one-way domain handoff and the rollback boundary. A pre-V3 binary
may read a migrated root but must never write it: it does not understand the
domain field and cannot enforce the no-downgrade rule. The strict protocol
version ensures spawned producers and the sidecar upgrade together.

```bash
git add crates/persisting-pchronicle/src/store/events/manifest.rs crates/persisting-pchronicle/src/store/events/mod.rs docs/src/pchronicle/design/trajectory-storage.md docs/src/pchronicle/design/trajectory-storage.zh.md
git commit -m "refactor(pchronicle): bind event manifests to writer domains"
```

### Task 3: Return manifest-derived durable receipts

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/events/mod.rs`
- Test: `crates/persisting-pchronicle/src/store/events/tests.rs`

**Interfaces:**
- Produces: `RawEventAppendReceipt`.
- Consumes: existing manifest `fact_version` and `revision`.

- [ ] **Step 1: Write failing durable-receipt tests**

```rust
#[tokio::test]
async fn append_receipt_is_visible_before_return() {
    // Append 2; assert accepted=2 and replay already sees both at return.
}

#[tokio::test]
async fn maintenance_does_not_change_fact_version() {
    // Append, maintain, and assert the logical fact version is unchanged.
}

#[tokio::test]
async fn stale_writer_failure_returns_no_receipt() {
    // A fenced publication cannot be reported as committed.
}
```

- [ ] **Step 2: Run the tests and verify receipt APIs are absent**

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib --locked append_receipt -- --nocapture`

Expected: FAIL to compile until the receipt API is added.

- [ ] **Step 3: Add the core receipt types**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEventAppendReceipt {
    pub accepted_records: usize,
    pub fact_version: u64,
    pub manifest_revision: u64,
}
```

- [ ] **Step 4: Make each partition outcome carry publication evidence**

Change `EventAppendBatchReport` values from `Result<usize>` to a result that
contains `RawEventAppendReceipt`. Construct it only from the manifest returned
by the successful `publish_segment_with_mode` CAS:

```rust
RawEventAppendReceipt {
    accepted_records: appended_rows,
    fact_version: manifest.fact_version,
    manifest_revision: manifest.revision,
}
```

Do not modify the Arrow row schema.

- [ ] **Step 5: Keep replay APIs unchanged**

Document that `replay(session, offset, limit)` is session-relative while
manifest `fact_rows` is run-global. Do not add a cursor or a run-global replay
surface until a production follow/resume caller exists.

- [ ] **Step 6: Run receipt, replay, and maintenance tests**

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib store::events --locked`

Expected: PASS; duplicate `event_id` tests remain unchanged.

- [ ] **Step 7: Commit the receipt unit when authorized**

```bash
git add crates/persisting-pchronicle/src/store/mod.rs crates/persisting-pchronicle/src/store/events/mod.rs crates/persisting-pchronicle/src/store/events/tests.rs
git commit -m "feat(pchronicle): return manifest-derived append receipts"
```

### Task 4: Replace the single appender worker with a keyed append service

**Files:**
- Modify: `crates/persisting-pchronicle/src/append_queue.rs`
- Modify: `crates/persisting-pchronicle/src/storage.rs`
- Test: `crates/persisting-pchronicle/src/append_queue.rs`
- Test: `crates/persisting-pchronicle/tests/production_scale.rs`

**Interfaces:**
- Produces: `RawEventAppendService`, `RawEventAppendClient`, `RawEventAppendRequest`, `RawEventWriterContext`.
- Consumes: Tasks 2–3 domain-aware appender and receipts.

- [ ] **Step 1: Add failing service-reuse tests**

```rust
#[test]
fn fenced_service_reuses_one_epoch_across_single_record_appends() { /* 100 appends */ }

#[test]
fn different_event_roots_or_fences_do_not_share_appender_state() { /* 2 keys */ }
```

Assert one active writer epoch and a segment count bounded by sealing policy,
not by the 100 request count. For different roots, assert independent manifest
state; do not require a separate worker or appender object per root.

- [ ] **Step 2: Run the focused tests and observe current per-service limitations**

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib append_queue::tests --locked`

Expected: FAIL because the worker owns one default appender and jobs carry no
fence.

- [ ] **Step 3: Define the service-facing types**

```rust
pub struct RawEventWriterContext {
    // Public service API; the manifest domain stays internal.
    pub authority: RawEventWriterAuthority,
}

pub enum RawEventWriterAuthority {
    AutoEpoch { writer_id: String },
    RunLease(EventWriterFence),
}

pub struct RawEventAppendRequest {
    pub coords: StoryCoords,
    pub writer: RawEventWriterContext,
    pub records: Vec<EventRecord>, // non-empty; one receipt per request
    pub timeout: Duration,
}

pub struct RawEventAppendService { /* owns worker and shutdown */ }
#[derive(Clone)]
pub struct RawEventAppendClient { /* owns bounded sender and metrics */ }
```

The client exposes one async durable wait for Control and one blocking durable
wait for Gateway's synchronous callback. They share admission and worker state;
they are adapters, not separate append implementations. The async method must
not call a blocking receiver on a Tokio worker thread.

- [ ] **Step 4: Key appender reuse by writer authority**

Introduce an internal ordered key containing the resolved normalized storage
authority—not the caller's target spelling—plus fence domain, epoch, derived
writer ID, and manifest write mode. For Run leases,
derive the writer ID from `attempt_id`; do not add another producer field. The
worker owns `BTreeMap<WriterKey, RawEventLanceAppender>`; each appender keeps its
existing per-root dataset map.

Drain the FIFO queue into a micro-batch, split it only into contiguous runs of
the same key, and process those runs in arrival order. This prevents a takeover
request from being reordered around the stale writer it fences. Do not add a
task, mutex, or scheduler per key in this phase. The existing appender may keep
its bounded concurrency across independent event roots inside one run; further
cross-key concurrency requires benchmark evidence.

- [ ] **Step 5: Move sealing and maintenance scheduling into the service**

Keep the existing thresholds and best-effort maintenance behavior. On service
shutdown, seal final partial segments and await the maintenance worker exactly
once.

- [ ] **Step 6: Update the existing durable, partition, panic, and compaction tests**

Retain all current behavioral assertions. Rename types rather than deleting
coverage.

- [ ] **Step 7: Add the production-scale segment bound test**

Append 10,000 single-record requests under one writer context. Assert:

```rust
assert_eq!(layout.active_epoch, Some(lease_epoch));
assert!(layout.visible_segments <= expected_hierarchy_bound);
assert_eq!(layout.visible_rows, 10_000);
```

- [ ] **Step 8: Run core service and scale tests**

Run:

```bash
cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib append_queue::tests --locked
cargo test -p persisting-pchronicle --no-default-features --features lance-store --test production_scale --locked
```

- [ ] **Step 9: Commit the service unit when authorized**

```bash
git add crates/persisting-pchronicle/src/append_queue.rs crates/persisting-pchronicle/src/storage.rs crates/persisting-pchronicle/tests/production_scale.rs
git commit -m "refactor(pchronicle): unify canonical append service"
```

### Task 5: Add byte admission, acknowledgement deadlines, and metrics

**Files:**
- Modify: `crates/persisting-pchronicle/src/append_queue.rs`
- Modify: `crates/persisting-pchronicle/src/store/mod.rs`
- Test: `crates/persisting-pchronicle/src/append_queue.rs`

**Interfaces:**
- Produces: `RawEventAppendLimits`, `RawEventAppendMetricsSnapshot`, and the three-state durable outcome.
- Consumes: Task 4's service/client/request.

- [ ] **Step 1: Write deterministic failing admission tests**

Add tests for oversize record, total byte exhaustion, exact reservation release,
expired pre-admission timeout, and post-admission timeout. Use barriers/channels,
not sleeps.

- [ ] **Step 2: Run the admission tests and verify the old count-only queue fails**

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib --locked byte_budget -- --nocapture`

- [ ] **Step 3: Add limits with the exact defaults from the design**

```rust
pub struct RawEventAppendLimits {
    pub max_queued_records: NonZeroUsize, // 256
    pub max_event_bytes: NonZeroUsize,    // 4 MiB
    pub max_inflight_bytes: NonZeroUsize, // 64 MiB
    pub max_batch_records: NonZeroUsize,  // 256
    pub max_batch_bytes: NonZeroUsize,    // 8 MiB
    pub batch_delay: Duration,            // 2 ms
    pub ack_timeout: Duration,             // 30 s
}
```

- [ ] **Step 4: Add RAII byte reservations**

Validate route identity, canonicalize, and serialize/size every event before
queue admission; reject the request if any event is invalid or exceeds the
per-event cap. Acquire aggregate record and byte capacity atomically for the
request. Transfer the reservation and canonicalized records into the queued
job; release it after every completion, rejection, worker failure, panic, or
shutdown path.

- [ ] **Step 5: Classify outcomes without adding retry deduplication**

Return `Rejected` only when the service can prove no manifest publication
occurred, including a definitive fence conflict. Use a Tokio oneshot plus
`tokio::time::timeout` for async Control and a bounded blocking completion wait
for Gateway's synchronous callback. Once transmission/admission has begun, an
unresolved timeout becomes `Indeterminate`. Continue processing an
indeterminate job in the worker; do not cancel a possible commit.

The effective wait is `min(request.ack_timeout, limits.ack_timeout)`; a caller
may shorten the 30-second server maximum but cannot widen it.

- [ ] **Step 6: Add the metrics snapshot**

```rust
pub struct RawEventAppendMetricsSnapshot {
    pub admitted: u64,
    pub committed: u64,
    pub rejected_full: u64,
    pub rejected_oversize: u64,
    pub rejected_bytes: u64,
    pub indeterminate: u64,
    pub failed: u64,
    pub inflight_records: usize,
    pub inflight_bytes: usize,
}
```

Do not introduce a metrics framework in this task; expose an atomic snapshot.

- [ ] **Step 7: Run all append-queue tests**

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib append_queue::tests --locked`

Expected: PASS, including exact zero in-flight bytes after worker panic.

- [ ] **Step 8: Commit the admission unit when authorized**

```bash
git add crates/persisting-pchronicle/src/append_queue.rs crates/persisting-pchronicle/src/store/mod.rs
git commit -m "feat(pchronicle): bound append bytes and ack waits"
```

### Task 6: Configure named Control write targets

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs` (`ServeArgs`, `run_serve`)
- Modify: `crates/persisting-pchronicle-cli/src/control.rs`
- Modify: `crates/persisting-pchronicle-cli/src/settings.rs`
- Modify: `crates/persisting-events/src/control.rs` (process spawn configuration)
- Test: `crates/persisting-pchronicle-cli/src/tests.rs`
- Test: `crates/persisting-pchronicle-cli/tests/control_process.rs`

**Interfaces:**
- Produces: repeatable `--control-write-target` and
  `ChronicleServeProcessConfig`.
- Consumes: Task 1's V3 request `target`.

- [ ] **Step 1: Add failing CLI/config tests**

Cover one-root automatic `default`, explicit multi-mount targets, unknown target,
read-only mount, duplicate target name, and a target not present in Dataset
mounts. Reject two writable names that resolve to the same normalized URI so one
event root cannot accidentally acquire two service identities.

- [ ] **Step 2: Add failing Control safety tests**

Send raw V3 envelopes and assert that unknown/non-writable targets fail before
creating a directory or object. Include path-alias and object-URI cases.

- [ ] **Step 3: Run the focused tests**

Run:

```bash
cargo test -p persisting-pchronicle-cli --lib control_write_target --locked
cargo test -p persisting-pchronicle-cli --test control_process --locked control_rejects_unknown_target -- --nocapture
```

- [ ] **Step 4: Resolve writable names against existing mounts**

```rust
type ControlWriteTargets = BTreeMap<String, String>; // mounted name -> URI
```

Build this map once from `ChronicleServerConfig.datasets` filtered by the exact
names passed through `--control-write-target`. Do not create another mount
model or copy of Dataset metadata. `PreparedControl` receives the map, resolves
only by exact target name, and constructs `StoryCoords` from its URI.

Do not add a per-agent ACL in this phase. The process token, loopback binding,
named-target allow-list, and route identity check are the complete boundary.

- [ ] **Step 5: Add process spawn configuration without storage dependencies**

```rust
pub struct ChronicleServeProcessConfig {
    pub control_root: String,
    pub writable_targets: BTreeMap<String, String>,
}
```

Keep `ChronicleServeProcessClient::spawn(binary, root)` as a convenience that
registers only `default=root`. Add `spawn_with_config` for pPilot's separate
trajectory mount.

- [ ] **Step 6: Prepare V3 coordinates without request-controlled URIs**

Add the target-resolution/validation helper used by the V3 handler. It resolves
`request.target` first and builds coordinates only from the configured URI; no
request field may reach `StoryCoords::new` as a root URI. The handler still
returns Task 1's temporary unavailable response until Task 7 supplies the
shared append client—do not introduce an interim direct-store write.

- [ ] **Step 7: Run CLI unit and process tests outside the network sandbox if needed**

Run:

```bash
cargo test -p persisting-pchronicle-cli --lib --locked control::tests
cargo test -p persisting-pchronicle-cli --test control_process --locked
```

- [ ] **Step 8: Commit the target-confinement unit when authorized**

```bash
git add crates/persisting-events/src/control.rs crates/persisting-pchronicle-cli/src/control.rs crates/persisting-pchronicle-cli/src/lib.rs crates/persisting-pchronicle-cli/src/settings.rs crates/persisting-pchronicle-cli/src/tests.rs crates/persisting-pchronicle-cli/tests/control_process.rs
git commit -m "feat(pchronicle): confine control writes to named targets"
```

### Task 7: Make Control and embedded Gateway share the append service

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs`
- Modify: `crates/persisting-pchronicle-cli/src/control.rs`
- Modify: `crates/persisting-pchronicle-cli/src/gateway_capture.rs`
- Test: `crates/persisting-pchronicle-cli/src/gateway_capture.rs`
- Test: `crates/persisting-pchronicle-cli/tests/control_process.rs`

**Interfaces:**
- Consumes: Task 4's service/client and Task 5's limits/outcomes.
- Produces: one process-owned service and clients for both server components.

- [ ] **Step 1: Add a failing shared-service integration test**

Start `serve` with Control and Gateway, append through both, and assert the
metrics snapshot reflects one service and one configured byte budget.

- [ ] **Step 2: Start the service in `run_serve` before preparing components**

Create one `RawEventAppendService`; pass clients into `PreparedControl::bind`
and `prepare_gateway`. Preserve the Gateway-selected manifest publication mode
in each writer key.

- [ ] **Step 3: Remove independent writer creation from Gateway capture**

Change `gateway_capture_sink` to accept `RawEventAppendClient` and use its
blocking adapter. Keep the current callback error mapping, but map
committed/rejected/indeterminate explicitly. Control uses the async adapter so
it never blocks a Tokio worker while awaiting durable publication.

- [ ] **Step 4: Replace Control's `RawEventLanceStore::append_events` call**

Replace Task 1's temporary `AppendTrajectoryV3` rejection with the service
submission. Submit the request's non-empty record vector as one
`RawEventAppendRequest`.
The worker may flatten adjacent jobs into one Lance micro-batch, but completes
each job only after the shared manifest publication and reports that job's own
accepted record count with the published fact/manifest versions. Never split
one Control request into independently acknowledged record jobs.

Before submission, validate the writer against durable local state using this
order: a matching `RunControlRecord.commit` is accepted; otherwise a matching
unexpired Run lease plus bound attempt is accepted; otherwise a matching active,
unexpired Attempt record is accepted. A terminal/expired Attempt, no record, or
a different epoch/attempt is rejected.
This permits pPilot's post-commit result event while fencing stale pVisor
attempts.

The V2 handler remains untouched and is used only by producers not yet switched
in Task 8. V3 must never fall back to it after a service error.

- [ ] **Step 5: Finish the service once during coordinated shutdown**

Stop accepting new requests, wait for Control/Gateway tasks, then call service
`finish`. Preserve the existing rule that successful canonical append is not
reclassified as failed when best-effort maintenance fails.

- [ ] **Step 6: Run shared Control/Gateway tests**

Run:

```bash
cargo test -p persisting-pchronicle-cli --lib gateway_capture --locked
cargo test -p persisting-pchronicle-cli --test control_process --locked
```

- [ ] **Step 7: Verify no service path calls direct append**

Run: `rg -n "RawEventLanceStore|append_events\(" crates/persisting-pchronicle-cli/src/control.rs crates/persisting-pchronicle-cli/src/gateway_capture.rs`

Expected: no direct store append call.

- [ ] **Step 8: Commit the wiring unit when authorized**

```bash
git add crates/persisting-pchronicle-cli/src/lib.rs crates/persisting-pchronicle-cli/src/control.rs crates/persisting-pchronicle-cli/src/gateway_capture.rs crates/persisting-pchronicle-cli/tests/control_process.rs
git commit -m "refactor(pchronicle): share append service across control and gateway"
```

### Task 8: Propagate writer leases from pVisor and pPilot

**Files:**
- Modify: `crates/persisting-pvisor/src/cli/trajectory.rs`
- Modify: `crates/persisting-pvisor/src/cli/run.rs`
- Modify: `crates/persisting-pvisor/src/pvisor.rs`
- Create: `crates/persisting-pvisor/tests/chronicle_sidecar.rs`
- Modify: `crates/persisting-ppilot/src/sink_traj.rs`
- Modify: `crates/persisting-ppilot/src/cli.rs`
- Test: `crates/persisting-ppilot/src/sink_traj.rs`

**Interfaces:**
- Consumes: `TrajectoryAppendRequestV3`, `append_trajectory_v3`, and named
  targets from Tasks 1 and 6.
- Produces: every pVisor/pPilot append carries current Run lease ownership.

- [ ] **Step 1: Add a pPilot unit test with a recording `ChronicleControl`**

Assert `LanceResultSink` forwards `TaskResult.run_id`, `attempt_id`, and
`lease_epoch`; Control derives the stable storage writer ID from `attempt_id`.
Also assert the sink selects the configured target name. Exercise all outcomes:
Committed keeps the `seen` reservation, a definitive Rejected result releases
it for a safe retry, and Indeterminate logs a warning but does not schedule an
automatic retry. The terminal RunCommit remains the authoritative pPilot result,
so avoiding a duplicate optional trajectory event is the safer policy.

- [ ] **Step 2: Add a pVisor black-box stale-writer test**

Start a sidecar, publish Attempt epoch 7, append several events, publish epoch
8, and assert the epoch-7 sink is fenced while epoch 8 commits. Also assert
repeated epoch-7 appends did not create one epoch per event.

- [ ] **Step 3: Run the new tests and verify missing writer propagation**

Run:

```bash
cargo test -p persisting-ppilot --lib sink_traj --locked
cargo test -p persisting-pvisor --test chronicle_sidecar --locked -- --nocapture
```

Expected: FAIL until the writer context reaches both request builders.

- [ ] **Step 4: Pass Run context into `chronicle_sink`**

Make sink construction receive `run_id`, `attempt_id`, and `lease_epoch` after
the Run attempt exists. Build `TrajectoryAppendRequestV3` and call
`append_trajectory_v3`; do not derive lease ownership from event payload JSON.
Map typed rejection and indeterminate outcomes through `EventAppendErrorKind`;
do not collapse them into an unclassified transport string.

- [ ] **Step 5: Publish the active Attempt to the trajectory sidecar**

Even when orchestration uses a separate Attempt registry, register the same
`run_id/attempt_id/lease_epoch` with the trajectory sidecar. Continue publishing
to the orchestration registry; do not merge their persistence records here.

- [ ] **Step 6: Configure pPilot's process mounts before spawning Control**

Compute the optional trajectory root before creating the process client. Spawn
with `default=<control-root>` and `trajectory=<trajectory-root>`, mark
`trajectory` writable, and configure `LanceResultSink` with that target.

- [ ] **Step 7: Run pVisor, pPilot, and Control integration tests**

Run:

```bash
cargo test -p persisting-ppilot --lib --locked sink_traj
cargo test -p persisting-pvisor --test chronicle_sidecar --locked
cargo test -p persisting-pchronicle-cli --test control_process --locked
```

- [ ] **Step 8: Commit producer wiring when authorized**

```bash
git add crates/persisting-pvisor/src/cli/trajectory.rs crates/persisting-pvisor/src/cli/run.rs crates/persisting-pvisor/src/pvisor.rs crates/persisting-pvisor/tests/chronicle_sidecar.rs crates/persisting-ppilot/src/sink_traj.rs crates/persisting-ppilot/src/cli.rs
git commit -m "fix(persisting): fence trajectory writes with run leases"
```

### Task 9: Align identity, ordering, duplicate, and ACK documentation

**Files:**
- Modify: `crates/persisting-events/src/control.rs`
- Modify: `crates/persisting-events/src/lib.rs`
- Modify: `crates/persisting-pchronicle/src/store/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/events/rows.rs`
- Modify: `crates/persisting-agentctl/src/runtime.rs`
- Modify: `crates/persisting-ppilot/src/coordination.rs`
- Modify: `crates/persisting-pchronicle/src/store/run_control.rs` (tests)
- Modify: `crates/persisting-pchronicle-cli/src/control.rs`
- Modify: `crates/persisting-pchronicle-cli/tests/control_process.rs`
- Modify: `docs/src/rfcs/0003-pchronicle-ownership.md`
- Modify: `docs/src/rfcs/0007-events-contract-pchronicle-sidecar.md`
- Test: `crates/persisting-pchronicle/src/store/events/tests.rs`

**Interfaces:**
- Produces: one public semantic contract; `EventRecord::producer_seq()` compatibility accessor.
- Consumes: Task 3 durable receipt and Task 5 outcome classes.

- [ ] **Step 1: Replace the conflicting identity test with fail-closed behavior**

Test that missing route identity is filled, exact identity is accepted, and a
conflicting `session_id`, `agent_id`, `run_id`, or `storyline_id` is rejected
before any manifest fact state advances.

- [ ] **Step 2: Add ordering and duplicate contract tests**

Keep duplicate `event_id` tests. Assert producer sequence may repeat and that
physical append order—not `seq` sorting—controls replay.

- [ ] **Step 3: Add the compatibility accessor without renaming the wire field**

```rust
impl EventRecord {
    pub fn producer_seq(&self) -> u64 { self.seq }
}
```

Update Rustdoc to call `seq` producer evidence. Do not change serialized JSON or
the Lance `seq` column in this plan.

- [ ] **Step 4: Enforce conflict rejection in canonicalization**

Replace `fill_missing_identity` with a helper that fills `None` and rejects a
different `Some`. Keep historic replay behavior unchanged.

- [ ] **Step 5: Rewrite the RFC semantics exactly**

Document:

- physical committed fact order is replay truth;
- `seq` is producer evidence;
- duplicate `event_id` is valid and no dedup is performed;
- a definitive rejection is safe to retry with respect to duplicates, while
  retry after an indeterminate outcome explicitly accepts possible duplicate
  facts;
- pChronicle does not require an indeterminate producer to reuse or advance its
  local `seq`;
- Run lease epoch enters the manifest fence.

- [ ] **Step 6: Cut over the strict V3 protocol and delete V2 append**

Bump `CHRONICLE_CONTROL_VERSION` to `3`. Delete the V2
`TrajectoryAppendRequest/Response`, request variant, trait method, handler, and
tests; retain only the explicitly named V3 types/method. Raw V2 envelopes must
fail version/shape validation. This is the mandatory deletion point for the
temporary scaffolding introduced by Task 1.

- [ ] **Step 7: Remove the unused high-watermark placeholder**

Delete `RunCommitRequest.event_high_watermark` and update its constructors. A
repository scan shows pPilot always supplies `None`, no reader consumes it, and
the only non-`None` value is a unit-test fixture. Do not retain the field to
pretend this phase has a cursor contract.

- [ ] **Step 8: Run contract tests**

Run:

```bash
cargo test -p persisting-events --features control --locked
cargo test -p persisting-agentctl --lib --locked
cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib store::events::tests --locked
cargo test -p persisting-ppilot --lib coordination --locked
cargo test -p persisting-pchronicle-cli --test control_process --locked
```

- [ ] **Step 9: Commit contract convergence when authorized**

```bash
git add crates/persisting-events/src/control.rs crates/persisting-events/src/lib.rs crates/persisting-agentctl/src/runtime.rs crates/persisting-ppilot/src/coordination.rs crates/persisting-pchronicle/src/store/mod.rs crates/persisting-pchronicle/src/store/events/rows.rs crates/persisting-pchronicle/src/store/events/tests.rs crates/persisting-pchronicle/src/store/run_control.rs crates/persisting-pchronicle-cli/src/control.rs crates/persisting-pchronicle-cli/tests/control_process.rs docs/src/rfcs/0003-pchronicle-ownership.md docs/src/rfcs/0007-events-contract-pchronicle-sidecar.md
git commit -m "refactor(pchronicle): converge canonical event semantics"
```

### Task 10: Add crash-window and main-chain acceptance tests

**Files:**
- Modify: `crates/persisting-pchronicle/tests/langfuse_backend_faults.rs`
- Modify: `crates/persisting-pchronicle/tests/production_scale.rs`
- Modify: `crates/persisting-pchronicle-cli/tests/control_process.rs`
- Modify: `crates/persisting-pchronicle/src/store/events/manifest.rs` (unit tests)

**Interfaces:**
- Consumes: completed Phase A behavior.
- Produces: the release gate for deleting legacy service-path append.

- [ ] **Step 1: Prove unpublished-segment behavior at the manifest boundary**

In manifest unit tests, create a valid private segment without publishing it and
assert the visible snapshot ignores it. Add a CAS-conflict test proving a stale
writer cannot publish that segment. Use existing internal test helpers; do not
add a production failpoint framework.

- [ ] **Step 2: Extend the process-level SIGKILL coverage**

Keep the existing kill-after-ack test. Add a Control child case that sends a V3
append, deliberately does not read the response, waits until the manifest
either advances or the bounded probe expires, then kills the process and
restarts. Assert:

- every record covered by a returned `Committed` receipt is replayable after
  restart and the visible manifest fact version is at least the receipt value;
- an unacknowledged append may be absent or present, matching `Indeterminate`;
- temporary/private segments are not treated as visible facts.

This covers the contract without shipping timing hooks or another injectable
storage abstraction solely for tests.

- [ ] **Step 3: Add stale writer and 10k-request assertions to the process test**

Use real Control V3 requests, not direct store calls.

- [ ] **Step 4: Run Phase A's complete targeted gate**

Run:

```bash
cargo fmt --check -p persisting-agentctl -p persisting-events -p persisting-pchronicle -p persisting-pchronicle-cli -p persisting-pvisor -p persisting-ppilot
cargo test -p persisting-agentctl --lib --locked
cargo test -p persisting-events --features control --locked
cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib store::events --locked
cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib append_queue --locked
cargo test -p persisting-pchronicle --no-default-features --features lance-store --test production_scale --locked
cargo test -p persisting-pchronicle --no-default-features --features lance-store --test langfuse_backend_faults --locked
cargo test -p persisting-pchronicle-cli --test control_process --locked
cargo test -p persisting-pvisor --test chronicle_sidecar --locked
cargo test -p persisting-ppilot --lib sink_traj --locked
```

- [ ] **Step 5: Remove temporary legacy service-path escape hatches**

Delete any rollout flag that permits Control V3 to fall back to automatic
unfenced append. Keep only the manifest legacy-domain handoff reader. Run
`rg -n '\bTrajectoryAppendRequest\b|\bappend_trajectory\b' crates -g '*.rs'`
and confirm no V2 type/method remains; explicit `*V3` names are expected.

- [ ] **Step 6: Commit the Phase A gate when authorized**

```bash
git add crates/persisting-pchronicle/src/store/events/manifest.rs crates/persisting-pchronicle/tests/langfuse_backend_faults.rs crates/persisting-pchronicle/tests/production_scale.rs crates/persisting-pchronicle-cli/tests/control_process.rs crates/persisting-pvisor/tests/chronicle_sidecar.rs
git commit -m "test(pchronicle): cover append fencing and crash windows"
```

**Phase A exit gate:** Do not begin optional cleanup until every command in
Task 10 Step 4 passes and the 10k sidecar test demonstrates bounded segments.

---

## Phase B — Evidence-Gated Cleanup

### Task 11: Make Server acceleration reproducible, then split or remove it

**Files:**
- Create: `benchmark/pchronicle/server_acceleration.py`
- Create: `benchmark/pchronicle/test_server_acceleration.py`
- Create: `benchmark/pchronicle/server-acceleration-current.json`
- Modify: `benchmark/pchronicle/README.md`
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs` (`ServeArgs`)
- Modify if retained: `crates/persisting-pchronicle-cli/src/server/acceleration.rs`
- Modify if removed: `crates/persisting-pchronicle-cli/src/server/mod.rs`
- Test: `crates/persisting-pchronicle-cli/src/server/tests.rs`
- Test: `crates/persisting-pchronicle-cli/tests/server_http_contract.rs`

**Interfaces:**
- Produces: reproducible cold/warm/RSS evidence and a deterministic retain/remove decision.
- Consumes: existing 2026-08-12 result as historical comparison only.

- [ ] **Step 1: Write the benchmark runner test before the runner**

Add a Python unit test that validates the result JSON contains:

```python
REQUIRED = {
    "fixture", "cold_ms", "warm_http_p50_ms", "warm_http_p95_ms",
    "unaccelerated_http_p95_ms", "candidate_sources",
    "incremental_process_rss_kib"
}
```

Run: `python3 -m unittest benchmark/pchronicle/test_server_acceleration.py`

Expected: FAIL because the runner/result schema does not exist.

- [ ] **Step 2: Add one operational/benchmark switch**

Add `--source-routing auto|off`, defaulting to `auto`. `off` bypasses only SQL
source routing; run summaries stay enabled. Use the same switch as the rollback
lever if a retained index regresses. Do not add per-index tuning flags.

- [ ] **Step 3: Implement a runner for the 113k-row/211-source fixture**

The runner must start the current release binary twice—routing enabled and
disabled—sample point/list/aggregate 20 times after warmup, and write one JSON
file. Reuse the seeded fixture generator in `benchmark/pchronicle/bench.py` (or
extract that generator once); do not depend on the historical `/tmp` fixture.
Record commit, toolchain, machine profile, and exact commands. It must not reuse
the old ClickHouse comparison as a current A/B result.

Run the unit test again and require PASS before launching the expensive fixture.

- [ ] **Step 4: Run and check in the current result**

Run: `python3 benchmark/pchronicle/server_acceleration.py --release --output benchmark/pchronicle/server-acceleration-current.json`

- [ ] **Step 5: Split run summaries, then apply per-route decision gates**

First split `RunSummaryCache` from `SourceRoutingAcceleration`; the summary
cache remains in every outcome. Evaluate identity/point and partition/list or
aggregate routing independently:

- retain point routing only if its warm P95 is at least 2x faster than
  unaccelerated and no greater than 10 ms;
- retain each list/aggregate routing surface only if its warm P95 is at least
  2x faster than the matching unaccelerated path;
- retain no routing surface if cold index build exceeds 250 ms or incremental
  RSS exceeds 128 MiB on the fixture.

Delete the maps and SQL rewrite branches for every surface that fails. If all
routing fails, return compatibility fields `source_routing="disabled"` and
`candidate_sources=null` for one release. Preserve current HTTP fields and
equivalence tests for any retained surface.

- [ ] **Step 6: Run server behavior and contract tests**

Run:

```bash
cargo test -p persisting-pchronicle-cli --lib server::tests --locked
cargo test -p persisting-pchronicle-cli --test server_http_contract --locked
```

- [ ] **Step 7: Commit benchmark and selected implementation together when authorized**

```bash
git add benchmark/pchronicle/server_acceleration.py benchmark/pchronicle/test_server_acceleration.py benchmark/pchronicle/server-acceleration-current.json benchmark/pchronicle/README.md crates/persisting-pchronicle-cli/src/lib.rs crates/persisting-pchronicle-cli/src/server/acceleration.rs crates/persisting-pchronicle-cli/src/server/mod.rs crates/persisting-pchronicle-cli/src/server/tests.rs crates/persisting-pchronicle-cli/tests/server_http_contract.rs
git commit -m "refactor(pchronicle): evidence-gate server source routing"
```

### Task 12: Make foreign unknown-field preservation explicit

**Files:**
- Modify: `crates/persisting-pchronicle/src/document.rs`
- Modify: `crates/persisting-pchronicle/src/formats/unknown_fields.rs`
- Modify: `crates/persisting-pchronicle/src/convert/atif.rs`
- Modify: `crates/persisting-pchronicle/src/convert/actf.rs`
- Modify: `crates/persisting-pchronicle/src/formats/openai_corpus.rs`
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs` (`ImportArgs`, `ExportArgs`)
- Modify: `crates/persisting-pchronicle-cli/src/exchange.rs`
- Test: `crates/persisting-pchronicle/tests/conversion_semantics.rs`
- Test: `crates/persisting-pchronicle-cli/tests/import_export_roundtrip.rs`

**Interfaces:**
- Produces: `DocumentCodecOptions::preserve_foreign_unknowns` and `--preserve-foreign-unknowns`.
- Preserves: legacy envelope reads and same-format round trips.

- [ ] **Step 1: Write failing codec policy tests**

Cover same-format unknown roundtrip, legacy envelope read, cross-format export
without opt-in returning a specific error, opt-in writing `_storyline`, and
reserved-key collision remaining fail closed.

- [ ] **Step 2: Add finite default budgets**

Change defaults to 10,000 logical unknown fields and 16 MiB encoded unknown
bytes. Add a test that all checked-in fixtures remain within both limits.

- [ ] **Step 3: Add the codec option and CLI flag**

```rust
pub struct DocumentCodecOptions {
    pub unknown_fields: UnknownFieldLimits,
    pub preserve_foreign_unknowns: bool,
}
```

Default is `false`. Export must error, not silently discard, when foreign
unknowns exist and the option is false.

- [ ] **Step 4: Route every converter through the single policy**

Remove unconditional envelope writes from ATIF, ACTF, and OpenAI exporters.
Do not replace the entire mechanism with `serde(flatten)`; keep source namespace
and carrier binding for the explicit cross-format path.

- [ ] **Step 5: Run conversion and CLI round-trip tests**

Run:

```bash
cargo test -p persisting-pchronicle --no-default-features --features lance-store --test conversion_semantics --locked
cargo test -p persisting-pchronicle-cli --test import_export_roundtrip --locked
```

- [ ] **Step 6: Commit the policy change when authorized**

```bash
git add crates/persisting-pchronicle/src/document.rs crates/persisting-pchronicle/src/formats/unknown_fields.rs crates/persisting-pchronicle/src/convert/atif.rs crates/persisting-pchronicle/src/convert/actf.rs crates/persisting-pchronicle/src/formats/openai_corpus.rs crates/persisting-pchronicle-cli/src/lib.rs crates/persisting-pchronicle-cli/src/exchange.rs crates/persisting-pchronicle/tests/conversion_semantics.rs crates/persisting-pchronicle-cli/tests/import_export_roundtrip.rs
git commit -m "refactor(pchronicle): make foreign-field preservation explicit"
```

### Task 13: Demote `unknown_key_counts` to a derived compatibility cache

**Files:**
- Modify: `crates/persisting-pchronicle/src/formats/storyline.rs`
- Modify: `crates/persisting-pchronicle/src/formats/unknown_fields.rs`
- Modify: `crates/persisting-pchronicle/src/document.rs`
- Modify: `crates/persisting-pchronicle/src/agenticmd/validate.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/model.rs`
- Test: `crates/persisting-pchronicle/src/document.rs`
- Test: `crates/persisting-pchronicle/src/store/storyline/tests.rs`

**Interfaces:**
- Consumes: Task 12's finite unknown-field policy.
- Produces: one documented authority (`unknown_fields`) while preserving the
  current JSON key and Lance column.

- [ ] **Step 1: Add authority and compatibility tests**

Read Storyline JSON with a missing count cache and an old runs table containing
`unknown_key_counts_json`; assert both expose counts derived from
`unknown_fields`. Feed a stale serialized cache and assert validation reports
the mismatch without treating the cache as fact.

- [ ] **Step 2: Make authority explicit at boundaries**

Document the public field as a compatibility cache. Add one normalization
helper: when the cache is empty, populate it from `unknown_fields`; when it is
non-empty and stale, return the existing validation error. Call the helper from
the Storyline document decoder and Storyline Lance restore boundary. At write
boundaries, compute counts from `unknown_fields` instead of branching on the
cached value. Warnings and policy decisions do the same.

- [ ] **Step 3: Keep the existing wire and Lance schemas**

Do not add custom `Deserialize`, a Storyline schema version, or a dual Lance
reader. Keep `unknown_key_counts` and `unknown_key_counts_json` until a future
semver/storage-schema migration is required for another reason. Removing a
small derived map does not justify those compatibility branches on its own.

- [ ] **Step 4: Run focused format and Storyline-store tests**

Run:

```bash
cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib formats::storyline --locked
cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib agenticmd --locked
cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib store::storyline --locked
```

- [ ] **Step 5: Commit the authority cleanup when authorized**

```bash
git add crates/persisting-pchronicle/src/formats/storyline.rs crates/persisting-pchronicle/src/formats/unknown_fields.rs crates/persisting-pchronicle/src/document.rs crates/persisting-pchronicle/src/agenticmd/validate.rs crates/persisting-pchronicle/src/store/storyline/model.rs crates/persisting-pchronicle/src/store/storyline/tests.rs
git commit -m "refactor(pchronicle): make unknown fields authoritative"
```

### Task 14: Benchmark and select the Storyline content default

**Files:**
- Modify: `crates/persisting-pchronicle/benches/lance_vs_json.rs`
- Modify: `benchmark/pchronicle/bench.py`
- Modify: `benchmark/pchronicle/test_bench.py`
- Create: `benchmark/pchronicle/content-offload-current.json`
- Modify: `crates/persisting-pchronicle/src/store/storyline/content.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs`
- Test: `crates/persisting-pchronicle/src/store/storyline/tests.rs`

**Interfaces:**
- Produces: `StorylineContentMode::{Inline, Externalize { threshold }}` and an evidence-selected default.
- Preserves: historical descriptor hydration and `objects.lance` reader/GC.

- [ ] **Step 1: Extend the benchmark with inline/externalized paired scenarios**

First add failing `test_bench.py` cases for the new suite, required metrics, and
recorded JSON equality. Run
`python3 -m unittest benchmark/pchronicle/test_bench.py` and observe failure.
Then record payload percentiles, write time, full-read time, projected-read
time, store bytes, and object count for identical large-payload input. Add
`content-offload` to the existing `bench.py run --suite` choices; do not create
a parallel benchmark CLI. Add a general `--record-json PATH` option that writes
the same `raw-report.json` payload to an explicitly checked-in evidence path,
then rerun the unit tests to PASS.

- [ ] **Step 2: Run the benchmark and check in its machine-readable result**

Run:

```bash
python3 benchmark/pchronicle/bench.py run --suite content-offload --output target/pchronicle-benchmark/content-offload --record-json benchmark/pchronicle/content-offload-current.json
```

- [ ] **Step 3: Apply the exact default gate**

Choose Inline only when its full-read and projected-read latency are each no
greater than 1.25x, and its store bytes no greater than 1.5x Externalize.
Otherwise retain Externalize at 64 KiB. Record the selected mode and numbers in
the JSON result.

- [ ] **Step 4: Add the explicit mode without removing the object dataset**

Inline mode keeps new values inline but still hydrates historical
descriptors. Do not make `objects_version` optional and do not stop creating
`objects.lance` in this task.

- [ ] **Step 5: Test both modes and old-data reads**

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib store::storyline --locked`

- [ ] **Step 6: Commit benchmark and policy when authorized**

```bash
git add crates/persisting-pchronicle/benches/lance_vs_json.rs benchmark/pchronicle/bench.py benchmark/pchronicle/test_bench.py benchmark/pchronicle/content-offload-current.json crates/persisting-pchronicle/src/store/storyline/content.rs crates/persisting-pchronicle/src/store/storyline/mod.rs crates/persisting-pchronicle/src/store/storyline/tests.rs
git commit -m "refactor(pchronicle): evidence-gate content externalization"
```

### Task 15: Remove the unused revision catalog and withdraw its RFC

**Files:**
- Delete: `crates/persisting-pchronicle/src/revision.rs`
- Modify: `crates/persisting-pchronicle/src/lib.rs`
- Modify: `crates/persisting-pchronicle/src/storage.rs`
- Modify: `docs/src/rfcs/0005-pchronicle-revision-lineage.md`
- Test: `crates/persisting-pchronicle-cli/src/server/tests.rs`

**Interfaces:**
- Removes: `RevisionRow`, `read_revisions`, `write_revisions`, and `revision_dataset_path`.
- Preserves: existing user files named `revisions.lance`; no deletion or migration command is added.

- [ ] **Step 1: Add a repository-level dead-surface assertion**

Run and record the expected current references:

```bash
rg -n "write_revisions|read_revisions|RevisionRow|revision_dataset_path" crates -g '*.rs'
```

Expected before change: only `revision.rs` and `storage.rs`.

- [ ] **Step 2: Remove the module, re-export, and isolated test**

Delete the code file; do not touch any data directory.

- [ ] **Step 3: Mark RFC-0005 Withdrawn/Unimplemented**

State that the accepted proposal never acquired a production writer and that
future lineage work requires a new RFC. Task 17 updates RFC-0006 after this
withdrawal lands.

- [ ] **Step 4: Run API/server tests and a dead-reference scan**

Run:

```bash
cargo check -p persisting-pchronicle -p persisting-pchronicle-cli --locked
cargo test -p persisting-pchronicle-cli --lib server::tests --locked
rg -n "write_revisions|read_revisions|RevisionRow|revision_dataset_path" crates -g '*.rs'
```

Expected final scan: no matches.

- [ ] **Step 5: Commit removal when authorized**

```bash
git add crates/persisting-pchronicle/src/revision.rs crates/persisting-pchronicle/src/lib.rs crates/persisting-pchronicle/src/storage.rs docs/src/rfcs/0005-pchronicle-revision-lineage.md
git commit -m "refactor(pchronicle): remove unused revision catalog"
```

### Task 16: Remove S3 from library defaults

**Files:**
- Modify: `crates/persisting-pchronicle/Cargo.toml`
- Modify: `.github/workflows/ci.yml`
- Modify: `crates/persisting-pchronicle/README.md`
- Modify: `docs/src/pchronicle/get-started.md`
- Modify: `docs/src/pchronicle/get-started.zh.md`

**Interfaces:**
- Changes: library default features from `lance-store,s3-store` to `lance-store`.
- Preserves: CLI's explicit `s3-store` feature and S3 storage format compatibility.

- [ ] **Step 1: Change the default feature and add an explicit CI matrix**

```toml
[features]
default = ["lance-store"]
```

CI must run one minimal Lance command and one `--features s3-store` integration
command.

- [ ] **Step 2: Verify dependency surfaces**

Run:

```bash
cargo tree -p persisting-pchronicle --no-default-features --features lance-store --locked
cargo tree -p persisting-pchronicle --features s3-store --locked
```

Confirm AWS crates appear only in the explicit S3 tree.

- [ ] **Step 3: Run both build modes**

Run:

```bash
cargo check -p persisting-pchronicle --no-default-features --features lance-store --locked
cargo check -p persisting-pchronicle --features s3-store --locked
cargo check -p persisting-pchronicle-cli --locked
```

- [ ] **Step 4: Document the feature change and commit when authorized**

```bash
git add crates/persisting-pchronicle/Cargo.toml crates/persisting-pchronicle/README.md .github/workflows/ci.yml docs/src/pchronicle/get-started.md docs/src/pchronicle/get-started.zh.md
git commit -m "build(pchronicle): make S3 an explicit library feature"
```

### Task 17: Reduce the Vortex proposal to an experiment charter

**Files:**
- Modify: `docs/src/rfcs/0006-pchronicle-vortex-backend.md`
- Modify: `docs/src/rfcs/index.md`
- Verify: `docs/mkdocs.yml`

**Interfaces:**
- Produces: a Proposed experimental charter at the same path and URL.
- Removes: unapproved physical schema, directory, LSM, and compaction design.

- [ ] **Step 1: Preserve only the experiment contract**

The rewritten RFC contains: motivation, isolation boundary, source snapshot,
correctness equivalence, resource budget, benchmark matrix, go/no-go thresholds,
rollback, and deletion criteria.

Remove its dependency on RFC-0005 as implemented current state; identify that
RFC as withdrawn historical context instead.

- [ ] **Step 2: State non-goals explicitly**

No default feature, no canonical facts, no dual write, no public unified backend
trait, no production cutover, and no dependency before Phase A completes.

- [ ] **Step 3: Remove speculative implementation detail**

Delete proposed physical schemas, directory layouts, compaction algorithms, and
phase-specific Rust APIs. Git history remains the archive.

- [ ] **Step 4: Verify documentation links**

Run: `rg -n "0006-pchronicle-vortex-backend|Vortex" docs/src docs/mkdocs.yml`

Expected: all links still use the unchanged RFC path.

- [ ] **Step 5: Commit the charter when authorized**

```bash
git add docs/src/rfcs/0006-pchronicle-vortex-backend.md docs/src/rfcs/index.md
git commit -m "docs(pchronicle): reduce Vortex RFC to experiment charter"
```

---

## Final Verification

### Task 18: Run the release gate and publish the cleanup record

**Files:**
- Create: `docs/src/pchronicle/design/refactor-cleanup-2026-08.md`
- Modify: `docs/mkdocs.yml`
- Modify: `benchmark/pchronicle/nightly.json`

**Interfaces:**
- Consumes: all completed tasks and their checked-in benchmark results.
- Produces: one auditable record of decisions, compatibility changes, and measured outcomes.

- [ ] **Step 1: Run formatting and targeted build checks**

```bash
cargo fmt --check -p persisting-agentctl -p persisting-events -p persisting-pchronicle -p persisting-pchronicle-cli -p persisting-pvisor -p persisting-ppilot
cargo check -p persisting-agentctl -p persisting-events -p persisting-pchronicle -p persisting-pchronicle-cli -p persisting-pvisor -p persisting-ppilot --locked
```

- [ ] **Step 2: Run the complete in-scope test matrix**

```bash
cargo test -p persisting-events --features control --locked
cargo test -p persisting-agentctl --lib --locked
cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib --locked
cargo test -p persisting-pchronicle --no-default-features --features lance-store --test production_scale --locked
cargo test -p persisting-pchronicle --no-default-features --features lance-store --test langfuse_backend_faults --locked
cargo test -p persisting-pchronicle-cli --lib --locked
cargo test -p persisting-pchronicle-cli --test control_process --locked
cargo test -p persisting-pchronicle-cli --test import_export_roundtrip --locked
cargo test -p persisting-pchronicle-cli --test server_http_contract --locked
cargo test -p persisting-pvisor --test chronicle_sidecar --locked
cargo test -p persisting-ppilot --lib sink_traj --locked
```

- [ ] **Step 3: Run the benchmark gates**

```bash
python3 benchmark/pchronicle/server_acceleration.py --release --output benchmark/pchronicle/server-acceleration-current.json
python3 benchmark/pchronicle/bench.py run --suite content-offload --output target/pchronicle-benchmark/content-offload --record-json benchmark/pchronicle/content-offload-current.json
python3 benchmark/pchronicle/bench.py run --suite nightly --output target/pchronicle-benchmark/nightly
python3 benchmark/pchronicle/bench.py jsonpath-set --document benchmark/pchronicle/nightly.json --path '$["latest"]' --value-file target/pchronicle-benchmark/nightly/raw-report.json --replace
```

Assert `nightly.json.latest` is non-null and references the current commit and
machine profile.

- [ ] **Step 4: Write the cleanup record**

Record:

- protocol and storage compatibility changes;
- manifest legacy handoff behavior and no-downgrade rule;
- exact test commands and pass counts;
- acceleration retain/remove decision and numbers;
- content Inline/Externalize decision and numbers;
- unknown-field migration behavior;
- revision RFC withdrawal and S3 feature change;
- remaining debt that did not pass a removal gate.

- [ ] **Step 5: Scan the plan for unfinished language and dead surfaces**

Run:

```bash
rg -n "[T]BD|[T]ODO|implement [l]ater|fill [i]n|write [t]ests for" docs/superpowers/plans/2026-08-23-pchronicle-refactor-cleanup.md
rg -n "write_revisions|read_revisions|RevisionRow|revision_dataset_path" crates -g '*.rs'
rg -n "RawEventLanceStore|append_events\(" crates/persisting-pchronicle-cli/src/control.rs crates/persisting-pchronicle-cli/src/gateway_capture.rs
rg -n "\bTrajectoryAppendRequest\b|\bappend_trajectory\b" crates -g '*.rs'
```

Expected: no placeholder, revision, direct service-path append, or V2 append
API matches.

- [ ] **Step 6: Commit the release record when authorized**

```bash
git add docs/src/pchronicle/design/refactor-cleanup-2026-08.md docs/mkdocs.yml benchmark/pchronicle/nightly.json
git commit -m "docs(pchronicle): record refactor and cleanup outcomes"
```

## Execution Notes

- Tasks 1–10 are serial and form the correctness gate.
- After Task 10 passes, Tasks 14, 15, and 16 may run in parallel reviewer
  worktrees. Tasks 11 and 12 both modify CLI argument wiring and run serially
  to avoid a gratuitous merge conflict.
- Task 12 must precede Task 13.
- Task 11 precedes Task 12 only because both touch
  `crates/persisting-pchronicle-cli/src/lib.rs`; there is no architectural
  dependency between their policies.
- Task 15 must precede Task 17 because the Vortex charter records RFC-0005 as
  withdrawn historical context.
- Task 14 changes only the content write policy; removing `objects.lance` would
  require a separate approved storage-format design.
- Task 18 runs only after every selected optional cleanup task is complete.

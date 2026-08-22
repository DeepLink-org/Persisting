# Storyline Writer Fencing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fence cross-process Storyline writers so every published `CURRENT` snapshot references one complete, internally consistent three-table write.

**Architecture:** Replace the direct `CURRENT` pointer with a backward-compatible CAS control envelope containing the committed pointer and an optional writer lease. Existing-generation mutations acquire and renew the lease; expired-lease takeover clones the pinned committed tables into a new physical generation before applying changes.

**Tech Stack:** Rust, Tokio, Lance, object_store conditional writes, shared-memory object-store tests.

**Spec:** `docs/superpowers/specs/2026-08-22-storyline-writer-fencing-design.md`

## Global Constraints

- Keep all writer-control types private to Storyline storage.
- Preserve legacy direct `CURRENT` pointer reads without offline migration.
- Keep normal replacement incremental; clone tables only after expired-lease takeover.
- Do not modify TTAS, Queue/Sampler, Search, or `persisting-dlcapt`.
- Do not include existing pChronicle Web changes in commits.

---

### Task 1: Control envelope and pure lease transitions

**Files:**
- Create: `crates/persisting-pchronicle/src/store/storyline/writer_control.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs`
- Test: `crates/persisting-pchronicle/src/store/storyline/writer_control.rs`

**Interfaces:**
- Consumes: existing private `StorylineSnapshotPointer` and `UpdateVersion`.
- Produces: `CurrentControlState`, `WriterLease`, `LeaseAcquireOutcome`, and validated pure transition functions used by the object-store CAS adapter.

- [ ] **Step 1: Write failing serialization and transition tests**

Add unit tests proving that a legacy pointer decodes as a committed control state, an unexpired foreign lease is held, an expired lease advances the epoch and marks takeover, and stale owner/epoch publication is rejected.

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p persisting-pchronicle --lib store::storyline::writer_control::tests --locked
```

Expected: compilation/test failure because `writer_control` and its transition API do not exist.

- [ ] **Step 3: Implement the minimal wire model and pure transitions**

Implement private types equivalent to:

```rust
pub(super) struct CurrentControlState {
    pub(super) control: StorylineCurrentControl,
    pub(super) version: Option<UpdateVersion>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct StorylineCurrentControl {
    pub(super) control_version: u32,
    pub(super) revision: u64,
    pub(super) committed: Option<StorylineSnapshotPointer>,
    pub(super) lease: Option<StorylineWriterLease>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct StorylineWriterLease {
    pub(super) epoch: u64,
    pub(super) owner_id: String,
    pub(super) issued_at_unix_ms: u64,
    pub(super) expires_at_unix_ms: u64,
    pub(super) base_generation: Option<String>,
}
```

Decode either the envelope or the legacy direct pointer, validate generation names and projection lineage through callbacks owned by `mod.rs`, and use checked revision/epoch increments.

- [ ] **Step 4: Re-run focused tests and verify GREEN**

Run the Task 1 command and require all writer-control tests to pass.

### Task 2: CAS control store and reader compatibility

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/storyline/writer_control.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs`
- Test: `crates/persisting-pchronicle/src/store/storyline/tests.rs`

**Interfaces:**
- Consumes: `ObjectStore`, `ObjectPath`, local atomic `CURRENT` writer, and Task 1 transitions.
- Produces: `read_control`, `try_acquire`, `renew`, `publish`, and `release` operations whose conditional updates all target the same `CURRENT` object.

- [ ] **Step 1: Add failing compatibility and fencing tests**

Tests must prove readers see the old committed snapshot while a lease is active, a legacy direct pointer upgrades on first mutation, a stale owner cannot publish, and stale release cannot clear the current owner.

- [ ] **Step 2: Verify the tests fail for missing control-object behavior**

Run the new test names individually with `cargo test -p persisting-pchronicle --lib ... --locked -- --exact`.

- [ ] **Step 3: Implement conditional control-object mutations**

For object stores, use `PutMode::Create` or `PutMode::Update(UpdateVersion)` and retry only CAS races that do not change semantic ownership. For local paths, reuse the existing process/file guard and atomic rename. Every publication must atomically install `committed`, clear only the matching lease, and advance revision.

- [ ] **Step 4: Verify focused compatibility and fencing tests pass**

Run the Task 2 tests plus existing malformed/missing `CURRENT` tests.

### Task 3: Fence normal existing-generation replacement

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/writer_control.rs`
- Test: `crates/persisting-pchronicle/src/store/storyline/tests.rs`

**Interfaces:**
- Consumes: Task 2 lease acquisition/publication and the existing replacement stream.
- Produces: a lease guard used by `StorylineStreamWriteMode::Replace` whenever `CURRENT` already has a committed snapshot.

- [ ] **Step 1: Make the concurrent replacement regression deterministically RED**

Retain the two independent store handles and barrier, but assert that the loser receives commit conflict before its rows enter any table version referenced by the winner.

- [ ] **Step 2: Run the regression and verify RED**

Run:

```bash
cargo test -p persisting-pchronicle --lib store::storyline::tests::independent_replacements_conflict_at_current_cas_and_retry_cleanly --locked -- --exact --nocapture
```

Expected: the pre-fix implementation either exposes the partial loser or allows both writers to mutate before final conflict.

- [ ] **Step 3: Acquire before mutation and publish through the lease**

For an existing committed snapshot, acquire the lease immediately after the process/file guard and before `next_storage_ordinal`, content commit, or any table merge. Active foreign ownership returns `Storyline commit conflict`. Successful final publication requires matching owner, epoch, and base generation.

- [ ] **Step 4: Add bounded renewal and owner-checked release**

Run renewal at a fixed fraction of the private lease TTL, stop it before publish/release, record lease loss, and never replace the primary operation error with a cleanup error.

- [ ] **Step 5: Verify normal replacement tests are GREEN**

Run the regression 30 times, then run all `store::storyline::tests`.

### Task 4: Isolated expired-lease takeover

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/storyline/mutation.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/writer_control.rs`
- Test: `crates/persisting-pchronicle/src/store/storyline/tests.rs`

**Interfaces:**
- Consumes: a takeover acquisition from Task 2 and exact pinned table versions.
- Produces: `clone_committed_generation` that streams pinned runs, steps, and tool-calls batches into a new generation before replacement.

- [ ] **Step 1: Add failing takeover tests**

Create an expired lease around a committed baseline, acquire with a new owner, and assert that replacement publishes a different `table_generation`, preserves unrelated Storylines, and rejects publication from the stale epoch.

- [ ] **Step 2: Verify takeover tests are RED**

Run only the new takeover tests and confirm failure is due to reuse of the old physical generation or missing lease takeover.

- [ ] **Step 3: Implement pinned-generation cloning**

Read all batches from the committed exact versions with no filter, create the three tables under one new generation, preserve storage ordinals and projection metadata, then apply incoming replacement batches to that isolated generation. Keep `objects.lance` content-addressed and pin the returned objects version containing all referenced content.

- [ ] **Step 4: Publish or clean up the isolated generation**

Publish only with the takeover owner/epoch. On failure, delete the unreferenced new generation only when it is known not to be committed; otherwise leave it for maintenance.

- [ ] **Step 5: Verify takeover and existing rebuild/create tests pass**

Run takeover tests, concurrent create tests, rebuild tests, and object-content tests.

### Task 5: Maintenance integration and verification

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs`
- Test: `crates/persisting-pchronicle/src/store/storyline/tests.rs`

**Interfaces:**
- Consumes: the Task 2 controller.
- Produces: lease-protected maintenance publication for existing generations.

- [ ] **Step 1: Add a failing maintenance-vs-writer fencing test**

Assert that maintenance cannot mutate/publish the same generation while another owner holds a live writer lease.

- [ ] **Step 2: Integrate maintenance with lease acquisition and publication**

Acquire before compaction/index mutation, publish the resulting versions through matching ownership, and release on error.

- [ ] **Step 3: Run formatting and targeted verification**

```bash
cargo fmt --all -- --check
cargo clippy -p persisting-pchronicle --lib --locked -- -D warnings -D clippy::unwrap_used -D clippy::expect_used -D clippy::unreachable
cargo test -p persisting-pchronicle --lib --locked
```

- [ ] **Step 4: Run boundary integration tests**

```bash
cargo test -p persisting-pchronicle --test storyline_lance_roundtrip --locked
cargo test -p persisting-pchronicle --test production_scale --locked
cargo test -p persisting-pchronicle-cli --lib --locked server::tests
```

- [ ] **Step 5: Review the final diff**

Confirm that only the design/plan and Storyline writer-control, mutation, and tests changed; verify existing pChronicle Web modifications remain untouched and uncommitted.

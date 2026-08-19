# Storyline Resource and Retention Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add opt-in Storyline import bounds, complete Storyline object/generation retention, and close the identified presence, concurrency, and documentation gaps.

**Architecture:** `StorylineContentOptions` remains the single store-level options object and gains optional hard import limits whose defaults preserve existing behavior. Storyline maintenance publishes one new four-dataset snapshot, then vacuums all four Lance datasets and removes only expired non-current physical generations. Tests exercise consumer-visible rollback, exact round trips, deterministic optimistic-CAS conflicts, and the real ignored S3 contract.

**Tech Stack:** Rust, Tokio, Lance 9, object_store, DataFusion, serde/serde_json, Cargo integration tests, mdBook Markdown.

**Spec:** `docs/superpowers/specs/2026-08-19-storyline-resource-retention-design.md`

## Global Constraints

- Existing APIs keep unlimited import behavior unless a new limit is explicitly `Some`.
- Duplicate `document_id` detection remains exact over full strings.
- `vacuum_older_than: None` performs neither Lance vacuum nor physical-generation deletion.
- The current `table_generation` is never deleted.
- No changes enter TTAS, Queue, Search, or standalone dlcapt.
- Every production behavior change follows a witnessed RED → GREEN cycle.
- Preserve unrelated worktree files and changes.

---

### Task 1: Opt-in import resource bounds

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/storyline/content.rs:40-75`
- Modify: `crates/persisting-pchronicle/src/store/storyline/mutation.rs:3-102`
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs:658-715`
- Test: `crates/persisting-pchronicle/src/store/storyline/tests.rs`

**Interfaces:**
- Consumes: `StorylineContentOptions`, `StorylineDocument`, `split_storyline`.
- Produces: five new `Option<usize>` fields on `StorylineContentOptions`; `StorylineChunkState` carrying one pending document; hard-limit validation used by every streamed replace/build/rebuild path.

- [ ] **Step 1: Add failing option-validation and rollback tests**

Add tests that name the breaks: zero limits being accepted, a too-large document moving `CURRENT`, a chunk exceeding its configured rows, and the global document-ID set exceeding its bound.

```rust
#[test]
fn storyline_import_limits_reject_zero() {
    let error = StorylineContentOptions {
        max_document_rows: Some(0),
        ..Default::default()
    }
    .validate()
    .unwrap_err();
    assert!(error.to_string().contains("max_document_rows must be positive"));
}

#[tokio::test]
async fn document_limit_failure_keeps_current_generation() {
    let dir = tempfile::tempdir().unwrap();
    let baseline = StorylineLanceStore::open(dir.path()).await.unwrap();
    baseline.replace_storyline(&story("baseline")).await.unwrap();
    let generation = baseline.current_table_paths().await.unwrap().unwrap().generation;
    let limited = StorylineLanceStore::open_with_content_options(
        dir.path(),
        StorylineContentOptions {
            max_document_rows: Some(2),
            ..Default::default()
        },
    ).await.unwrap();
    let error = limited.replace_storyline(&story("oversized")).await.unwrap_err();
    assert!(error.to_string().contains("max_document_rows"));
    assert_eq!(limited.current_table_paths().await.unwrap().unwrap().generation, generation);
}
```

For chunking, import three one-row documents with `max_chunk_rows: Some(2)` and assert all three are present; inspect the resulting table versions to prove more than one chunk was committed before publication. For `max_import_documents`, set it to one, stream two documents, and assert a limit error plus unchanged `CURRENT`.

- [ ] **Step 2: Run the new tests and verify RED**

Run:

```text
cargo test -p persisting-pchronicle --lib storyline_import_limits -- --nocapture
cargo test -p persisting-pchronicle --lib document_limit_failure_keeps_current_generation -- --nocapture
```

Expected: compilation fails because the five option fields and limit state do not exist.

- [ ] **Step 3: Add validated optional fields and a non-allocating byte counter**

Extend `StorylineContentOptions`:

```rust
pub max_document_rows: Option<usize>,
pub max_document_bytes: Option<usize>,
pub max_chunk_rows: Option<usize>,
pub max_chunk_bytes: Option<usize>,
pub max_import_documents: Option<usize>,
```

Default all five to `None`; reject zero with the field name in the error. Add a private `CountingWriter(usize)` implementing `std::io::Write`, and compute document bytes with `serde_json::to_writer(&mut counter, story)`.

- [ ] **Step 4: Implement hard document/chunk/import checks**

Introduce:

```rust
pub(super) struct StorylineChunkState {
    pub(super) all_document_ids: HashSet<String>,
    pub(super) pending: Option<StorylineDocument>,
}
```

Change `next_storyline_stream_chunk` to accept `&mut StorylineChunkState` and `StorylineContentOptions`. For each document compute `document_rows = 1 + steps.len() + tool_calls.len()` and the counted JSON bytes. Reject document and chunk self-overflow, save a document in `pending` when only the accumulated chunk would overflow, and check exact duplicate IDs before enforcing `max_import_documents`.

- [ ] **Step 5: Run Task 1 tests and the existing stream tests**

Run:

```text
cargo test -p persisting-pchronicle --lib storyline_import_limits -- --nocapture
cargo test -p persisting-pchronicle --lib document_limit_failure_keeps_current_generation -- --nocapture
cargo test -p persisting-pchronicle --lib streamed_replace -- --nocapture
```

Expected: all selected tests pass.

- [ ] **Step 6: Commit Task 1**

```text
git add crates/persisting-pchronicle/src/store/storyline/content.rs crates/persisting-pchronicle/src/store/storyline/mutation.rs crates/persisting-pchronicle/src/store/storyline/mod.rs crates/persisting-pchronicle/src/store/storyline/tests.rs
git commit -m "feat: bound storyline streaming imports"
```

---

### Task 2: ATIF presence through Lance

**Files:**
- Test: `crates/persisting-pchronicle/tests/atif_lance_corpus.rs`

**Interfaces:**
- Consumes: public ATIF decode/encode functions and `StorylineLanceStore`.
- Produces: one end-to-end regression test; no production API.

- [ ] **Step 1: Write the Lance presence round-trip test**

Add a literal ATIF value with root/agent/step nulls and three tool results:

```rust
#[tokio::test]
async fn atif_null_and_tool_result_presence_round_trip_through_lance() -> Result<()> {
    let input = serde_json::json!({
        "schema_version": "ATIF-v1.7",
        "trajectory_id": "presence-trajectory",
        "agent": {"name":"agent-1","version":"1","model_name":null,"extra":null},
        "steps": [{
            "step_id": 1,
            "timestamp": null,
            "source": "agent",
            "message": "done",
            "tool_calls": [
                {"tool_call_id":"missing","function_name":"a","arguments":{}},
                {"tool_call_id":"null","function_name":"b","arguments":{},"result":null},
                {"tool_call_id":"value","function_name":"c","arguments":{},"result":{"ok":true}}
            ],
            "observation": null,
            "metrics": null,
            "extra": null
        }],
        "notes": null,
        "final_metrics": null,
        "extra": null
    });
    let story = into_storyline(TestFormat::Atif, &input.to_string())?;
    let dir = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(dir.path()).await?;
    store.replace_storyline(&story).await?;
    let restored = store.get_storyline_full(&story.session_id).await?.unwrap();
    let output: serde_json::Value = serde_json::from_str(&from_storyline(TestFormat::Atif, &restored)?)?;
    assert_eq!(output, input);
    Ok(())
}
```

- [ ] **Step 2: Verify the test against a deliberately broken decoder**

Temporarily change the run-row decoder to always use `StorylinePresence::default()`, run:

```text
cargo test -p persisting-pchronicle --test atif_lance_corpus atif_null_and_tool_result_presence_round_trip_through_lance -- --nocapture
```

Expected: FAIL because explicit null fields disappear. Restore the decoder immediately.

- [ ] **Step 3: Run the restored implementation**

Run the same command. Expected: PASS with the current correct codec.

- [ ] **Step 4: Commit Task 2**

```text
git add crates/persisting-pchronicle/tests/atif_lance_corpus.rs
git commit -m "test: preserve ATIF presence through Lance"
```

---

### Task 3: Vacuum objects.lance and report its reclamation

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs:234-241,919-999`
- Test: `crates/persisting-pchronicle/src/store/storyline/tests.rs:357-397`

**Interfaces:**
- Consumes: `vacuum_table`, `merge_maintenance_reports`, `LanceMaintenanceOptions`.
- Produces: `StorylineMaintenanceReport::objects: LanceMaintenanceReport` while retaining `objects_removed`.

- [ ] **Step 1: Write a failing objects-vacuum test**

Extend the existing unreachable-object maintenance setup, run maintenance with zero retention, and assert physical object cleanup is reported:

```rust
let report = store.maintain(&LanceMaintenanceOptions {
    compact: false,
    optimize_indices: false,
    vacuum_older_than: Some(std::time::Duration::ZERO),
    ..Default::default()
}).await.unwrap();
assert_eq!(report.objects_removed, 1);
assert!(report.objects.old_versions_removed > 0);
assert!(report.objects.bytes_removed > 0);
```

- [ ] **Step 2: Run and verify RED**

Run:

```text
cargo test -p persisting-pchronicle --lib maintenance_vacuums_unreferenced_objects -- --nocapture
```

Expected: compilation fails because `StorylineMaintenanceReport::objects` does not exist.

- [ ] **Step 3: Vacuum objects after CURRENT publication**

Add `objects: LanceMaintenanceReport` to the report. Extend the post-publication `tokio::try_join!` with `vacuum_table(&paths.objects, options.vacuum_older_than)`, and place its result in the report. Do not vacuum before `commit_snapshot`.

- [ ] **Step 4: Run objects maintenance tests**

Run:

```text
cargo test -p persisting-pchronicle --lib maintenance_vacuums_unreferenced_objects -- --nocapture
cargo test -p persisting-pchronicle --lib maintenance_prunes_objects_unreachable_from_current_snapshot -- --nocapture
```

Expected: both pass.

- [ ] **Step 5: Commit Task 3**

```text
git add crates/persisting-pchronicle/src/store/storyline/mod.rs crates/persisting-pchronicle/src/store/storyline/tests.rs
git commit -m "fix: vacuum storyline content objects"
```

---

### Task 4: Retain and prune physical generations

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs:234-241,987-999,1280-1310`
- Test: `crates/persisting-pchronicle/src/store/storyline/tests.rs`

**Interfaces:**
- Consumes: `object_store`, `object_root`, `GENERATIONS_DIR`, generation-name timestamps, `vacuum_older_than`.
- Produces: `prune_expired_generations(&self, current: &str, retention: Option<Duration>) -> Result<usize>` and `StorylineMaintenanceReport::generations_removed`.

- [ ] **Step 1: Write local generation-retention tests**

Create a store, perform two projection rebuilds so two physical generation directories exist, and verify:

```rust
let no_vacuum = store.maintain(&LanceMaintenanceOptions {
    vacuum_older_than: None,
    ..Default::default()
}).await.unwrap();
assert_eq!(no_vacuum.generations_removed, 0);

let vacuumed = store.maintain(&LanceMaintenanceOptions {
    vacuum_older_than: Some(std::time::Duration::ZERO),
    ..Default::default()
}).await.unwrap();
assert_eq!(vacuumed.generations_removed, 1);
let current = store.current_table_paths().await.unwrap().unwrap();
assert!(current.runs.is_dir());
```

Also create `generations/not-owned-by-storyline` and assert it is preserved.

- [ ] **Step 2: Run and verify RED**

Run:

```text
cargo test -p persisting-pchronicle --lib maintenance_prunes_expired_physical_generations -- --nocapture
```

Expected: compilation fails because `generations_removed` does not exist.

- [ ] **Step 3: Implement strict generation timestamp parsing and pruning**

Add a parser that accepts only `gen-<u128>-<u32>-<u64>`. List direct children beneath `generations`, skip the current generation and malformed names, compare parsed nanoseconds with `SystemTime::now() - retention`, and call `remove_dir_all` only for expired candidates. Return zero without listing when retention is `None`.

- [ ] **Step 4: Invoke pruning after all four Lance vacuums**

Call `prune_expired_generations(&paths.table_generation, options.vacuum_older_than)` after the four-dataset vacuum join and store the count in the report.

- [ ] **Step 5: Run generation and full Storyline maintenance tests**

Run:

```text
cargo test -p persisting-pchronicle --lib maintenance_prunes_expired_physical_generations -- --nocapture
cargo test -p persisting-pchronicle --lib store::storyline::tests -- --nocapture
```

Expected: all selected tests pass.

- [ ] **Step 6: Commit Task 4**

```text
git add crates/persisting-pchronicle/src/store/storyline/mod.rs crates/persisting-pchronicle/src/store/storyline/tests.rs
git commit -m "fix: retain storyline physical generations"
```

---

### Task 5: Exercise non-empty optimistic-CAS replacements

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs:270-340,658-690`
- Test: `crates/persisting-pchronicle/src/store/storyline/tests.rs:846-877`
- Test: `crates/persisting-pchronicle/tests/s3_storage.rs`

**Interfaces:**
- Consumes: existing `cfg(test)` barrier pattern, `StorylineLanceStore::replace_storyline`, `PCHRONICLE_S3_TEST_URI`.
- Produces: deterministic `ReplaceAfterCurrentReadBarrier` unit-test hook and ignored S3 multiprocess replacement contract.

- [ ] **Step 1: Write the deterministic failing replacement test**

Install a two-party barrier for a non-empty shared-memory root, replace each store's `write_lock` with an independent mutex, and start two replacements from the same baseline. Assert exactly one returns `Ok`, the loser error contains `commit conflict`, and a retry publishes the loser without removing baseline or winner.

```rust
let successes = [&left_result, &right_result]
    .into_iter().filter(|result| result.is_ok()).count();
assert_eq!(successes, 1);
let failure = [&left_result, &right_result]
    .into_iter().find_map(|result| result.as_ref().err()).unwrap();
assert!(failure.to_string().contains("commit conflict"));
```

- [ ] **Step 2: Run and verify RED**

Run:

```text
cargo test -p persisting-pchronicle --lib independent_object_store_replacements_publish_one_snapshot -- --nocapture
```

Expected: test cannot deterministically force both writers past the same `CURRENT` read because the replacement barrier does not exist.

- [ ] **Step 3: Add the test-only replacement barrier**

Mirror the existing create barrier with a `root_uri` and Tokio barrier. Invoke it immediately after `expected_generation` is captured for `StorylineStreamWriteMode::Replace`, before content/table mutations. Do not compile the hook outside `cfg(test)`.

- [ ] **Step 4: Run the deterministic test until GREEN**

Run the Task 5 unit-test command. Expected: PASS with one publication, one conflict, and a successful explicit retry.

- [ ] **Step 5: Add an ignored real S3 multiprocess contract**

Add a parent ignored test that writes a baseline under a unique prefix and spawns the current integration-test executable with an environment-selected worker test. Each worker waits on a localhost TCP release before calling `replace_storyline`; the worker exits 0 for success or a recognized `commit conflict`, and writes its outcome to a unique S3 marker. The parent asserts all worker outcomes are recognized, reopens `CURRENT`, verifies the baseline, and verifies every success marker's session.

- [ ] **Step 6: Compile the S3 test without running credentials-required cases**

Run:

```text
cargo test -p persisting-pchronicle --test s3_storage --no-run
cargo test -p persisting-pchronicle --test s3_storage -- --list
```

Expected: exit 0 and the new parent/worker test names appear.

- [ ] **Step 7: Commit Task 5**

```text
git add crates/persisting-pchronicle/src/store/storyline/mod.rs crates/persisting-pchronicle/src/store/storyline/tests.rs crates/persisting-pchronicle/tests/s3_storage.rs
git commit -m "test: cover storyline object-store replacement CAS"
```

---

### Task 6: Correct Storyline storage documentation

**Files:**
- Modify: `docs/src/pchronicle/design/storyline-lance.md`
- Modify: `docs/src/pchronicle/design/storyline-lance.zh.md`
- Modify: `docs/src/rfcs/0001-storyline-format.md`

**Interfaces:**
- Consumes: actual merge keys, `RUN_INDEXES`/`STEP_INDEXES`/`TOOL_CALL_INDEXES`, canonical manifest invariants.
- Produces: documentation matching the implementation; no code interface.

- [ ] **Step 1: Correct replacement scope and index tables**

In both Storyline Lance documents, replace `session_id` as the mutation/delete scope with `document_id`. Update the index table to the literal implementation:

```text
runs: BTree(document_id, session_id, run_id)
steps: BTree(document_id, session_id, timestamp), Bitmap(effective_kind, source)
tool_calls: BTree(document_id, session_id, tool_call_id), Bitmap(function_name)
```

- [ ] **Step 2: Document the watermark proof obligations**

Add the same explanation to both language variants: the append range is valid only because manifest validation enforces `fact_rows == total_rows()`, maintenance preserves replacement row count and segment order, and the range reader verifies the exact returned length.

- [ ] **Step 3: Correct RFC-0001 identity mappings**

Change the root table and extraction example to:

```text
trajectory <- $.trajectory_id
run <- no ATIF source; reserved for Storyline run_id
session <- $.session_id
```

Keep the already-correct comparison table entry `trajectory_id | trajectory`.

- [ ] **Step 4: Check the documentation diff and commit**

Run:

```text
git diff --check -- docs/src/pchronicle/design/storyline-lance.md docs/src/pchronicle/design/storyline-lance.zh.md docs/src/rfcs/0001-storyline-format.md
```

Expected: exit 0 with no whitespace errors.

```text
git add docs/src/pchronicle/design/storyline-lance.md docs/src/pchronicle/design/storyline-lance.zh.md docs/src/rfcs/0001-storyline-format.md
git commit -m "docs: align Storyline storage contracts"
```

---

### Task 7: Final verification

**Files:**
- Verify only; modify only if a preceding task left a test or lint failure attributable to this work.

**Interfaces:**
- Consumes: all Task 1-6 changes.
- Produces: fresh evidence for completion.

- [ ] **Step 1: Format the changed Rust files**

Run:

```text
cargo fmt --all
cargo fmt --all -- --check
```

Expected: formatter check exits 0.

- [ ] **Step 2: Run the Storyline unit and integration tests**

Run:

```text
cargo test -p persisting-pchronicle --lib store::storyline::tests -- --nocapture
cargo test -p persisting-pchronicle --test atif_lance_corpus -- --nocapture
cargo test -p persisting-pchronicle --test s3_storage -- --nocapture
```

Expected: all non-ignored tests pass; credentials-required S3 cases remain ignored.

- [ ] **Step 3: Run targeted Clippy**

Run:

```text
cargo clippy -p persisting-pchronicle --all-targets -- -D warnings
```

Expected: exit 0 with no warnings attributable to pChronicle targets.

- [ ] **Step 4: Audit scope and diff**

Run:

```text
git status --short
git diff --check 4a52403d..HEAD
git diff --stat 4a52403d..HEAD
```

Expected: only the approved Storyline code/tests/docs plus the design and plan commits are present; unrelated untracked files remain unmodified and uncommitted.

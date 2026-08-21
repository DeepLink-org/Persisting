# pChronicle Automatic Storyline Projection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the public `pchronicle project` command and make canonical `events.lance` projection a create-only `import` mode plus an automatic, continuously maintained `serve` responsibility reported by `status`.

**Architecture:** Add a library-owned automatic-projection inventory and convergence layer on top of the existing pinned canonical-event and generation/CAS Storyline primitives. The CLI uses that layer at three boundaries: `import` performs one explicit create-only projection, `status` performs read-only inspection, and `serve` converges before readiness then runs a bounded per-source retry supervisor. Warehouse serving receives a prepared shared Catalog handle so successful projection publication or changed canonical facts can build a complete replacement Catalog runtime and atomically swap it without interrupting the old snapshot.

**Tech Stack:** Rust 2021 workspace, Tokio, Clap, Futures, Lance, DataFusion, Axum, object_store, Serde, existing pChronicle Catalog and Storyline generation/CAS storage.

**Spec:** `docs/superpowers/specs/2026-08-21-pchronicle-automatic-projection-design.md`

## Global Constraints

- `events.lance` remains the source of truth; `storyline` is rebuildable derived state.
- Do not put projection work on the Gateway or Control append acknowledgement path.
- Do not change canonical event ordering, append acknowledgement, fencing, or physical storage semantics.
- Automatic destinations are deterministic siblings: `run/events.lance` maps to `run/storyline` for local and object-store URIs.
- Never overwrite an existing destination whose committed lineage does not identify the matching canonical source.
- Initial projection failure prevents the single `serve` readiness record; runtime projection failure does not stop Warehouse, Control, or Gateway.
- Runtime work is bounded, uses capped per-source backoff, and coalesces Catalog refreshes.
- A failed Catalog rebuild retains the previously installed Catalog runtime.
- `serve` without `--listen` does not construct an unused Warehouse Catalog runtime.
- `status` is observational and never creates, syncs, rebuilds, or publishes projection state.
- Remove `project` without an alias while retaining the underlying Rust projection operations for internal callers and tests.
- Keep TTAS, Queue, Search, and `persisting-dlcapt` out of scope.

---

## File map

- Create `crates/persisting-pchronicle/src/projection/automatic.rs`: deterministic target derivation, Catalog inventory, read-only health inspection, and one-source convergence.
- Modify `crates/persisting-pchronicle/src/projection/mod.rs`: expose the automatic-projection types and functions.
- Modify `crates/persisting-pchronicle/src/store/events/datafusion.rs`: add manifest-only canonical-store probing without opening every Lance segment.
- Modify `crates/persisting-pchronicle/src/store/catalog/mod.rs`: expose an internal pinned canonical-source view used to build snapshot-consistent inventories.
- Modify `crates/persisting-pchronicle/src/store/storyline/mod.rs`: add a read-only destination-existence check for create-only import.
- Modify `crates/persisting-pchronicle/src/storage.rs`: re-export the new library boundary.
- Modify `crates/persisting-pchronicle/src/projection/storyline.rs`: make the minimum lineage helpers visible to the sibling automatic module and remove CLI-specific error wording.
- Modify `crates/persisting-pchronicle-cli/src/exchange.rs`: split JSON import from canonical-event projection import.
- Modify `crates/persisting-pchronicle-cli/src/lib.rs`: make `--output-format` contextual, remove `project`, add projection status records, and wire the supervisor into `serve`.
- Modify `crates/persisting-pchronicle-cli/src/output.rs`: add the compact projection summary to table status output.
- Create `crates/persisting-pchronicle-cli/src/projection_supervisor.rs`: startup convergence, runtime discovery, per-source retry state, shutdown, and Catalog refresh coalescing.
- Modify `crates/persisting-pchronicle-cli/src/server/mod.rs`: introduce a prepared Warehouse handle with atomic Catalog replacement.
- Modify `crates/persisting-pchronicle-cli/src/tests.rs`: parser, import, status, and in-process supervisor coverage.
- Modify `crates/persisting-pchronicle-cli/src/server/tests.rs`: prepared Catalog and failed-refresh retention coverage.
- Modify `crates/persisting-pchronicle-cli/tests/control_process.rs`: readiness, runtime discovery, durable-write independence, and Warehouse refresh process coverage.
- Modify `crates/persisting-pchronicle-cli/tests/binary_contract.rs`: absence of `project` and release-profile canonical import smoke coverage.
- Modify pChronicle READMEs and the English/Chinese CLI, exchange, serve, Storyline, and Catalog documentation listed in Task 8.

### Task 1: Canonical-store probing and deterministic projection inventory

**Files:**
- Create: `crates/persisting-pchronicle/src/projection/automatic.rs`
- Modify: `crates/persisting-pchronicle/src/projection/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/events/datafusion.rs`
- Modify: `crates/persisting-pchronicle/src/store/catalog/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs`
- Modify: `crates/persisting-pchronicle/src/storage.rs`

**Interfaces:**
- Consumes: `DatasetCatalogSnapshot`, `DatasetMount`, `DiscoveredSource`, `CatalogSourceRevision::Events`, `RawEventDataSource`, `StorylineLanceStore`, and `EventFactSnapshot`.
- Produces:

```rust
pub async fn probe_canonical_event_store(
    uri: impl AsRef<str>,
) -> anyhow::Result<Option<EventFactSnapshot>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticProjectionTarget {
    pub dataset: String,
    pub source_path: String,
    pub source_uri: String,
    pub projection_path: String,
    pub projection_uri: String,
    pub source_snapshot: EventFactSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticProjectionInventoryError {
    pub dataset: String,
    pub source_path: String,
    pub projection_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticProjectionInventory {
    pub snapshot_id: String,
    pub targets: Vec<AutomaticProjectionTarget>,
    pub errors: Vec<AutomaticProjectionInventoryError>,
}

pub fn automatic_projection_inventory(
    snapshot: &DatasetCatalogSnapshot,
) -> anyhow::Result<AutomaticProjectionInventory>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticProjectionState {
    Fresh,
    Stale,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticProjectionInspection {
    pub state: AutomaticProjectionState,
    pub generation: Option<String>,
    pub fact_version: u64,
    pub fact_rows: u64,
}

pub async fn inspect_automatic_storyline_projection(
    target: &AutomaticProjectionTarget,
) -> anyhow::Result<AutomaticProjectionInspection>;

pub async fn storyline_projection_destination_exists(
    uri: impl AsRef<str>,
) -> anyhow::Result<bool>;
```

- `DatasetCatalogSnapshot` adds a crate-visible `canonical_event_sources()` accessor returning the exact pinned `source_uri`, source path, Dataset name, and `EventFactSnapshot`; it does not expose mutable Catalog internals.
- `RawEventDataSource` adds `pub async fn probe_uri(uri: impl AsRef<str>) -> Result<Option<EventFactSnapshot>>`; the free `probe_canonical_event_store` wrapper lives in `projection::automatic` so CLI callers need only the projection boundary.

- [ ] **Step 1: Write failing manifest-probe and URI-mapping tests**

Add unit tests that distinguish a real manifest from a suffix and cover local, nested, direct-root, and object-store names:

```rust
#[tokio::test]
async fn canonical_probe_requires_a_valid_nonempty_manifest() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let suffix_only = temp.path().join("events.lance");
    std::fs::create_dir(&suffix_only)?;
    assert_eq!(probe_canonical_event_store(suffix_only.to_string_lossy()).await?, None);

    let storage = temp.path().join("capture");
    append_note(&storage, "session", 0).await?;
    let source = raw_event_lance_path(&coords(&storage, "session"))?;
    let snapshot = probe_canonical_event_store(source.to_string_lossy())
        .await?
        .expect("written canonical store must be detected");
    assert_eq!(snapshot.fact_rows, 1);
    Ok(())
}

#[test]
fn projection_target_is_a_sibling_for_local_and_object_uris() -> Result<()> {
    assert_eq!(
        automatic_projection_uri("/tmp/run/events.lance")?,
        "/tmp/run/storyline"
    );
    assert_eq!(
        automatic_projection_uri("s3://bucket/jobs/7/events.lance")?,
        "s3://bucket/jobs/7/storyline"
    );
    assert!(automatic_projection_uri("/tmp/run/not-events").is_err());
    Ok(())
}
```

- [ ] **Step 2: Run the focused tests and verify failure**

Run:

```bash
cargo test -p persisting-pchronicle --lib projection::automatic::tests::canonical_probe_requires_a_valid_nonempty_manifest
cargo test -p persisting-pchronicle --lib projection::automatic::tests::projection_target_is_a_sibling_for_local_and_object_uris
```

Expected: compilation fails because `automatic` and `probe_canonical_event_store` do not exist.

- [ ] **Step 3: Implement manifest-only probing and deterministic URI mapping**

Normalize existing local inputs with `std::fs::canonicalize`, retain object-store URIs, call the existing validated manifest reader, and return `None` only when no manifest exists. A malformed manifest or a manifest without visible segments is an error:

```rust
pub async fn probe_uri(
    uri: impl AsRef<str>,
) -> Result<Option<EventFactSnapshot>> {
    let requested = uri.as_ref();
    let normalized = if requested.contains("://") {
        requested.trim_end_matches('/').to_owned()
    } else {
        match std::fs::canonicalize(requested) {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("canonicalize canonical event probe"),
        }
    };
    let Some(manifest) = super::pin_visible_snapshot(&normalized).await? else {
        return Ok(None);
    };
    anyhow::ensure!(
        !manifest.segments.is_empty(),
        "canonical event manifest has no visible segments at {normalized}"
    );
    Ok(Some(EventFactSnapshot {
        source_uri: normalized,
        fact_version: manifest.fact_version,
        fact_rows: manifest.fact_rows,
        layout_revision: manifest.revision,
    }))
}

pub async fn probe_canonical_event_store(
    uri: impl AsRef<str>,
) -> Result<Option<EventFactSnapshot>> {
    RawEventDataSource::probe_uri(uri).await
}
```

For object URIs, remove exactly the terminal `/events.lance` segment and append `/storyline`. For local paths, require a terminal `events.lance` component and use `Path::parent().join("storyline")`.

- [ ] **Step 4: Write failing inventory and observational-inspection tests**

Build a Dataset containing two nested canonical stores, one fresh sidecar, one absent sidecar, and a separate malformed Storyline pointer. Assert stable sorting and that inspection creates no directory:

```rust
let inventory = automatic_projection_inventory(&snapshot)?;
assert_eq!(
    inventory.targets.iter().map(|target| target.source_path.as_str()).collect::<Vec<_>>(),
    ["a/events.lance", "b/events.lance"]
);
assert_eq!(inventory.targets[0].projection_path, "a/storyline");
assert_eq!(inventory.targets[1].projection_path, "b/storyline");

let missing = inspect_automatic_storyline_projection(&inventory.targets[1]).await?;
assert_eq!(missing.state, AutomaticProjectionState::Missing);
assert!(!temp.path().join("b/storyline").exists());
```

Also assert that `storyline_projection_destination_exists` returns true for an existing empty local directory and for an object-store prefix containing a sentinel object, while read-only inspection does not create either destination.

- [ ] **Step 5: Run inventory tests and verify failure**

Run:

```bash
cargo test -p persisting-pchronicle --lib projection::automatic::tests::inventory_is_sorted_and_uses_pinned_event_snapshots
cargo test -p persisting-pchronicle --lib projection::automatic::tests::missing_inspection_is_observational
cargo test -p persisting-pchronicle --lib projection::automatic::tests::destination_existence_covers_local_and_object_stores
```

Expected: compilation fails because the inventory, inspection, and existence APIs are absent.

- [ ] **Step 6: Implement the inventory and read-only inspection**

Have the Catalog accessor obtain exact URIs from `LazySourceSpec::Events`, and build error records from canonical-event `DiscoveredSource` rows whose status is `Error`. Direct-root mounts display `events.lance`/`storyline`, not `.`. Inspection reads `CURRENT` and compares its lineage to `target.source_snapshot`; it returns an error for lineage-free, foreign-source, or malformed destinations.

The Catalog accessor walks `self.prepared` in Dataset order and retains only event specs:

```rust
pub(crate) fn canonical_event_sources(&self) -> Vec<CatalogCanonicalEventSource> {
    self.prepared
        .iter()
        .flat_map(|dataset| {
            dataset.sources.iter().filter_map(|source| match &source.spec {
                LazySourceSpec::Events { uri, snapshot, .. } => {
                    Some(CatalogCanonicalEventSource {
                        dataset: dataset.name.clone(),
                        source_path: source.file.clone(),
                        source_uri: uri.clone(),
                        snapshot: snapshot.fact_snapshot(),
                    })
                }
                _ => None,
            })
        })
        .collect()
}
```

```rust
match storyline_projection_status(&target.projection_uri).await? {
    status if status.generation.is_none() => Ok(inspection(target, Missing, None)),
    status => {
        let lineage = status.lineage.as_ref().context(
            "automatic Storyline destination has no canonical lineage",
        )?;
        ensure_matching_source(&target.source_snapshot, lineage)?;
        let state = if projection_lineage_is_fresh(&target.source_snapshot, lineage) {
            AutomaticProjectionState::Fresh
        } else {
            AutomaticProjectionState::Stale
        };
        Ok(inspection(target, state, status.generation))
    }
}
```

Implement create-only existence without writing a lock file or directory:

```rust
pub async fn destination_exists(root: impl AsRef<str>) -> Result<bool> {
    let store = Self::open_uri_unchecked(root).await?;
    if matches!(store.storage_scheme(), "file" | "file+uring") {
        return Ok(store.root.exists());
    }
    let mut objects = store.object_store.inner.list(Some(&store.object_root));
    objects
        .try_next()
        .await
        .context("inspect Storyline destination prefix")
        .map(|object| object.is_some())
}

pub async fn storyline_projection_destination_exists(
    uri: impl AsRef<str>,
) -> Result<bool> {
    StorylineLanceStore::destination_exists(uri).await
}
```

- [ ] **Step 7: Run the focused library tests**

Run:

```bash
cargo test -p persisting-pchronicle --lib projection::automatic
cargo test -p persisting-pchronicle --lib store::catalog::tests
```

Expected: all tests pass.

- [ ] **Step 8: Commit the inventory boundary**

```bash
git add crates/persisting-pchronicle/src/projection/automatic.rs \
  crates/persisting-pchronicle/src/projection/mod.rs \
  crates/persisting-pchronicle/src/store/events/datafusion.rs \
  crates/persisting-pchronicle/src/store/catalog/mod.rs \
  crates/persisting-pchronicle/src/store/storyline/mod.rs \
  crates/persisting-pchronicle/src/storage.rs
git commit -m "feat(pchronicle): inventory automatic Storyline projections"
```

### Task 2: Safe one-source automatic convergence

**Files:**
- Modify: `crates/persisting-pchronicle/src/projection/automatic.rs`
- Modify: `crates/persisting-pchronicle/src/projection/storyline.rs`
- Modify: `crates/persisting-pchronicle/src/projection/mod.rs`
- Modify: `crates/persisting-pchronicle/src/storage.rs`

**Interfaces:**
- Consumes: `AutomaticProjectionTarget`, `build_storyline_projection`, `sync_storyline_projection`, `rebuild_storyline_projection`, `verify_storyline_projection`, and existing Storyline `CURRENT` CAS publication.
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomaticProjectionMaintenanceMode {
    Unchanged,
    Built,
    Incremental,
    Rebuilt,
    ConcurrentWinner,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticProjectionMaintenanceReport {
    pub mode: AutomaticProjectionMaintenanceMode,
    pub generation: String,
    pub fact_version: u64,
    pub fact_rows: u64,
    pub trajectories: Option<usize>,
}

impl AutomaticProjectionMaintenanceReport {
    pub fn published(&self) -> bool;
}

pub async fn maintain_automatic_storyline_projection(
    target: &AutomaticProjectionTarget,
) -> anyhow::Result<AutomaticProjectionMaintenanceReport>;
```

- [ ] **Step 1: Write failing convergence state-machine tests**

Cover missing build, fresh no-op, append-only incremental sync, matching obsolete recipe rebuild, non-monotonic watermark rebuild, and foreign/no-lineage refusal:

```rust
let built = maintain_automatic_storyline_projection(&target).await?;
assert_eq!(built.mode, AutomaticProjectionMaintenanceMode::Built);
assert!(built.published());

let unchanged = maintain_automatic_storyline_projection(&target).await?;
assert_eq!(unchanged.mode, AutomaticProjectionMaintenanceMode::Unchanged);
assert!(!unchanged.published());

append_note(&storage, "session", 1).await?;
let incremental = maintain_automatic_storyline_projection(&rediscovered_target).await?;
assert_eq!(incremental.mode, AutomaticProjectionMaintenanceMode::Incremental);
assert_eq!(incremental.fact_rows, 2);

let before = std::fs::read(projection.join("CURRENT"))?;
let error = maintain_automatic_storyline_projection(&foreign_target)
    .await
    .unwrap_err();
assert!(error.to_string().contains("matching canonical source"));
assert_eq!(std::fs::read(projection.join("CURRENT"))?, before);
```

- [ ] **Step 2: Run state-machine tests and verify failure**

Run:

```bash
cargo test -p persisting-pchronicle --lib projection::automatic::tests::maintenance_builds_syncs_and_noops
cargo test -p persisting-pchronicle --lib projection::automatic::tests::maintenance_rebuilds_only_owned_outputs
```

Expected: compilation fails because `maintain_automatic_storyline_projection` is absent.

- [ ] **Step 3: Implement ownership-first convergence**

Use the following decision order:

```rust
match inspect_automatic_storyline_projection(target).await {
    Ok(inspection) if inspection.state == AutomaticProjectionState::Missing => {
        build_or_accept_concurrent_winner(target).await
    }
    Ok(inspection) if inspection.state == AutomaticProjectionState::Fresh => {
        Ok(report_from_inspection(Unchanged, inspection))
    }
    Ok(_) => match sync_storyline_projection(
        &target.source_uri,
        &target.projection_uri,
    ).await? {
        StorylineProjectionSyncOutcome::Synced(report) => map_sync_report(report),
        StorylineProjectionSyncOutcome::MissingProjection => {
            build_or_accept_concurrent_winner(target).await
        }
        StorylineProjectionSyncOutcome::RequiresRebuild(_) => {
            ensure_current_lineage_owns_target(target).await?;
            map_rebuild_report(
                rebuild_storyline_projection(
                    &target.source_uri,
                    &target.projection_uri,
                    &target.source_path,
                ).await?
            )
        }
    },
    Err(error) => Err(error),
}
```

Before rebuild, require the canonical source URI/source ID to match. Missing lineage and foreign lineage remain conflicts. If build/sync/rebuild loses a publication race, re-run verification: a fresh matching winner maps to `ConcurrentWinner`; any other state returns the original conflict.

Change the internal sync diagnostic from “use `project rebuild`” to “projection requires a complete rebuild” so the removed CLI is never suggested.

- [ ] **Step 4: Write and run a concurrent-winner test**

```rust
let (left, right) = tokio::join!(
    maintain_automatic_storyline_projection(&target),
    maintain_automatic_storyline_projection(&target),
);
let reports = [left?, right?];
assert!(reports.iter().all(|report| matches!(
    report.mode,
    AutomaticProjectionMaintenanceMode::Built
        | AutomaticProjectionMaintenanceMode::ConcurrentWinner
)));
assert_eq!(inspect_automatic_storyline_projection(&target).await?.state, Fresh);
```

Run:

```bash
cargo test -p persisting-pchronicle --lib projection::automatic::tests::concurrent_maintenance_accepts_one_fresh_winner
```

Expected: pass, with exactly one committed fresh generation and no in-place mutation.

- [ ] **Step 5: Run the complete projection test group**

Run:

```bash
cargo test -p persisting-pchronicle --lib projection::
cargo test -p persisting-pchronicle --features s3-store --test s3_storage projection
```

Expected: all selected tests pass. If the environment has no S3 test configuration, the existing S3 tests must skip through their current harness rather than becoming acceptance blockers.

- [ ] **Step 6: Commit convergence**

```bash
git add crates/persisting-pchronicle/src/projection/automatic.rs \
  crates/persisting-pchronicle/src/projection/storyline.rs \
  crates/persisting-pchronicle/src/projection/mod.rs \
  crates/persisting-pchronicle/src/storage.rs
git commit -m "feat(pchronicle): converge owned Storyline projections"
```

### Task 3: Absorb one-shot canonical projection into `import`

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs`
- Modify: `crates/persisting-pchronicle-cli/src/exchange.rs`
- Modify: `crates/persisting-pchronicle-cli/src/settings.rs`
- Test: `crates/persisting-pchronicle-cli/src/tests.rs`

**Interfaces:**
- Consumes: `probe_canonical_event_store`, `storyline_projection_destination_exists`, and `build_storyline_projection`.
- Produces: `ImportArgs.output_format: Option<ImportOutputFormat>` and an `ImportResponse` with optional `input_bytes` plus optional `fact_rows`.

```rust
#[derive(Debug, Serialize)]
struct ImportResponse {
    dataset_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<String>,
    output_format: String,
    sources: usize,
    trajectories: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    fact_rows: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_bytes: Option<usize>,
}
```

- [ ] **Step 1: Write failing local canonical-import tests**

Assert auto-detection, contextual output format, source immutability, response fields, queryability, explicit `storyline` acceptance, explicit `preserve` rejection, and create-only output:

```rust
let before = std::fs::read(source.join("_manifest.json"))?;
let response = run_cli([
    "import", "--from", source_str, "--output", projection_str,
]).await?.json()?;
assert_eq!(response["format"], "events");
assert_eq!(response["source_path"], "events.lance");
assert_eq!(response["output_format"], "storyline-lance");
assert_eq!(response["sources"], 1);
assert_eq!(response["trajectories"], 1);
assert_eq!(response["fact_rows"], 1);
assert!(response.get("input_bytes").is_none());
assert_eq!(std::fs::read(source.join("_manifest.json"))?, before);
assert!(projection.join("CURRENT").is_file());
```

The suffix-only directory test must continue into ordinary directory import and report “contains no .json, .jsonl, or .ndjson files”; it must not be treated as canonical events.

- [ ] **Step 2: Run local import tests and verify failure**

Run:

```bash
cargo test -p persisting-pchronicle-cli --lib canonical_event_import
```

Expected: tests fail because the current import path scans `events.lance` as a JSON directory and defaults `output_format` to `preserve` before input classification.

- [ ] **Step 3: Split import classification before JSON file collection**

Probe non-stream input before calling `collect_import_candidates`. Resolve output mode contextually:

```rust
let canonical = if args.stream {
    None
} else {
    probe_canonical_event_store(&args.from).await?
};

if let Some(snapshot) = canonical {
    anyhow::ensure!(
        matches!(args.format, ExchangeFormat::Auto),
        "canonical event import does not accept a JSON exchange --format"
    );
    anyhow::ensure!(
        args.output_format != Some(ImportOutputFormat::Preserve),
        "canonical event import cannot preserve an existing event Store"
    );
    return run_canonical_event_import(args, snapshot, settings_override, stdout, stderr).await;
}

let output_format = args.output_format.unwrap_or(ImportOutputFormat::Preserve);
```

`run_canonical_event_import` may use a local default output when `--output` is omitted, but it must accept an explicit local path or supported object-store URI. Check destination existence before building; map `OutputNotEmpty` to `BoundaryCode::Conflict`; omit `input_bytes` from canonical-import JSON and stderr output.

- [ ] **Step 4: Preserve the existing JSON response contract**

Update existing JSON-import assertions to require the same numeric `input_bytes` and the same `preserve` default when `--output-format` is absent:

```rust
assert_eq!(response["output_format"], "preserve");
assert_eq!(response["input_bytes"], std::fs::metadata(&input)?.len());
assert!(response.get("fact_rows").is_none());
```

Run:

```bash
cargo test -p persisting-pchronicle-cli --lib import_
cargo test -p persisting-pchronicle-cli --test command_matrix import_matrix
```

Expected: all JSON, JSONL, NDJSON, recursive-directory, symlink, warning, and atomic-publication import tests pass unchanged except for the intentionally optional response members.

- [ ] **Step 5: Add object-store canonical import coverage**

Create canonical facts and the output under unique `shared-memory://` roots in the same process, then query the resulting Storyline Store:

```rust
let output = format!("shared-memory://canonical-import-{id}/storyline");
let response = run_cli([
    "import", "--from", source.as_str(), "--output", output.as_str(),
]).await?.json()?;
assert_eq!(response["fact_rows"], 1);
let store = StorylineLanceStore::open_uri(&output).await?;
assert!(store.current_table_paths().await?.is_some());
```

Run:

```bash
cargo test -p persisting-pchronicle-cli --lib canonical_event_import_supports_object_store_uris
```

Expected: pass.

- [ ] **Step 6: Commit canonical import**

```bash
git add crates/persisting-pchronicle-cli/src/lib.rs \
  crates/persisting-pchronicle-cli/src/exchange.rs \
  crates/persisting-pchronicle-cli/src/settings.rs \
  crates/persisting-pchronicle-cli/src/tests.rs
git commit -m "feat(pchronicle): import canonical events as Storyline"
```

### Task 4: Fold projection health into `status` and remove `project`

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs`
- Modify: `crates/persisting-pchronicle-cli/src/output.rs`
- Modify: `crates/persisting-pchronicle-cli/src/tests.rs`
- Modify: `crates/persisting-pchronicle-cli/tests/binary_contract.rs`

**Interfaces:**
- Consumes: `automatic_projection_inventory` and `inspect_automatic_storyline_projection`.
- Produces:

```rust
#[derive(Debug, Serialize)]
struct ProjectionStatusResponse {
    source_path: String,
    projection_path: String,
    status: ProjectionStatusName,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fact_version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fact_rows: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProjectionStatusName { Fresh, Stale, Missing, Error }
```

`StatusResponse` adds `projections: Vec<ProjectionStatusResponse>`.

- [ ] **Step 1: Replace project parser tests with absence and status tests**

Delete the `project watch` and `project verify` CLI tests. Change the command tree assertion and binary help contract:

```rust
assert_eq!(
    names,
    [
        "onboard", "default", "ls", "status", "query", "analysis", "find",
        "import", "export", "echo", "serve",
    ]
);
assert!(Cli::try_parse_from(["pchronicle", "project", "status"]).is_err());
```

Add status cases for fresh, stale, missing, lineage-free, malformed `CURRENT`, and two nested event sources. Assert array ordering by `source_path`, optional members, and no filesystem writes during status.

- [ ] **Step 2: Run parser and status tests and verify failure**

Run:

```bash
cargo test -p persisting-pchronicle-cli --lib command_tree_contains_the_product_commands
cargo test -p persisting-pchronicle-cli --lib status_reports_projection_
cargo test -p persisting-pchronicle-cli --test binary_contract help_exposes_the_supported_product_surface
```

Expected: parser tests fail because `project` still exists and status lacks `projections`.

- [ ] **Step 3: Remove the public project surface**

Remove `Command::Project`, all `Project*Args`, `ProjectCommand`, `run_project`, `run_project_watch`, watch-only response types/constants/imports, and the dispatch arm. Do not remove or deprecate the library projection functions.

Add `project` to the explicit forbidden command list in `binary_contract.rs`:

```rust
for command in ["control", "project", "search", "maintain"] {
    assert!(!stdout.lines().any(|line| {
        line.trim_start().starts_with(command)
    }));
}
```

- [ ] **Step 4: Implement read-only projection status aggregation**

Build inventory from the already pinned status Catalog. Inspect ready targets with `buffered(STATUS_PROJECTION_CONCURRENCY)`, where `STATUS_PROJECTION_CONCURRENCY` is a fixed `16`, so output order remains stable without adding another public flag. Convert each inspection error and each inventory error into an `error` record without exposing its source chain in JSON.

```rust
const STATUS_PROJECTION_CONCURRENCY: usize = 16;

let inventory = automatic_projection_inventory(snapshot.as_ref())?;
let mut projections = stream::iter(inventory.targets)
    .map(|target| async move {
        match inspect_automatic_storyline_projection(&target).await {
            Ok(inspection) => ProjectionStatusResponse::from_inspection(target, inspection),
            Err(error) => {
                tracing::error!(error = ?error, source = %target.source_path,
                    "pChronicle projection status inspection failed");
                ProjectionStatusResponse::error(target.source_path, target.projection_path)
            }
        }
    })
    .buffered(STATUS_PROJECTION_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
projections.extend(inventory.errors.into_iter().map(|error| {
    ProjectionStatusResponse::error(error.source_path, error.projection_path)
}));
projections.sort_by(|left, right| left.source_path.cmp(&right.source_path));
```

The table output adds a compact block after aggregate counts:

```text
PROJECTION                         STATUS   FACT_VERSION FACT_ROWS GENERATION
a/events.lance -> a/storyline      fresh    12           4812      generation-id
b/events.lance -> b/storyline      missing  3            97
```

- [ ] **Step 5: Run status and binary contracts**

Run:

```bash
cargo test -p persisting-pchronicle-cli --lib status_
cargo test -p persisting-pchronicle-cli --test command_matrix
cargo test -p persisting-pchronicle-cli --test binary_contract
```

Expected: all tests pass and `project` is rejected as an unknown subcommand.

- [ ] **Step 6: Commit status consolidation**

```bash
git add crates/persisting-pchronicle-cli/src/lib.rs \
  crates/persisting-pchronicle-cli/src/output.rs \
  crates/persisting-pchronicle-cli/src/tests.rs \
  crates/persisting-pchronicle-cli/tests/binary_contract.rs
git commit -m "feat(pchronicle): report projections through status"
```

### Task 5: Prepare and atomically refresh Warehouse Catalog runtimes

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/server/mod.rs`
- Modify: `crates/persisting-pchronicle-cli/src/server/tests.rs`
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs`

**Interfaces:**
- Consumes: `ChronicleServerConfig`, `DatasetCatalogSnapshot::discover`, and `ChronicleQueryEngine`.
- Produces:

```rust
#[derive(Clone)]
pub(crate) struct PreparedWarehouse {
    state: AppState,
}

impl PreparedWarehouse {
    pub(crate) async fn prepare(config: ChronicleServerConfig) -> anyhow::Result<Self>;
    pub(crate) async fn refresh_catalog(&self) -> anyhow::Result<String>;
    pub(crate) fn router(&self) -> Router;
    #[cfg(test)]
    pub(crate) async fn current_snapshot_id(&self) -> Option<String>;
}

pub(crate) async fn serve_prepared_warehouse_with_listener_and_shutdown(
    warehouse: PreparedWarehouse,
    listener: tokio::net::TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()>;
```

- [ ] **Step 1: Write failing prepared-Catalog tests**

Assert that prepare installs a Catalog before any request, refresh atomically replaces it, and failed refresh leaves the old snapshot and trajectory cache available:

```rust
let prepared = PreparedWarehouse::prepare(config).await?;
let first = prepared.current_snapshot_id().await.expect("prepared snapshot");

std::fs::write(root.join("second.json"), fixture_bytes())?;
let second = prepared.refresh_catalog().await?;
assert_ne!(second, first);

std::fs::create_dir(root.join("broken"))?;
std::fs::write(root.join("broken/CURRENT"), "{")?;
assert!(prepared.refresh_catalog().await.is_err());
assert_eq!(prepared.current_snapshot_id().await.as_deref(), Some(second.as_str()));
```

Keep the existing HTTP `POST /api/catalog` atomicity test and route it through the same handle method.

- [ ] **Step 2: Run the focused server tests and verify failure**

Run:

```bash
cargo test -p persisting-pchronicle-cli --lib server::tests::prepared_catalog_
cargo test -p persisting-pchronicle-cli --lib server::tests::catalog_refresh_is_atomic_and_dataset_filtering_is_explicit
```

Expected: compilation fails because `PreparedWarehouse` does not exist.

- [ ] **Step 3: Implement prepared state and one swap primitive**

Build the complete `CatalogRuntime` outside the write lock. Install the runtime and clear the trajectory cache only after construction succeeds:

```rust
async fn install_catalog_runtime(&self, runtime: Arc<CatalogRuntime>) -> String {
    let snapshot_id = runtime.snapshot.snapshot_id().to_owned();
    *self.state.catalog.write().await = Some(runtime);
    *self.state.trajectory_cache.write().await = None;
    snapshot_id
}

pub(crate) async fn refresh_catalog(&self) -> Result<String> {
    let runtime = build_catalog_runtime(&self.state.config).await?;
    Ok(self.install_catalog_runtime(runtime).await)
}
```

`warehouse_router(config)` remains available for existing library and HTTP tests; it creates lazy state as before. The unified `serve` path uses `PreparedWarehouse::prepare` and the prepared listener function so readiness implies a complete initial Catalog whenever `--listen` is present.

- [ ] **Step 4: Run server tests**

Run:

```bash
cargo test -p persisting-pchronicle-cli --lib server::tests
cargo test -p persisting-pchronicle-cli --test server_http_contract
```

Expected: all tests pass.

- [ ] **Step 5: Commit prepared Warehouse support**

```bash
git add crates/persisting-pchronicle-cli/src/server/mod.rs \
  crates/persisting-pchronicle-cli/src/server/tests.rs \
  crates/persisting-pchronicle-cli/src/lib.rs
git commit -m "refactor(pchronicle): prepare atomic Warehouse catalogs"
```

### Task 6: Add the bounded projection supervisor to `serve`

**Files:**
- Create: `crates/persisting-pchronicle-cli/src/projection_supervisor.rs`
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs`
- Test: `crates/persisting-pchronicle-cli/src/tests.rs`

**Interfaces:**
- Consumes: `ChronicleServerConfig`, `PreparedWarehouse`, `automatic_projection_inventory`, and `maintain_automatic_storyline_projection`.
- Produces:

```rust
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProjectionSupervisorOptions {
    pub(crate) interval: Duration,
    pub(crate) max_backoff: Duration,
    pub(crate) max_concurrent: usize,
}

pub(crate) struct ProjectionSupervisor {
    config: server::ChronicleServerConfig,
    warehouse: Option<server::PreparedWarehouse>,
    options: ProjectionSupervisorOptions,
    diagnostics: tokio::sync::mpsc::Sender<ProjectionDiagnostic>,
    retries: BTreeMap<String, RetryState>,
    catalog_retry: Option<RetryState>,
    observed_snapshot_id: Option<String>,
    catalog_dirty: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectionIterationReport {
    pub(crate) succeeded: usize,
    pub(crate) failed: usize,
    pub(crate) publications: usize,
    pub(crate) catalog_refreshes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectionDiagnostic {
    pub(crate) source_path: String,
    pub(crate) projection_path: String,
    pub(crate) status: &'static str,
    pub(crate) retry_ms: u64,
}

fn sanitize_log_field(value: &str) -> String {
    value.chars().flat_map(char::escape_default).collect()
}

impl ProjectionSupervisor {
    pub(crate) fn new(
        config: server::ChronicleServerConfig,
        warehouse: Option<server::PreparedWarehouse>,
        diagnostics: tokio::sync::mpsc::Sender<ProjectionDiagnostic>,
    ) -> Self;

    pub(crate) fn set_warehouse(
        &mut self,
        warehouse: Option<server::PreparedWarehouse>,
    );

    pub(crate) async fn converge_before_readiness(&mut self) -> anyhow::Result<()>;

    pub(crate) async fn run_iteration(
        &mut self,
        now: tokio::time::Instant,
    ) -> ProjectionIterationReport;

    pub(crate) async fn run(
        self,
        stop: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()>;
}
```

- [ ] **Step 1: Write failing startup and runtime iteration tests**

Use short test-only options to cover:

- startup builds all initially discovered projections before returning;
- startup rejects a foreign or lineage-free deterministic destination;
- two sources are attempted even if one fails;
- appending facts leads to incremental convergence;
- a newly created `events.lance` is discovered after the first iteration;
- a failed source receives capped exponential backoff without delaying healthy sources;
- one iteration with multiple publications requests one Catalog refresh;
- changed canonical membership/watermarks mark the Catalog dirty even when projection fails, so Warehouse can publish a stale/fallback Catalog;
- a failed Catalog refresh retains `catalog_dirty=true` and is retried.
- a source or object key containing newline or ANSI escape characters still emits exactly one escaped diagnostic line.

Representative assertion:

```rust
supervisor.converge_before_readiness().await?;
assert_eq!(projection_state(&first).await?, Fresh);

append_note(&second_storage, "new-session", 0).await?;
let report = supervisor.run_iteration(Instant::now()).await;
assert_eq!(report.succeeded, 2);
assert_eq!(report.failed, 0);
assert_eq!(report.catalog_refreshes, 1);
```

- [ ] **Step 2: Run supervisor tests and verify failure**

Run:

```bash
cargo test -p persisting-pchronicle-cli --lib projection_supervisor::tests
```

Expected: compilation fails because the module is absent.

- [ ] **Step 3: Implement bounded iteration and per-source retry state**

Discover a fresh Report-policy Catalog each iteration, derive its inventory, and run due targets with `buffer_unordered(options.max_concurrent)`. Store retry state by `source_uri`:

```rust
fn retry_delay(&self, failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(20);
    let multiplier = 1u32.checked_shl(exponent).unwrap_or(u32::MAX);
    self.options.interval
        .saturating_mul(multiplier)
        .min(self.options.max_backoff)
}
```

Success removes the retry entry. Failure increments only that source's entry and uses `try_send` to place one sanitized `ProjectionDiagnostic` into a bounded channel containing only `source_path`, `projection_path`, state, and retry delay. A full diagnostic channel drops the duplicate diagnostic rather than delaying maintenance; no Control token or error source chain is passed into this module.

Set `catalog_dirty` when the discovered snapshot ID changes or a projection publishes. If a prepared Warehouse exists and the iteration reaches its refresh phase, call `refresh_catalog` once. Clear `catalog_dirty` and `catalog_retry` only on successful installation. A failed Catalog build advances its own `catalog_retry` with the same capped delay function, so it is retried independently without re-running already-fresh projections.

- [ ] **Step 4: Implement startup and shutdown semantics**

`converge_before_readiness` attempts every initial target with bounded concurrency, then returns one summarized error if any discovery/maintenance error remains. It does not apply runtime backoff.

The runtime loop checks the stop watch between iterations. Once an iteration starts, await its maintenance futures and Catalog publication before returning, ensuring shutdown cannot abandon a `CURRENT` publication future midway:

```rust
loop {
    tokio::select! {
        changed = stop.changed() => {
            if changed.is_err() || *stop.borrow() { return Ok(()); }
        }
        _ = tokio::time::sleep(next_delay) => {
            self.run_iteration(Instant::now()).await;
        }
    }
}
```

- [ ] **Step 5: Wire startup ordering into `run_serve`**

The order must be:

```rust
let config = resolve_serve_config(&args)?;
let (diagnostic_tx, diagnostic_rx) = tokio::sync::mpsc::channel(256);
let mut projections = ProjectionSupervisor::new(
    config.clone(),
    None,
    diagnostic_tx,
);
projections.converge_before_readiness().await?;

let warehouse = match args.listen {
    Some(_) => Some(server::PreparedWarehouse::prepare(config.clone()).await?),
    None => None,
};
projections.set_warehouse(warehouse.clone());

// Bind/prepare enabled listeners and services.
// Emit and flush exactly one ChronicleServeReady JSON line.
// Run Warehouse, Control, Gateway, the projection supervisor, and the
// diagnostic receiver together.
```

No Warehouse Catalog is prepared when `--listen` is absent. Add the supervisor as a managed sibling in `serve_components`; a runtime source error remains internal to the supervisor and therefore does not end sibling services.

Pass `diagnostic_rx` and the existing borrowed `stderr: &mut dyn Write` into `serve_components`. Its `tokio::select!` drains diagnostics and writes one escaped line at a time while also waiting for shutdown or a service completion:

```rust
diagnostic = diagnostic_rx.recv() => {
    if let Some(diagnostic) = diagnostic {
        writeln!(
            stderr,
            "projection source={} output={} status={} retry_ms={}",
            sanitize_log_field(&diagnostic.source_path),
            sanitize_log_field(&diagnostic.projection_path),
            diagnostic.status,
            diagnostic.retry_ms,
        )?;
    }
}
```

Do not call `eprintln!`: `main` holds a `StderrLock` for the lifetime of `run_with_stdio`, so direct background stderr locking can deadlock. On shutdown, signal services first, await the active supervisor iteration, then drain diagnostics until its sender is dropped.

- [ ] **Step 6: Run in-process serve and supervisor tests**

Run:

```bash
cargo test -p persisting-pchronicle-cli --lib projection_supervisor::tests
cargo test -p persisting-pchronicle-cli --lib serve_
```

Expected: all tests pass, stdout contains no maintenance events, and shutdown waits for an active iteration.

- [ ] **Step 7: Commit the supervisor**

```bash
git add crates/persisting-pchronicle-cli/src/projection_supervisor.rs \
  crates/persisting-pchronicle-cli/src/lib.rs \
  crates/persisting-pchronicle-cli/src/tests.rs
git commit -m "feat(pchronicle): maintain projections under serve"
```

### Task 7: Prove process-level readiness, refresh, fallback, and CAS behavior

**Files:**
- Modify: `crates/persisting-pchronicle-cli/tests/control_process.rs`
- Modify: `crates/persisting-pchronicle-cli/tests/binary_contract.rs`

**Interfaces:**
- Consumes: the public `pchronicle import`, `serve`, Control protocol, Warehouse HTTP API, and Storyline storage inspection.
- Produces: end-to-end acceptance coverage; no new production API.

- [ ] **Step 1: Add a process helper with bounded polling**

```rust
async fn wait_until<F, Fut>(
    timeout: Duration,
    mut condition: F,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool>>,
{
    tokio::time::timeout(timeout, async {
        loop {
            if condition().await? { return Ok(()); }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.context("timed out waiting for pChronicle state")??;
    Ok(())
}
```

All child processes use `kill_on_drop(true)` and consume stderr after termination so a full pipe cannot deadlock the test.

- [ ] **Step 2: Write readiness and runtime discovery process tests**

Create one canonical source before launch and assert its sibling projection is fresh before the readiness line is accepted. Then append a second source through Control after readiness and poll for its sibling projection while continuing to ping Control.

```rust
let ready = read_ready(&mut child).await?;
assert_eq!(inspect_source(&initial).await?.state, Fresh);

append_through_control(&ready, second_request).await?;
wait_until(Duration::from_secs(10), || async {
    Ok(inspect_source(&second).await.is_ok_and(|inspection| {
        inspection.state == AutomaticProjectionState::Fresh
    }))
}).await?;
ping_control(&ready).await?;
```

- [ ] **Step 3: Write startup-conflict and runtime-failure tests**

For startup, create a lineage-free valid Storyline Store at the deterministic destination and assert the process exits non-zero without a stdout readiness line and without changing `CURRENT`.

For runtime, start with an empty Dataset, create a foreign destination, append the matching canonical source through Control, wait for a projection error line on stderr, and assert a later Control append and ping still succeed.

- [ ] **Step 4: Write Warehouse atomic-refresh and fallback tests**

Start `serve --listen ... --control ...`, capture `/api/catalog` snapshot ID, append canonical facts, and poll until a new snapshot reports a fresh projection generation. Then place a valid foreign Storyline Store at a newly created source's deterministic destination before appending its canonical facts. Assert the replacement Catalog reports that exact event `_file_` as missing/error and an `_file_ = '.../events.lance'` bounded query uses canonical fallback; the foreign Source must not be mistaken for matching lineage.

For Catalog build failure, add a malformed committed Source before refresh, assert `/api/catalog` retains the old snapshot ID, remove the malformed Source, and assert the supervisor retry eventually installs a new snapshot.

- [ ] **Step 5: Write a two-process CAS test**

Start two `serve --storage ROOT --control 127.0.0.1:0` processes against the same initially unprojected source. Both must emit readiness, the deterministic destination must have one valid fresh `CURRENT`, and neither process may overwrite it with foreign lineage.

- [ ] **Step 6: Add release-profile canonical import smoke coverage**

In `binary_contract.rs`, create a local canonical source, execute `CARGO_BIN_EXE_pchronicle import --from EVENTS --output STORYLINE`, parse stdout, and query the new output. Run the test under release profile:

```bash
cargo test --release -p persisting-pchronicle-cli \
  --test binary_contract canonical_event_import_is_queryable_in_release
```

Expected: response is `format=events`, `output_format=storyline-lance`, `fact_rows=1`, no `input_bytes`, and the query observes one trajectory.

- [ ] **Step 7: Run all process contracts**

Run:

```bash
cargo test -p persisting-pchronicle-cli --test control_process -- --test-threads=1
cargo test -p persisting-pchronicle-cli --test binary_contract
cargo test -p persisting-pchronicle-cli --test server_http_contract
```

Expected: all tests pass without timing-dependent sleeps beyond bounded polling.

- [ ] **Step 8: Commit process coverage**

```bash
git add crates/persisting-pchronicle-cli/tests/control_process.rs \
  crates/persisting-pchronicle-cli/tests/binary_contract.rs
git commit -m "test(pchronicle): cover automatic projection lifecycle"
```

### Task 8: Replace manual projection documentation and verify the scoped product

**Files:**
- Modify: `crates/persisting-pchronicle-cli/README.md`
- Modify: `crates/persisting-pchronicle/README.md`
- Modify: `docs/src/pchronicle/reference/cli.md`
- Modify: `docs/src/pchronicle/guides/exchange.md`
- Modify: `docs/src/pchronicle/guides/exchange.zh.md`
- Modify: `docs/src/pchronicle/guides/serve.md`
- Modify: `docs/src/pchronicle/guides/serve.zh.md`
- Modify: `docs/src/pchronicle/guides/serve-gateway.md`
- Modify: `docs/src/pchronicle/guides/serve-gateway.zh.md`
- Modify: `docs/src/pchronicle/design/storyline-lance.md`
- Modify: `docs/src/pchronicle/design/storyline-lance.zh.md`
- Modify: `docs/src/pchronicle/design/catalog.md`
- Modify: `docs/src/pchronicle/design/catalog.zh.md`

**Interfaces:**
- Consumes: the final CLI help and behavior from Tasks 3–7.
- Produces: one user model centered on `import`, `serve`, and `status`.

- [ ] **Step 1: Update command reference and exchange docs**

Document both contextual forms exactly:

```bash
# JSON remains byte-preserving unless explicitly squashed.
pchronicle import --from ./corpus --output ./dataset

# A validated canonical event Store always creates Storyline Lance.
pchronicle import \
  --from ./run/events.lance \
  --output ./run/storyline
```

State that canonical import omits `input_bytes`, reports `fact_rows`, accepts local/object-store URIs, does not mutate the source, and is create-only. Explain that explicit `--output-format preserve` is invalid for canonical events.

- [ ] **Step 2: Update serve, Storyline, and Catalog docs**

Replace every manual build/sync/watch/rebuild command with:

```bash
pchronicle serve --storage ./trajectory-data --control 127.0.0.1:0
pchronicle status ./trajectory-data --format json
```

Document pre-readiness convergence, deterministic sibling placement, runtime discovery, bounded retry, durable-write independence, stale canonical fallback, complete Catalog rebuild plus atomic swap, and old-snapshot retention after refresh failure. Remove statements that Catalog refresh is only explicit.

- [ ] **Step 3: Prove no public manual command remains in maintained docs/code**

Run:

```bash
rg -n "pchronicle project|project (build|status|verify|sync|watch|rebuild)" \
  crates/persisting-pchronicle-cli crates/persisting-pchronicle/README.md \
  docs/src/pchronicle -g '*.rs' -g '*.md'
```

Expected: no matches. Internal Rust function names such as `build_storyline_projection` are allowed and are not matched by this command-oriented expression.

- [ ] **Step 4: Run formatting and focused static checks**

Run:

```bash
cargo fmt --all -- --check
cargo clippy -p persisting-pchronicle --all-targets --features lance-store,s3-store -- -D warnings
cargo clippy -p persisting-pchronicle-cli --all-targets -- -D warnings
```

Expected: all checks pass.

- [ ] **Step 5: Run the scoped test suite**

Run:

```bash
cargo test -p persisting-pchronicle --lib --features lance-store,s3-store
cargo test -p persisting-pchronicle \
  --test document_source \
  --test query_engine \
  --test storyline_lance_roundtrip \
  --test s3_storage
cargo test -p persisting-pchronicle-cli --lib
cargo test -p persisting-pchronicle-cli --tests -- --test-threads=1
```

Expected: all in-scope tests pass. Do not broaden acceptance to Search, Queue, TTAS, or `persisting-dlcapt`.

- [ ] **Step 6: Build strict documentation and run release smoke**

Run:

```bash
just docs-links
cargo test --release -p persisting-pchronicle-cli \
  --test binary_contract canonical_event_import_is_queryable_in_release
```

Expected: strict MkDocs build passes and the release-profile smoke passes.

- [ ] **Step 7: Commit documentation**

```bash
git add crates/persisting-pchronicle-cli/README.md \
  crates/persisting-pchronicle/README.md \
  docs/src/pchronicle/reference/cli.md \
  docs/src/pchronicle/guides/exchange.md \
  docs/src/pchronicle/guides/exchange.zh.md \
  docs/src/pchronicle/guides/serve.md \
  docs/src/pchronicle/guides/serve.zh.md \
  docs/src/pchronicle/guides/serve-gateway.md \
  docs/src/pchronicle/guides/serve-gateway.zh.md \
  docs/src/pchronicle/design/storyline-lance.md \
  docs/src/pchronicle/design/storyline-lance.zh.md \
  docs/src/pchronicle/design/catalog.md \
  docs/src/pchronicle/design/catalog.zh.md
git commit -m "docs(pchronicle): document automatic Storyline projection"
```

- [ ] **Step 8: Record final evidence**

Capture the exact passing commands, test counts, skipped environment-dependent object-store cases, and release-smoke result in the final handoff. Report any pre-existing unrelated dirty-worktree changes separately and do not include them in these commits.

# pChronicle Lightweight Errors Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace pChronicle's cross-layer typed error framework with local expected outcomes, `anyhow::Result` for operational failures, and seven stable classifications that exist only at HTTP, CLI, and Gateway boundaries.

**Architecture:** Leaf parsers return a small document-local `InputIssue`; lookups use `Option`; commands expose module-local outcomes only when callers branch. All other failures flow through `anyhow` with one context at ownership boundaries, DataFusion uses a source-only bridge, and transport code maps explicit outcomes to public responses without inspecting error text or source types.

**Tech Stack:** Rust 2021, anyhow, thiserror for leaf issues only, tracing, Tokio, DataFusion 54, Lance 9, Axum.

**Spec:** `docs/superpowers/specs/2026-08-18-pchronicle-unified-error-design.md`

## Global Constraints

- Stable classification exists only at HTTP, CLI, and Gateway boundaries.
- Internal production code must not reference protocol codes or HTTP status values.
- Operational failures use `anyhow::Result`; expected decisions use `Option`, `InputIssue`, or a module-local Outcome.
- Do not classify by `Display`, `to_string()`, source downcast, or backend error text.
- Add `anyhow::Context` only at module entry, resource ownership transfer, task join, or external trait bridge boundaries.
- Do not introduce a global outcome type, propagation frames, structured error context, diagnostic copies, or a backend-to-protocol classifier.
- DataFusion adaptation preserves sources only and contains no protocol classification.
- The public protocol is exactly `invalid_request`, `not_found`, `conflict`, `unsupported`, `resource_exhausted`, `unavailable`, and `internal`.
- `internal` and `unavailable` responses use fixed public messages and never expose source text or paths.
- Search, TTAS/tiered memory, general Queue/Sampler, and standalone dlcapt remain untouched.
- Preserve the existing unstaged documentation changes, `.workbuddy/`, and `docs/src/rfcs/0006-pchronicle-vortex-backend.md`.

## Execution Preflight

The index currently contains the rejected thick-error implementation. Preserve it without changing unstaged user files, then execute this plan from commit `6a0a3f2e`'s source tree:

```bash
git stash push --staged -m "archive/rejected-pchronicle-thick-error"
git status --short
git rev-parse HEAD
```

Expected:

- `HEAD` is `6a0a3f2e` or a descendant containing the approved design;
- the old implementation is recoverable from the named stash;
- only the five pre-existing plan deletions, five matching design edits, `.workbuddy/`, and the Vortex RFC remain dirty;
- do not drop the named stash during implementation.

---

### Task 1: Establish the lightweight result and input contract

**Files:**
- Create: `crates/persisting-pchronicle/src/input.rs`
- Modify: `crates/persisting-pchronicle/src/error.rs` (temporary `InputIssue` bridge; deleted in Task 6)
- Modify: `crates/persisting-pchronicle/src/lib.rs`
- Modify: `crates/persisting-pchronicle/src/document.rs`
- Modify: `crates/persisting-pchronicle/src/model.rs`
- Modify: `crates/persisting-pchronicle/src/format.rs`
- Modify: `crates/persisting-pchronicle/src/atif.rs`
- Modify: `crates/persisting-pchronicle/src/formats/actf.rs`
- Modify: `crates/persisting-pchronicle/src/formats/events.rs`
- Modify: `crates/persisting-pchronicle/src/formats/openai_corpus.rs`
- Modify: `crates/persisting-pchronicle/src/formats/storyline.rs`
- Modify: `crates/persisting-pchronicle/src/agenticmd/codec.rs`
- Modify: `crates/persisting-pchronicle/src/agenticmd/convert.rs`
- Modify: `crates/persisting-pchronicle/src/agenticmd/fs.rs`
- Modify: `crates/persisting-pchronicle/src/agenticmd/validate.rs`
- Modify: `crates/persisting-pchronicle/src/convert/actf.rs`
- Modify: `crates/persisting-pchronicle/src/convert/atif.rs`
- Modify: `crates/persisting-pchronicle/src/convert/events.rs`
- Modify: `crates/persisting-pchronicle/src/layout/resolve.rs`
- Test: `crates/persisting-pchronicle/tests/public_api.rs`
- Test: `crates/persisting-pchronicle/src/document.rs`

**Interfaces:**
- Produces: `InputIssue`, `InputIssueKind::{Invalid, Unsupported}`, and `InputResult<T>` from `document`.
- Produces: in-memory decode/validation APIs returning `InputResult<T>`.
- Transitional: the legacy crate `Result<T>` remains until Task 6 so each intermediate commit compiles; add `Error::Input(#[from] InputIssue)` only as a temporary bridge.

- [ ] **Step 1: Replace the public API regression with a failing lightweight contract**

Add these assertions to `tests/public_api.rs` and remove constructor/code assertions for the old global error:

```rust
use persisting_pchronicle::document::{InputIssue, InputIssueKind, InputResult};

#[test]
fn public_errors_are_operational_or_input_local() {
    let issue = InputIssue::invalid("invalid JSON").at("turns[0]");
    assert_eq!(issue.kind(), InputIssueKind::Invalid);
    assert_eq!(issue.message(), "invalid JSON");
    assert_eq!(issue.location(), Some("turns[0]"));
    let _: InputResult<()> = Err(issue);
}
```

- [ ] **Step 2: Run the public contract and record the RED result**

Run:

```bash
cargo test -p persisting-pchronicle --test public_api --locked
```

Expected: compile failure because `InputIssue`, `InputIssueKind`, `InputResult`, and the anyhow result façade do not exist.

- [ ] **Step 3: Add the leaf input type and split input from operational failures**

Implement `input.rs` with this complete public shape:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputIssueKind {
    Invalid,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct InputIssue {
    kind: InputIssueKind,
    message: String,
    location: Option<String>,
}

impl InputIssue {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self { kind: InputIssueKind::Invalid, message: message.into(), location: None }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self { kind: InputIssueKind::Unsupported, message: message.into(), location: None }
    }

    pub fn at(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    pub fn kind(&self) -> InputIssueKind { self.kind }
    pub fn message(&self) -> &str { &self.message }
    pub fn location(&self) -> Option<&str> { self.location.as_deref() }
}

pub type InputResult<T> = std::result::Result<T, InputIssue>;
```

Convert in-memory parse and validation entrypoints to `InputResult<T>`. Construct `InputIssue` only where the parser owns the invalid/unsupported decision. Add a temporary `Input(#[from] InputIssue)` variant to the legacy `error.rs` so unchanged callers can use `?` during Tasks 1–5; do not expose new codes or classification from that variant. Leave operational signatures unchanged until their owning task migrates them to anyhow. Do not preserve path, format, frames, or source copies in `InputIssue`; parser locations remain local field coordinates.

- [ ] **Step 4: Run focused format and public tests**

Run:

```bash
cargo test -p persisting-pchronicle --test public_api --locked
cargo test -p persisting-pchronicle document:: --locked
cargo test -p persisting-pchronicle agenticmd:: --locked
cargo test -p persisting-pchronicle convert:: --locked
```

Expected: PASS. Invalid/unsupported inputs remain distinguishable through `InputIssueKind`; operational errors retain their source through anyhow.

- [ ] **Step 5: Commit the lightweight core and input contract**

```bash
git add crates/persisting-pchronicle/src/input.rs crates/persisting-pchronicle/src/error.rs crates/persisting-pchronicle/src/lib.rs crates/persisting-pchronicle/src/document.rs crates/persisting-pchronicle/src/model.rs crates/persisting-pchronicle/src/format.rs crates/persisting-pchronicle/src/atif.rs crates/persisting-pchronicle/src/formats crates/persisting-pchronicle/src/agenticmd crates/persisting-pchronicle/src/convert crates/persisting-pchronicle/src/layout/resolve.rs crates/persisting-pchronicle/tests/public_api.rs
git commit -m "refactor: separate pchronicle input issues from failures"
```

### Task 2: Reduce storage adaptation to anyhow and a source-only DataFusion bridge

**Files:**
- Create: `crates/persisting-pchronicle/src/store/datafusion_bridge.rs`
- Modify: `crates/persisting-pchronicle/src/store/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/document_source.rs`
- Modify: `crates/persisting-pchronicle/src/document.rs`
- Modify: `crates/persisting-pchronicle/src/store/query_engine.rs`
- Modify: `crates/persisting-pchronicle/src/store/agenticmd_datafusion.rs`
- Modify: `crates/persisting-pchronicle/src/store/events/datafusion.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/datafusion.rs`
- Modify: `crates/persisting-pchronicle/src/store/files/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/files/atif_reader.rs`
- Modify: `crates/persisting-pchronicle/src/store/files/atif_stream.rs`
- Modify: `crates/persisting-pchronicle/src/store/catalog/provider.rs`
- Test: `crates/persisting-pchronicle/src/store/datafusion_bridge.rs`
- Test: `crates/persisting-pchronicle/tests/document_source.rs`
- Test: `crates/persisting-pchronicle/tests/query_engine.rs`

**Interfaces:**
- Consumes: existing storage functions that already return `anyhow::Result<T>`; transitional typed callers remain supported until Task 6.
- Produces: private `into_datafusion(anyhow::Error) -> DataFusionError` and `from_datafusion(&'static str, DataFusionError) -> anyhow::Error`.
- Produces: no error code, classifier, copied context, or backend variant mapping.

- [ ] **Step 1: Write RED bridge tests**

Create focused tests proving only source preservation:

```rust
#[test]
fn external_roundtrip_preserves_root_source() {
    let error = anyhow::Error::new(std::io::Error::other("disk sentinel"))
        .context("read source");
    let recovered = from_datafusion("execute query", into_datafusion(error));
    let rendered = format!("{recovered:#}");
    assert!(rendered.contains("execute query"));
    assert!(rendered.contains("read source"));
    assert!(rendered.contains("disk sentinel"));
}

#[test]
fn native_datafusion_failure_remains_a_source() {
    let recovered = from_datafusion(
        "plan query",
        datafusion::error::DataFusionError::Plan("bad plan".into()),
    );
    assert!(format!("{recovered:#}").contains("bad plan"));
}
```

- [ ] **Step 2: Run the bridge test and verify RED**

Run:

```bash
cargo test -p persisting-pchronicle store::datafusion_bridge --features lance-store --locked
```

Expected: compile failure because the source-only bridge does not exist.

- [ ] **Step 3: Implement the source-only bridge and remove storage classifiers**

Use a private wrapper with `Display` delegated to `{:#}` and `source()` delegated to `anyhow::Error::as_ref()`. `into_datafusion` stores that wrapper in `External`. `from_datafusion` unwraps the private wrapper when owned; otherwise it returns `anyhow::Error::new(datafusion_error).context(operation)`. Remove Lance/object-store/DataFusion-to-code tables, propagation helpers, context-copy helpers, and per-call classification `map_err` blocks. Keep at most one `.context(...)` at each trait or resource boundary.

```rust
#[derive(Debug)]
struct ExternalFailure(anyhow::Error);

impl std::fmt::Display for ExternalFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}", self.0)
    }
}

impl std::error::Error for ExternalFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

pub(super) fn into_datafusion(error: anyhow::Error) -> DataFusionError {
    DataFusionError::External(Box::new(ExternalFailure(error)))
}

pub(super) fn from_datafusion(
    operation: &'static str,
    error: DataFusionError,
) -> anyhow::Error {
    match error {
        DataFusionError::External(source) => match source.downcast::<ExternalFailure>() {
            Ok(source) => source.0.context(operation),
            Err(source) => anyhow::Error::new(DataFusionError::External(source)).context(operation),
        },
        source => anyhow::Error::new(source).context(operation),
    }
}
```

- [ ] **Step 4: Run storage/query regressions**

Run:

```bash
cargo test -p persisting-pchronicle --features lance-store --test document_source --locked
cargo test -p persisting-pchronicle --features lance-store --test query_engine --locked
cargo test -p persisting-pchronicle --features lance-store --test direct_file_query --locked
```

Expected: PASS. Malformed data returns an anyhow chain; no test branches on a global error code.

- [ ] **Step 5: Commit the storage bridge**

```bash
git add crates/persisting-pchronicle/src/store
git commit -m "refactor: reduce storage errors to source propagation"
```

### Task 3: Express storage and projection decisions as local outcomes

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/events/manifest.rs`
- Modify: `crates/persisting-pchronicle/src/store/events/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/run_control.rs`
- Modify: `crates/persisting-pchronicle/src/projection/storyline.rs`
- Modify: `crates/persisting-pchronicle/src/storage.rs`
- Test: `crates/persisting-pchronicle/src/store/events/manifest.rs`
- Test: `crates/persisting-pchronicle/src/projection/storyline.rs`

**Interfaces:**
- Produces: private `ManifestWriteOutcome<T>::Applied(T) | Conflict(EventWriterConflict)` for writer-fence/CAS decisions.
- Produces: `StorylineProjectionBuildOutcome::{Built(report), OutputNotEmpty}`.
- Produces: `StorylineProjectionSyncOutcome::{Synced(report), MissingProjection, RequiresRebuild(reason)}`.
- Produces: `ProjectionRebuildReason::{MissingLineage, IncompatibleLineage, NonMonotonicWatermark}`.

- [ ] **Step 1: Write RED outcome tests**

Replace code-based assertions with explicit decisions:

```rust
#[tokio::test]
async fn building_into_nonempty_output_is_an_explicit_conflict() {
    let outcome = build_storyline_projection(&source, &output, "events.lance")
        .await
        .unwrap();
    assert!(matches!(outcome, StorylineProjectionBuildOutcome::OutputNotEmpty));
}

#[tokio::test]
async fn empty_projection_is_an_explicit_missing_result() {
    let outcome = sync_storyline_projection(&source, &output).await.unwrap();
    assert!(matches!(outcome, StorylineProjectionSyncOutcome::MissingProjection));
}
```

Add a manifest test asserting a stale fence returns `ManifestWriteOutcome::Conflict` while an object-store failure remains `Err`.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test -p persisting-pchronicle projection::storyline:: --features lance-store --locked
cargo test -p persisting-pchronicle store::events::manifest:: --features lance-store --locked
```

Expected: compile failures because the outcome types and signatures do not exist.

- [ ] **Step 3: Implement local outcomes without a shared classifier**

Return `ManifestWriteOutcome` only for writer-fence/CAS rejection; preserve I/O, serialization, join, and object-store failures as anyhow. Return the projection outcomes at build/sync decision points. Keep successful report structs unchanged inside `Built`/`Synced`. Use fixed `ProjectionRebuildReason` variants rather than diagnostic strings for boundary decisions.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EventWriterConflict {
    StaleFence,
    EpochAlreadyOwned,
    PublicationChanged,
}

pub(super) enum ManifestWriteOutcome<T> {
    Applied(T),
    Conflict(EventWriterConflict),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionRebuildReason {
    MissingLineage,
    IncompatibleLineage,
    NonMonotonicWatermark,
}

pub enum StorylineProjectionBuildOutcome {
    Built(StorylineProjectionBuildReport),
    OutputNotEmpty,
}

pub enum StorylineProjectionSyncOutcome {
    Synced(StorylineProjectionSyncReport),
    MissingProjection,
    RequiresRebuild(ProjectionRebuildReason),
}
```

- [ ] **Step 4: Run projection and storage tests**

Run:

```bash
cargo test -p persisting-pchronicle projection::storyline:: --features lance-store --locked
cargo test -p persisting-pchronicle store::events:: --features lance-store --locked
cargo test -p persisting-pchronicle --test storyline_lance_roundtrip --features lance-store --locked
```

Expected: PASS. Expected state transitions are matchable values; corrupted or unavailable storage remains `Err`.

- [ ] **Step 5: Commit local storage outcomes**

```bash
git add crates/persisting-pchronicle/src/store/events crates/persisting-pchronicle/src/store/run_control.rs crates/persisting-pchronicle/src/projection/storyline.rs crates/persisting-pchronicle/src/storage.rs
git commit -m "refactor: expose storage decisions as outcomes"
```

### Task 4: Make append admission an outcome and preserve operational failures

**Files:**
- Modify: `crates/persisting-pchronicle/src/append_queue.rs`
- Modify: `crates/persisting-pchronicle/src/storage.rs`
- Modify: `crates/persisting-pchronicle-cli/src/gateway_capture.rs`
- Modify: `crates/persisting-gateway/tests/agenticmd_bridge.rs`
- Test: `crates/persisting-pchronicle/src/append_queue.rs`
- Test: `crates/persisting-pchronicle-cli/src/gateway_capture.rs`

**Interfaces:**
- Produces: `RawEventAppendOutcome::{Accepted, Full, Unavailable}`.
- Produces: `RawEventAppendSender::try_append(...) -> RawEventAppendOutcome`.
- Produces: `RawEventAppendSender::append_durable(...) -> anyhow::Result<RawEventAppendOutcome>`.
- Produces: `RawEventAppendWorker::finish() -> anyhow::Result<()>`.

- [ ] **Step 1: Write RED admission and source tests**

```rust
#[test]
fn full_and_closed_are_expected_append_outcomes() {
    let (tx, _rx) = mpsc::sync_channel(1);
    let state = Arc::new(SenderState {
        tx,
        accepting: AtomicBool::new(true),
        in_flight: AtomicUsize::new(0),
    });
    let sender = RawEventAppendSender { state: Arc::clone(&state) };
    let coords = StoryCoords::new("memory://queue", "agent", "session", None);
    assert_eq!(sender.try_append(coords.clone(), event()), RawEventAppendOutcome::Accepted);
    assert_eq!(sender.try_append(coords.clone(), event()), RawEventAppendOutcome::Full);
    state.accepting.store(false, Ordering::SeqCst);
    assert_eq!(sender.try_append(coords, event()), RawEventAppendOutcome::Unavailable);
}

#[test]
fn durable_storage_failure_keeps_its_source_chain() {
    let dir = tempfile::tempdir().unwrap();
    let invalid_storage = dir.path().join("not-a-directory");
    std::fs::write(&invalid_storage, b"file").unwrap();
    let coords = StoryCoords::new(invalid_storage.to_string_lossy(), "agent", "session", None);
    let (sender, worker) = raw_event_append_queue_with_capacity(1).unwrap();
    let error = sender.append_durable(coords, event()).unwrap_err();
    assert!(error.chain().count() >= 2, "missing source chain: {error:#}");
    assert!(worker.finish().is_err());
}
```

Retain burst batching, waiter ordering, dropped receiver, terminal failure, and worker panic regressions, but assert outcomes or source text rather than global codes/context fields.

- [ ] **Step 2: Run append queue tests and verify RED**

Run:

```bash
cargo test -p persisting-pchronicle append_queue --features lance-store --locked
```

Expected: compile failures because append methods still return the old queue error/result shapes.

- [ ] **Step 3: Implement outcome-based admission**

Map `TrySendError::Full` to `RawEventAppendOutcome::Full` and disconnected/closed state to `Unavailable`. Return `Accepted` after non-durable enqueue and only after durable visibility from `append_durable`. Send `anyhow::Result<()>` through completion channels. For a failed URI batch, move the original error to the first live waiter; give additional live waiters a fresh anyhow failure contextualized with the URI. Never stringify the original error to recreate it. Keep `finish` responsible only for worker/task failure.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawEventAppendOutcome {
    Accepted,
    Full,
    Unavailable,
}

impl RawEventAppendSender {
    pub fn try_append(&self, coords: StoryCoords, record: EventRecord) -> RawEventAppendOutcome {
        self.enqueue(coords, record, None)
    }

    pub fn append_durable(
        &self,
        coords: StoryCoords,
        record: EventRecord,
    ) -> anyhow::Result<RawEventAppendOutcome> {
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        match self.enqueue(coords, record, Some(completion_tx)) {
            RawEventAppendOutcome::Accepted => completion_rx
                .recv()
                .context("await raw event append completion")?
                .map(|()| RawEventAppendOutcome::Accepted),
            rejection => Ok(rejection),
        }
    }
}
```

- [ ] **Step 4: Update Gateway capture to match outcomes**

Map `Accepted` to capture success, `Full` to the Gateway capacity policy, and `Unavailable` to the Gateway availability policy. Propagate operational `Err` through `anyhow` unchanged. Queue startup returns `anyhow::Result` and never panics.

- [ ] **Step 5: Run queue and Gateway regressions**

Run:

```bash
cargo test -p persisting-pchronicle append_queue --features lance-store --locked
cargo test -p persisting-pchronicle-cli gateway_capture --locked
cargo test -p persisting-gateway --test agenticmd_bridge --locked
```

Expected: PASS. Capacity/availability are explicit decisions and storage failures preserve sources.

- [ ] **Step 6: Commit append and Gateway outcomes**

```bash
git add crates/persisting-pchronicle/src/append_queue.rs crates/persisting-pchronicle/src/storage.rs crates/persisting-pchronicle-cli/src/gateway_capture.rs crates/persisting-gateway/tests/agenticmd_bridge.rs
git commit -m "refactor: model append admission as an outcome"
```

### Task 5: Centralize seven public responses at transport boundaries

**Files:**
- Create: `crates/persisting-pchronicle-cli/src/server/problem.rs`
- Modify: `crates/persisting-pchronicle-cli/src/server/mod.rs`
- Modify: `crates/persisting-pchronicle-cli/src/server/tests.rs`
- Modify: `crates/persisting-pchronicle-cli/src/server/acceleration.rs`
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs`
- Modify: `crates/persisting-pchronicle-cli/src/exchange.rs`
- Modify: `crates/persisting-pchronicle-cli/src/tests.rs`
- Test: `crates/persisting-pchronicle-cli/src/server/tests.rs`

**Interfaces:**
- Produces: private `BoundaryCode` with exactly seven variants and snake-case serialization.
- Produces: private `ApiError { status, code, message }` with explicit constructors for expected outcomes and `ApiError::internal(anyhow::Error)`.
- Produces: private `QueryEvidenceWriteOutcome::{Complete(Vec<u8>), LimitExceeded}` from the writer-owned exhaustion state.
- Consumes: `InputIssueKind`, lookup `Option`, projection/append outcomes, and bounded writer state.

- [ ] **Step 1: Write the seven-code and no-classifier RED tests**

```rust
fn assert_problem(error: ApiError, status: StatusCode, code: BoundaryCode) {
    assert_eq!(error.status, status);
    assert_eq!(error.code, code);
}

async fn response_json(response: Response) -> serde_json::Value {
    use http_body_util::BodyExt as _;
    serde_json::from_slice(
        &response.into_body().collect().await.unwrap().to_bytes(),
    )
    .unwrap()
}

#[tokio::test]
async fn boundary_maps_explicit_results_and_redacts_failures() {
    assert_problem(ApiError::invalid_request("bad input"), StatusCode::BAD_REQUEST, BoundaryCode::InvalidRequest);
    assert_problem(ApiError::not_found("missing"), StatusCode::NOT_FOUND, BoundaryCode::NotFound);
    assert_problem(ApiError::conflict("stale"), StatusCode::CONFLICT, BoundaryCode::Conflict);
    assert_problem(ApiError::unsupported("format"), StatusCode::UNPROCESSABLE_ENTITY, BoundaryCode::Unsupported);
    assert_problem(ApiError::resource_exhausted("limit"), StatusCode::TOO_MANY_REQUESTS, BoundaryCode::ResourceExhausted);
    assert_problem(ApiError::unavailable(), StatusCode::SERVICE_UNAVAILABLE, BoundaryCode::Unavailable);

    let response = ApiError::internal(anyhow::anyhow!("/secret/backend"))
        .into_response();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = response_json(response).await;
    assert_eq!(body["code"], "internal");
    assert_eq!(body["message"], "internal server error");
    assert!(!body.to_string().contains("/secret/backend"));
}
```

Add a router-level test proving `None` maps to `not_found`, `InputIssueKind::Unsupported` maps to `unsupported`, and query byte exhaustion maps to `resource_exhausted` from `BoundedOutput::exhausted()` rather than error text.

- [ ] **Step 2: Run server tests and verify RED**

Run:

```bash
cargo test -p persisting-pchronicle-cli server::tests::boundary_ --locked
cargo test -p persisting-pchronicle-cli query_evidence_byte_budget --locked
```

Expected: compile or assertion failures because the explicit boundary module is absent and the server still classifies arbitrary error text.

- [ ] **Step 3: Implement the boundary module**

Define the seven variants, exact status table, fixed `service unavailable` and `internal server error` messages, and safe 4xx constructors. `ApiError::internal` records once with `tracing::error!(error = ?error, "pChronicle request failed")`; it does not traverse sources manually. Remove `classify_error`, typed-error lookup, diagnostics vectors, thread-local log capture, and all response creation from arbitrary `Display` values.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum BoundaryCode {
    InvalidRequest,
    NotFound,
    Conflict,
    Unsupported,
    ResourceExhausted,
    Unavailable,
    Internal,
}

#[derive(Debug, serde::Serialize)]
struct ApiError {
    #[serde(skip)]
    status: StatusCode,
    code: BoundaryCode,
    message: String,
}

impl ApiError {
    fn public(status: StatusCode, code: BoundaryCode, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into() }
    }

    fn invalid_request(message: impl Into<String>) -> Self {
        Self::public(StatusCode::BAD_REQUEST, BoundaryCode::InvalidRequest, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::public(StatusCode::NOT_FOUND, BoundaryCode::NotFound, message)
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::public(StatusCode::CONFLICT, BoundaryCode::Conflict, message)
    }

    fn unsupported(message: impl Into<String>) -> Self {
        Self::public(StatusCode::UNPROCESSABLE_ENTITY, BoundaryCode::Unsupported, message)
    }

    fn resource_exhausted(message: impl Into<String>) -> Self {
        Self::public(StatusCode::TOO_MANY_REQUESTS, BoundaryCode::ResourceExhausted, message)
    }

    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: BoundaryCode::Unavailable,
            message: "service unavailable".into(),
        }
    }

    fn internal(error: anyhow::Error) -> Self {
        tracing::error!(error = ?error, "pChronicle request failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: BoundaryCode::Internal,
            message: "internal server error".into(),
        }
    }
}
```

- [ ] **Step 4: Convert handlers and CLI commands to explicit results**

At each handler, map `Option` and Outcome directly to the appropriate constructor. Map `InputIssueKind` at import/query input entrypoints. Route every operational `Err` through `ApiError::internal`. Add `BoundedOutput::finish(self, write_result: anyhow::Result<()>) -> anyhow::Result<QueryEvidenceWriteOutcome>`: return `LimitExceeded` only when the writer-owned `exhausted` flag is true, return `Complete(bytes)` on success, and propagate ordinary writer errors. Match that outcome in `query_evidence`; never inspect error text. CLI commands may use `anyhow` for process reporting, but JSON protocol output must use the seven boundary labels.

```rust
enum QueryEvidenceWriteOutcome {
    Complete(Vec<u8>),
    LimitExceeded,
}

impl BoundedOutput {
    fn finish(
        self,
        write_result: anyhow::Result<()>,
    ) -> anyhow::Result<QueryEvidenceWriteOutcome> {
        match (write_result, self.exhausted) {
            (_, true) => Ok(QueryEvidenceWriteOutcome::LimitExceeded),
            (Ok(()), false) => Ok(QueryEvidenceWriteOutcome::Complete(self.bytes)),
            (Err(error), false) => Err(error),
        }
    }
}
```

- [ ] **Step 5: Run CLI and HTTP tests**

Run:

```bash
cargo test -p persisting-pchronicle-cli --locked
```

Expected: PASS. No 500/503 response leaks a source, and no boundary searches error text or source types.

- [ ] **Step 6: Commit transport boundaries**

```bash
git add crates/persisting-pchronicle-cli/src/server crates/persisting-pchronicle-cli/src/lib.rs crates/persisting-pchronicle-cli/src/exchange.rs crates/persisting-pchronicle-cli/src/tests.rs
git commit -m "refactor: map explicit outcomes at pchronicle boundaries"
```

### Task 6: Remove the legacy global error protocol and document the public façade

**Files:**
- Delete: `crates/persisting-pchronicle/src/error.rs`
- Modify: `crates/persisting-pchronicle/src/lib.rs`
- Modify: `crates/persisting-pchronicle/src/document.rs`
- Modify: `crates/persisting-pchronicle/src/model.rs`
- Modify: `crates/persisting-pchronicle/src/query.rs`
- Modify: `crates/persisting-pchronicle/src/storage.rs`
- Modify: `crates/persisting-pchronicle/src/agenticmd/codec.rs`
- Modify: `crates/persisting-pchronicle/src/agenticmd/convert.rs`
- Modify: `crates/persisting-pchronicle/src/agenticmd/fs.rs`
- Modify: `crates/persisting-pchronicle/src/agenticmd/validate.rs`
- Modify: `crates/persisting-pchronicle/src/atif.rs`
- Modify: `crates/persisting-pchronicle/src/convert/actf.rs`
- Modify: `crates/persisting-pchronicle/src/convert/atif.rs`
- Modify: `crates/persisting-pchronicle/src/convert/events.rs`
- Modify: `crates/persisting-pchronicle/src/format.rs`
- Modify: `crates/persisting-pchronicle/src/formats/actf.rs`
- Modify: `crates/persisting-pchronicle/src/formats/events.rs`
- Modify: `crates/persisting-pchronicle/src/formats/openai_corpus.rs`
- Modify: `crates/persisting-pchronicle/src/formats/storyline.rs`
- Modify: `crates/persisting-pchronicle/src/projection/storyline.rs`
- Modify: `crates/persisting-pchronicle/src/revision.rs`
- Modify: `crates/persisting-pchronicle/src/store/document_source.rs`
- Modify: `crates/persisting-pchronicle/src/store/event_row.rs`
- Modify: `crates/persisting-pchronicle/src/store/files/atif_reader.rs`
- Modify: `crates/persisting-pchronicle/src/store/files/atif_stream.rs`
- Modify: `crates/persisting-pchronicle/src/store/files/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/local_query_manifest.rs`
- Modify: `crates/persisting-pchronicle/src/store/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/model.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/mutation.rs`
- Modify: `crates/persisting-pchronicle/README.md`
- Modify: `crates/persisting-pchronicle/tests/public_api.rs`
- Modify: `docs/superpowers/specs/2026-08-18-pchronicle-unified-error-design.md`

**Interfaces:**
- Produces: public `Result<T> = anyhow::Result<T>` and document-local `InputIssue`/`InputResult<T>` only.
- Removes: `Error`, `ErrorCode`, `ErrorContext`, `ResultContext`, `classify_error`, and every propagation/classification helper.

- [ ] **Step 1: Add a failing static/public contract**

Update `tests/public_api.rs` so the only imported error-facing names are:

```rust
use persisting_pchronicle::document::{InputIssue, InputIssueKind, InputResult};
use persisting_pchronicle::Result;

fn accepts_anyhow<T>(result: anyhow::Result<T>) -> anyhow::Result<T> {
    result
}

#[test]
fn result_alias_is_anyhow() {
    let result: Result<()> = Ok(());
    let _: anyhow::Result<()> = accepts_anyhow(result);
}
```

Run this static scan and require no output:

```bash
rg -n 'ErrorCode|ErrorContext|ResultContext|classify_error|propagate_shared|with_dynamic_context|with_storage_context' crates/persisting-pchronicle/src crates/persisting-pchronicle-cli/src --glob '*.rs' --glob '!**/search/**'
```

Expected before deletion: matches from the legacy module/facades or transitional call sites.

- [ ] **Step 2: Delete the global protocol and clean exports**

Convert every remaining operational function in the listed files to `anyhow::Result`, replacing legacy `Error::*` construction with `anyhow::bail!`, `anyhow::ensure!`, direct `?`, or one ownership-boundary `.context(...)`. Keep parser decisions as `InputIssue`. Then delete `error.rs`; export the anyhow result alias from crate root and façade modules; export `InputIssue`, `InputIssueKind`, and `InputResult` only from `document`. Remove legacy constructors/tests. Update README examples to use `?`, `Option`, and explicit outcomes.

```rust
pub type Result<T> = anyhow::Result<T>;

// document.rs
pub use crate::input::{InputIssue, InputIssueKind, InputResult};
pub type Result<T> = anyhow::Result<T>;
```

- [ ] **Step 3: Verify the static and public contracts**

Run:

```bash
cargo test -p persisting-pchronicle --test public_api --locked
rg -n 'ErrorCode|ErrorContext|ResultContext|classify_error|propagate_shared|with_dynamic_context|with_storage_context' crates/persisting-pchronicle/src crates/persisting-pchronicle-cli/src --glob '*.rs' --glob '!**/search/**'
rg -n 'to_string\(\).*code|downcast_ref::<.*Error|contains\("(not found|conflict|unsupported|corrupt|io error)' crates/persisting-pchronicle/src crates/persisting-pchronicle-cli/src --glob '*.rs' --glob '!**/search/**'
```

Expected: public API test PASS; both scans produce no output.

- [ ] **Step 4: Commit façade cleanup**

```bash
git add crates/persisting-pchronicle docs/superpowers/specs/2026-08-18-pchronicle-unified-error-design.md
git commit -m "refactor: remove pchronicle global error protocol"
```

### Task 7: Full regression, lint, and executed-plan cleanup

**Files:**
- Modify: `docs/superpowers/specs/2026-08-18-pchronicle-unified-error-design.md` only when a verified public signature differs from the approved contract; update the signature and its matching test together
- Delete after all checks pass: `docs/superpowers/plans/2026-08-19-pchronicle-lightweight-errors.md`

**Interfaces:**
- Consumes: all prior tasks.
- Produces: a green pChronicle/CLI/Gateway branch with the executed plan removed and its durable technical contract retained in the design.

- [ ] **Step 1: Format and run targeted unit/integration suites**

```bash
cargo fmt --all -- --check
cargo test -p persisting-pchronicle --locked
cargo test -p persisting-pchronicle --features lance-store --locked
cargo test -p persisting-pchronicle-cli --locked
cargo test -p persisting-gateway --locked
```

Expected: all non-ignored tests PASS. Credentialed S3 and sustained stress tests may remain explicitly ignored.

- [ ] **Step 2: Run deny-warning Clippy**

```bash
cargo clippy -p persisting-pchronicle --all-targets --features lance-store --locked -- -D warnings
cargo clippy -p persisting-pchronicle-cli --all-targets --locked -- -D warnings
cargo clippy -p persisting-gateway --all-targets --locked -- -D warnings
```

Expected: PASS with no warnings from the modified crates.

- [ ] **Step 3: Run in-scope infrastructure regressions**

```bash
cargo test -p persisting-agentctl -p persisting-events --features persisting-events/control --locked
cargo test -p persisting-overlay-core -p persisting-overlayfs -p persisting-overlaynet --locked
cargo build -p persisting-pvisor --bin pvisor --locked
cargo test -p persisting-ppilot --locked
cargo test -p persisting-pvisor --locked
```

Expected: all non-environment-dependent tests PASS.

- [ ] **Step 4: Enforce the architectural burden limits**

```bash
test ! -e crates/persisting-pchronicle/src/error.rs
test ! -e crates/persisting-pchronicle/src/store/error_adapter.rs
rg -n 'ErrorCode|ErrorContext|ResultContext|classify_error|propagate_shared|with_dynamic_context|with_storage_context' crates/persisting-pchronicle/src crates/persisting-pchronicle-cli/src --glob '*.rs' --glob '!**/search/**'
rg -n 'to_string\(\).*code|downcast_ref::<.*Error|contains\("(not found|conflict|unsupported|corrupt|io error)' crates/persisting-pchronicle/src crates/persisting-pchronicle-cli/src --glob '*.rs' --glob '!**/search/**'
git diff --check
```

Expected: both files absent, both scans empty, and diff check PASS. Verify the required bridge directly:

```bash
test "$(wc -l < crates/persisting-pchronicle/src/store/datafusion_bridge.rs)" -le 100
! rg -n 'BoundaryCode|invalid_request|not_found|conflict|unsupported|resource_exhausted|unavailable|internal' crates/persisting-pchronicle/src/store/datafusion_bridge.rs
```

- [ ] **Step 5: Delete the executed plan and commit final verification artifacts**

After every command above passes, delete this plan with `apply_patch`. Confirm the design already contains every durable technical invariant, then commit only the final documentation/test adjustments:

```bash
git add docs/superpowers/plans/2026-08-19-pchronicle-lightweight-errors.md docs/superpowers/specs/2026-08-18-pchronicle-unified-error-design.md
git commit -m "docs: finalize pchronicle lightweight error contract"
```

Do not stage or commit the pre-existing five plan deletions, five design edits, `.workbuddy/`, the Vortex RFC, or the named archival stash.

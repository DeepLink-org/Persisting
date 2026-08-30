# Serve + Web Error Logging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One failed Warehouse request is correlatable across API JSON, the Web banner, and `pchronicle serve` stderr; INFO logs cover startup and queries; 5xx logs expose `root_cause` while JSON stays redacted.

**Architecture:** Axum middleware assigns `request_id`, logs API completions, and injects the id into error JSON. `ApiError::internal(request_id, handler, error)` logs `root_cause` + `chain` at ERROR with target `pchronicle.serve`. The Web client parses the same JSON into `ApiFailure` and maps `code` to banner copy. Tracing is initialized only in `run_serve`.

**Tech Stack:** Rust, axum, tracing + tracing-subscriber, anyhow, Dioxus WASM (`pchronicle-web`), `just test persisting-pchronicle-cli`

**Spec:** `docs/superpowers/specs/2026-08-30-pchronicle-serve-web-error-logging-design.md`

## Global Constraints

- Warehouse HTTP + `pchronicle-web` only; do not change Gateway or Control logging.
- Do not add OpenTelemetry or `tower-http` TraceLayer.
- 5xx JSON `message` is always `internal server error`; never put the anyhow chain or SQL in the 5xx body.
- Every warehouse `tracing::*` event in this work uses `target: "pchronicle.serve"` so `--log-level` can filter `pchronicle.serve=info` without DataFusion noise.
- Default validation: `just test persisting-pchronicle-cli`. Web: `cargo test --bin pchronicle-web` from `pchronicle-web/`.
- Do not touch TTAS, Queue, Search, or `persisting-dlcapt`.
- Commits only when the human asks; skip Task commit steps unless they explicitly requested git commits.

---

### Task 1: Request id, truncation, and 5xx root-cause logging

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/server/problem.rs`
- Modify: `crates/persisting-pchronicle-cli/src/server/tests.rs` (extend `boundary_maps_explicit_results_and_redacts_failures`)
- Modify: `crates/persisting-pchronicle-cli/src/server/mod.rs` (re-export helpers if tests need them)

**Interfaces:**
- Consumes: existing `BoundaryCode`, `ApiError`
- Produces: `truncate_utf8(input, max_bytes) -> String`; `new_request_id() -> String`; `parse_incoming_request_id(header) -> Option<String>`; `ApiError` gains `request_id: String`; `ApiError::internal(request_id, handler, error)`; `ApiError::with_request_id(self, id)`

- [ ] **Step 1: Write the failing tests** in `crates/persisting-pchronicle-cli/src/server/tests.rs`

Add (keep the existing redaction test, update it to pass a request_id):

```rust
#[test]
fn truncate_utf8_does_not_split_characters_and_marks_overflow() {
    assert_eq!(super::problem::truncate_utf8("abcd", 4), "abcd");
    assert_eq!(super::problem::truncate_utf8("abcdef", 4), "abcd…");
    assert_eq!(super::problem::truncate_utf8("验证中文", 3), "验…");
}

#[test]
fn incoming_request_id_rejects_blank_and_overlong() {
    assert_eq!(super::problem::parse_incoming_request_id("abc-1"), Some("abc-1".into()));
    assert_eq!(super::problem::parse_incoming_request_id("has space"), None);
    assert_eq!(super::problem::parse_incoming_request_id(&"a".repeat(65)), None);
    assert_eq!(super::problem::parse_incoming_request_id(""), None);
}

#[tokio::test]
async fn internal_error_logs_root_cause_and_redacts_json() {
    use super::acceleration::tests::RecordingSubscriber; // if private, copy the subscriber into tests.rs instead
    // Prefer a local RecordingSubscriber in tests.rs matching acceleration.rs:1380
}
```

If `RecordingSubscriber` is private to `acceleration.rs`, copy the small subscriber (already at `acceleration.rs` ~1378–1435) into `server/tests.rs` as `struct CapturingSubscriber`. Then:

```rust
#[tokio::test]
async fn internal_error_logs_root_cause_and_redacts_json() {
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<CapturedEvent>::new()));
    let _guard = tracing::subscriber::set_default(CapturingSubscriber::new(events.clone()));
    let cause = anyhow::anyhow!("disk-sentinel").context("open table");
    let response = ApiError::internal("rid-internal-1", "query_evidence", cause).into_response();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = response_json(response).await;
    assert_eq!(body["code"], "internal");
    assert_eq!(body["message"], "internal server error");
    assert_eq!(body["request_id"], "rid-internal-1");
    assert!(!body.to_string().contains("disk-sentinel"));

    let logged = events.lock().unwrap().clone();
    let error_events: Vec<_> = logged
        .into_iter()
        .filter(|event| event.level == tracing::Level::ERROR)
        .collect();
    assert_eq!(error_events.len(), 1);
    assert!(error_events[0].message.contains("warehouse request failed"));
    assert!(!error_events[0].message.contains("internal server error"));
    assert_eq!(error_events[0].fields.get("root_cause").map(String::as_str), Some("disk-sentinel"));
    assert_eq!(error_events[0].fields.get("request_id").map(String::as_str), Some("rid-internal-1"));
    assert_eq!(error_events[0].fields.get("handler").map(String::as_str), Some("query_evidence"));
    assert!(error_events[0].fields.get("chain").unwrap().contains("open table"));
}
```

Update `boundary_maps_explicit_results_and_redacts_failures` so `ApiError::internal` is called with `("rid", "test", anyhow!(...))` and the JSON still redacts `/secret/backend` **and** includes `"request_id":"rid"`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `just test persisting-pchronicle-cli`

Expected: compile error (`truncate_utf8` / `internal` arity) or assertion failure on missing `request_id` / `root_cause`.

- [ ] **Step 3: Implement helpers and `ApiError` in `problem.rs`**

```rust
pub(crate) const LOG_TARGET: &str = "pchronicle.serve";
pub(crate) const QUERY_LOG_LIMIT: usize = 512;
pub(crate) const ROOT_CAUSE_LIMIT: usize = 512;
pub(crate) const CHAIN_LIMIT: usize = 2048;

pub(crate) fn truncate_utf8(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    let end = input
        .char_indices()
        .map(|(i, ch)| i + ch.len_utf8())
        .take_while(|end| *end <= max_bytes)
        .last()
        .unwrap_or(0);
    format!("{}…", &input[..end])
}

pub(crate) fn new_request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_owned()
}

pub(crate) fn parse_incoming_request_id(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 64
        || value.bytes().any(|b| b <= 0x20 || b >= 0x7f)
    {
        return None;
    }
    Some(value.to_owned())
}

#[derive(Debug, Serialize)]
pub(super) struct ApiError {
    #[serde(skip)]
    pub(super) status: StatusCode,
    pub(super) code: BoundaryCode,
    message: String,
    request_id: String,
}

impl ApiError {
    fn public(status: StatusCode, code: BoundaryCode, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            request_id: String::new(),
        }
    }

    pub(super) fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = request_id.into();
        self
    }

    pub(super) fn internal(
        request_id: impl Into<String>,
        handler: &'static str,
        error: anyhow::Error,
    ) -> Self {
        let request_id = request_id.into();
        let root_cause = truncate_utf8(&error.root_cause().to_string(), ROOT_CAUSE_LIMIT);
        let chain = truncate_utf8(&format!("{error:#}"), CHAIN_LIMIT);
        tracing::error!(
            target: LOG_TARGET,
            request_id = %request_id,
            code = "internal",
            handler,
            root_cause = %root_cause,
            chain = %chain,
            "warehouse request failed"
        );
        Self::public(
            StatusCode::INTERNAL_SERVER_ERROR,
            BoundaryCode::Internal,
            "internal server error",
        )
        .with_request_id(request_id)
    }
}
```

Keep `invalid_request` / `not_found` / etc. as today (empty `request_id` until middleware or `.with_request_id`). Update `acceleration.rs` test that calls `ApiError::internal(...)` to the new arity.

- [ ] **Step 4: Run tests**

Run: `just test persisting-pchronicle-cli`

Expected: PASS for the new tests and existing `boundary_maps_*` / acceleration tests.

- [ ] **Step 5: Commit** (only if the human asked)

```bash
git add crates/persisting-pchronicle-cli/src/server/problem.rs crates/persisting-pchronicle-cli/src/server/tests.rs crates/persisting-pchronicle-cli/src/server/acceleration.rs
git commit -m "$(cat <<'EOF'
Log Warehouse 5xx root causes with a correlatable request_id.

EOF
)"
```

---

### Task 2: Request middleware (id header, JSON inject, INFO/WARN)

**Files:**
- Create: `crates/persisting-pchronicle-cli/src/server/request_log.rs`
- Modify: `crates/persisting-pchronicle-cli/src/server/mod.rs` (`mod request_log`; `read_routes` layer)
- Modify: `crates/persisting-pchronicle-cli/src/server/tests.rs`

**Interfaces:**
- Consumes: `new_request_id`, `parse_incoming_request_id`, `truncate_utf8`, `QUERY_LOG_LIMIT`, `LOG_TARGET`
- Produces: `RequestId(pub String)` axum extractor; `warehouse_request_layer`; `FtsDiagnostics(Vec<String>)` request extension

- [ ] **Step 1: Write failing tests** using `tower::ServiceExt::oneshot` on `warehouse_router` or a tiny router with the layer + a handler that returns `ApiError::invalid_request("bad input")`.

```rust
#[tokio::test]
async fn middleware_echoes_request_id_on_json_errors() {
    async fn boom() -> Result<(), ApiError> {
        Err(ApiError::invalid_request("bad input"))
    }
    let app = axum::Router::new()
        .route("/api/boom", axum::routing::get(boom))
        .layer(axum::middleware::from_fn(crate::server::request_log::warehouse_request_layer));
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/boom")
                .header("x-request-id", "client-id-123")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.headers().get("x-request-id").unwrap(),
        "client-id-123"
    );
    let body = response_json(response).await;
    assert_eq!(body["request_id"], "client-id-123");
    assert_eq!(body["code"], "invalid_request");
}

#[tokio::test]
async fn middleware_rejects_illegal_incoming_id() {
    // header "bad id" (space) → generated 16 hex, not the illegal value
}

#[tokio::test]
async fn middleware_does_not_info_log_static_assets() {
    // GET /assets/app.css through a router with the layer + fallback 404
    // CapturingSubscriber must record no INFO with path=/assets/app.css
}
```

Need `use tower::ServiceExt` and `Router` without AppState for the boom route.

- [ ] **Step 2: Run tests expecting FAIL**

Run: `just test persisting-pchronicle-cli`

Expected: `request_log` module missing.

- [ ] **Step 3: Implement `request_log.rs`**

```rust
#[derive(Clone, Debug)]
pub(crate) struct RequestId(pub String);

#[derive(Clone, Default)]
pub(crate) struct FtsDiagnostics(pub Vec<String>);

impl RequestId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[async_trait::async_trait]
impl<S> axum::extract::FromRequestParts<S> for RequestId
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(parts
            .extensions
            .get::<RequestId>()
            .cloned()
            .unwrap_or_else(|| RequestId(String::new())))
    }
}
```

The crate may not have `async_trait` — axum 0.8 `FromRequestParts` is a native async trait. Implement without `async_trait` if the axum version in workspace supports it (match neighboring extractors).

Middleware outline:

1. Read `x-request-id`; `parse_incoming_request_id` or `new_request_id()`.
2. Insert `RequestId` into extensions; `Instant::now()`.
3. `let mut response = next.run(request).await`.
4. Insert response header `x-request-id`.
5. If content-type contains `json` and status >= 400, buffer body, `serde_json::Value` object, `as_object_mut().insert("request_id", ...)`, rebuild body. Preserve status.
6. `path` from URI. `is_api = path.starts_with("/api/")`.
7. If `is_api`: `tracing::info!(target: LOG_TARGET, request_id, method, path, status, elapsed_ms, query = truncate_utf8(uri.query().unwrap_or(""), 512), "warehouse request")`.
8. If `is_api && 400 <= status < 500`: parse JSON for `code`/`message`; `tracing::warn!(..., fts_errors = joined FtsDiagnostics if present, root_cause if message != root of anyhow — skip root_cause on 4xx unless FtsDiagnostics non-empty)`. Spec: 4xx WARN includes public message; attach `fts_errors` from extension (copy extension onto request before `next.run` by using a clone held in the middleware). **FtsDiagnostics must be inserted by the handler onto the request extensions — handlers run inside `next`, so store FtsDiagnostics on a `Arc<Mutex<Vec<String>>>` placed in extensions before `next.run`, handlers push into it.**

Use:

```rust
#[derive(Clone, Default)]
pub(crate) struct FtsDiagnostics(pub Arc<Mutex<Vec<String>>>);
```

Middleware inserts default empty slot before `next.run`. Handler does `fts.0.lock().unwrap().extend(...)`. WARN joins with `"; "` and `truncate_utf8(..., 512)`.

9. Do not INFO-log when `!is_api`.

Wire in `read_routes()`:

```rust
fn read_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .nest("/api", api_routes())
        .nest("/api/v1", api_routes())
        .fallback(asset_fallback)
        .layer(axum::middleware::from_fn(request_log::warehouse_request_layer))
}
```

Also apply the same layer in `PreparedWarehouse::router` because it uses `read_routes()` — one change covers both.

Check `Cargo.toml`: `tower` is dev-dep; oneshot tests need `tower` with `util`. Keep using `tower` as dev-dependency. If `from_fn` needs `axum` only, fine.

`async_trait`: do not add a new crate if axum's trait is inline.

- [ ] **Step 4: Run tests**

Run: `just test persisting-pchronicle-cli`

Expected: PASS including middleware tests and existing server tests.

- [ ] **Step 5: Commit** (only if asked)

---

### Task 3: Handler wiring, FTS diagnostics, query INFO, physical mapping

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/server/mod.rs`
- Modify: `crates/persisting-pchronicle-cli/src/server/physical.rs`
- Modify: `crates/persisting-pchronicle-cli/src/server/acceleration.rs` (ERROR target + root_cause)
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs` only if `CliBoundaryError` must be `pub(crate)` for downcast from `problem.rs` — **move `CliBoundaryError` into `problem.rs`** and keep `cli_boundary_error` in `lib.rs` constructing it.

**Interfaces:**
- Consumes: `RequestId`, `FtsDiagnostics`, `ApiError::internal(id, handler, err)`, `ApiError::with_request_id`
- Produces: every Warehouse 500 path passes `RequestId`; `query_evidence` / `compile_analysis` extra INFO; FTS warnings via extension

- [ ] **Step 1: Write failing tests**

```rust
#[tokio::test]
async fn query_evidence_info_truncates_sql() {
    // CapturingSubscriber; oneshot POST /api/query/evidence with sql of 600 'a'
    // plus a valid FROM clause if the handler validates SQL first.
    // Assert an INFO event field `sql` ends with `…` and len <= 512 + 3.
}

#[test]
fn map_inspect_unknown_errors_are_internal() {
    let error = ApiError::from_anyhow("rid", "physical_page", anyhow::anyhow!("lance exploded"));
    assert_eq!(error.code, BoundaryCode::Internal);
}
```

Add `ApiError::from_anyhow(request_id, handler, error)` in this task's implementation (tested here). For FTS: if an existing server test already hits `FTS unavailable` as 400, keep it; add assertion `request_id` present.

- [ ] **Step 2: Run tests expecting FAIL**

- [ ] **Step 3: Implement `from_anyhow` and rewire handlers**

```rust
impl ApiError {
    pub(super) fn from_anyhow(
        request_id: &str,
        handler: &'static str,
        error: anyhow::Error,
    ) -> Self {
        if let Some(boundary) = error.downcast_ref::<CliBoundaryError>() {
            return Self::from_boundary(request_id, boundary.code, boundary.message.clone());
        }
        let message = format!("{error:#}");
        if message.contains("FTS unavailable") {
            return Self::invalid_request(message).with_request_id(request_id);
        }
        Self::internal(request_id, handler, error)
    }
}
```

Spec forbids *new* `contains("not found")` in physical mapping. The FTS `contains("FTS unavailable")` already exists in `explorer_runs`; keep that one exact phrase check, do not add more substring classifiers.

`from_boundary`:

```rust
fn from_boundary(request_id: &str, code: BoundaryCode, message: String) -> Self {
    match code {
        BoundaryCode::InvalidRequest => Self::invalid_request(message).with_request_id(request_id),
        BoundaryCode::NotFound => Self::not_found(message).with_request_id(request_id),
        // ... same for Conflict, Unsupported, ResourceExhausted, Unavailable
        BoundaryCode::Internal => Self::internal(request_id, "boundary", anyhow::anyhow!(message)),
    }
}
```

Move `CliBoundaryError` to `problem.rs` as `pub(crate) struct CliBoundaryError { pub code, pub message }` with Display + Error. `lib.rs` `cli_boundary_error` constructs `anyhow::Error::new(CliBoundaryError { ... })`. `error_code` / `error_exit_code` downcast the same type.

Replace every `map_err(ApiError::internal)` in `server/mod.rs` and `physical.rs` with:

```rust
.map_err(|error| ApiError::from_anyhow(request_id.as_str(), "explorer_runs", error))
```

Add `request_id: RequestId` extractor to those handlers. `current_catalog` currently returns `ApiError` without an id — change to:

```rust
async fn current_catalog(state: &AppState, request_id: &RequestId) -> Result<Arc<CatalogRuntime>, ApiError>
```

and pass `request_id` from callers. Catalog refresh WARN:

```rust
tracing::warn!(
    target: LOG_TARGET,
    root_cause = %truncate_utf8(&error.root_cause().to_string(), ROOT_CAUSE_LIMIT),
    chain = %truncate_utf8(&format!("{error:#}"), CHAIN_LIMIT),
    "automatic Catalog refresh failed; retaining the last valid snapshot"
);
```

`query_evidence`: after validating SQL, before execute:

```rust
tracing::info!(
    target: LOG_TARGET,
    request_id = %request_id.as_str(),
    sql = %truncate_utf8(&request.sql, QUERY_LOG_LIMIT),
    "warehouse query"
);
```

`compile_analysis`: INFO with truncated spec question or compiled SQL (the handler has `request.spec` — log truncated `serde_json::to_string(&request.spec).unwrap_or_default()` or the compiled SQL after compile; spec says SQL or question text — log the compiled SQL string when present, else the spec question field if any).

FTS: in `explorer_runs` / `explorer_turns`, push `fts_errors` into `FtsDiagnostics` instead of `tracing::debug!`.

`physical.rs` `map_inspect(request_id, error)` → `ApiError::from_anyhow(request_id, "physical_page", error)` (drop `contains("not found")`). If inspect already returns a `CliBoundaryError` for missing sources, 404 still works; if not, missing sources become 500 + logged root_cause. Check `inspect_physical_*` return values; if they return anyhow with a stable `physical source not found` **root_cause**, you may map only via `error.root_cause().to_string() == "physical source not found"` (exact, not contains). Same for `"not a Lance dataset"` → `invalid_request`. Document those two exact root_cause strings in a comment.

Acceleration index failure at `acceleration.rs:316`:

```rust
tracing::error!(
    target: crate::server::problem::LOG_TARGET,
    acceleration_index = name,
    root_cause = %truncate_utf8(&error.root_cause().to_string(), ROOT_CAUSE_LIMIT),
    chain = %truncate_utf8(&format!("{error:#}"), CHAIN_LIMIT),
    "pChronicle acceleration index build failed"
);
```

- [ ] **Step 4: Run tests**

Run: `just test persisting-pchronicle-cli`

Expected: PASS.

- [ ] **Step 5: Commit** (only if asked)

---

### Task 4: Install tracing in `run_serve` and startup INFO

**Files:**
- Modify: `crates/persisting-pchronicle-cli/Cargo.toml` (move `tracing-subscriber` from `[dev-dependencies]` to `[dependencies]`; keep it in dev-deps or just one listing)
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs` (`run_serve`, `LogLevel` mapping)
- Modify: `crates/persisting-pchronicle-cli/src/server/mod.rs` or `request_log.rs` (`init_warehouse_tracing`, `log_warehouse_startup`)
- Modify: `crates/persisting-pchronicle-cli/src/tests.rs` if serve tests capture stderr

**Interfaces:**
- Consumes: `Cli.log_level`, `PreparedWarehouse` snapshot id, listen addr, dataset names
- Produces: `init_warehouse_tracing(LogLevel)` using `try_init`; startup INFO

- [ ] **Step 1: Write a unit test for the filter string**

```rust
#[test]
fn warehouse_tracing_filter_matches_log_level() {
    assert_eq!(
        crate::server::request_log::tracing_filter(LogLevel::Info),
        "pchronicle.serve=info"
    );
    assert_eq!(
        crate::server::request_log::tracing_filter(LogLevel::Error),
        "pchronicle.serve=error"
    );
}
```

- [ ] **Step 2: Run expecting FAIL**

- [ ] **Step 3: Implement**

```rust
pub(crate) fn tracing_filter(level: crate::LogLevel) -> String {
    let level = match level {
        crate::LogLevel::Error => "error",
        crate::LogLevel::Warn => "warn",
        crate::LogLevel::Info => "info",
        crate::LogLevel::Debug => "debug",
    };
    format!("pchronicle.serve={level}")
}

pub(crate) fn init_warehouse_tracing(level: crate::LogLevel) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::new(tracing_filter(level)),
        )
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();
}
```

Do **not** call `EnvFilter::from_default_env()` (spec: ignore `RUST_LOG`).

In `run_with_stdio`, pass `cli.log_level` into `run_serve`. At the start of `run_serve`: `server::request_log::init_warehouse_tracing(log_level)`.

After Warehouse listener is bound and `PreparedWarehouse::prepare` succeeded, log:

```rust
tracing::info!(
    target: LOG_TARGET,
    listen = %addr,
    datasets = %names.join(","),
    snapshot_id = %snapshot_id,
    "warehouse listening"
);
```

`snapshot_id` from `warehouse.current_snapshot_id()` — that method is `#[cfg(test)]` today. Add `pub(crate) async fn snapshot_id(&self) -> Option<String>` (non-test) duplicating the body of `current_snapshot_id`, and keep the test wrapper or just drop `cfg(test)` from the existing method.

Update clap help on `log_level` in `lib.rs`:

```rust
/// Control stderr diagnostics without changing command results. For serve, also filters Warehouse request logs (target pchronicle.serve).
```

- [ ] **Step 4: Run tests**

Run: `just test persisting-pchronicle-cli`

Expected: PASS. `try_init` must not break tests that already install a subscriber.

- [ ] **Step 5: Commit** (only if asked)

---

### Task 5: Web `ApiFailure` parser

**Files:**
- Modify: `pchronicle-web/src/api.rs`
- Modify: `pchronicle-web/src/analysis_session.rs` (`CompileFailure.request_id`)

**Interfaces:**
- Consumes: error JSON `{code, message, request_id, field?, engine_detail?}`
- Produces: `ApiFailure`; `checked() -> Result<Response, ApiFailure>`; `api` functions return `Result<T, ApiFailure>` except `compile_analysis` which stays `Result<T, CompileFailure>` but fills `request_id`

- [ ] **Step 1: Write failing tests at the bottom of `api.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_api_failure_reads_code_and_request_id() {
        let failure = parse_api_failure(400, r#"{"code":"resource_exhausted","message":"limit","request_id":"abc123"}"#);
        assert_eq!(failure.code, "resource_exhausted");
        assert_eq!(failure.message, "limit");
        assert_eq!(failure.request_id.as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_api_failure_falls_back_for_non_json() {
        let failure = parse_api_failure(500, "not-json");
        assert_eq!(failure.code, "");
        assert_eq!(failure.message, "HTTP 500: not-json");
        assert!(failure.request_id.is_none());
    }

    #[test]
    fn network_failure_is_unavailable() {
        let failure = ApiFailure::network("connection refused");
        assert_eq!(failure.code, "unavailable");
        assert!(failure.request_id.is_none());
    }
}
```

- [ ] **Step 2: Run expecting FAIL**

From `pchronicle-web/`: `cargo test --bin pchronicle-web -- parse_api_failure`

Expected: `parse_api_failure` missing.

- [ ] **Step 3: Implement**

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct ApiFailure {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub request_id: Option<String>,
    pub field: Option<String>,
    pub engine_detail: Option<String>,
    pub raw: String,
}

impl ApiFailure {
    pub fn network(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            status: 0,
            code: "unavailable".into(),
            message: message.clone(),
            request_id: None,
            field: None,
            engine_detail: None,
            raw: message,
        }
    }
}

impl std::fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.request_id.as_deref() {
            Some(id) => write!(f, "{} (request_id={id})", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

pub(crate) fn parse_api_failure(status: u16, body: &str) -> ApiFailure {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        let code = value.get("code").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let message = value
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or(body)
            .to_string();
        let request_id = value
            .get("request_id")
            .and_then(|v| v.as_str())
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        let field = value.get("field").and_then(|v| v.as_str()).map(str::to_owned);
        let engine_detail = value
            .get("engine_detail")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        return ApiFailure {
            status,
            code,
            message,
            request_id,
            field,
            engine_detail,
            raw: body.to_owned(),
        };
    }
    ApiFailure {
        status,
        code: String::new(),
        message: format!("HTTP {status}: {body}"),
        request_id: None,
        field: None,
        engine_detail: None,
        raw: body.to_owned(),
    }
}
```

Change `checked` to return `Result<Response, ApiFailure>`. Map `.send().map_err` to `ApiFailure::network`. Change all `api.rs` exports from `Result<T, String>` to `Result<T, ApiFailure>`.

`compile_analysis`: on error JSON, deserialize `CompileFailure` (with new optional `request_id`) or `parse_api_failure` then:

```rust
CompileFailure {
    code: failure.code,
    message: failure.message,
    field: failure.field,
    engine_detail: failure.engine_detail,
    request_id: failure.request_id,
}
```

Add to `CompileFailure`:

```rust
#[serde(default)]
pub request_id: Option<String>,
```

Fix compile sites that construct `CompileFailure` without `request_id` (`None`).

This will break `workspace.rs` / `physical.rs` / `agent.rs` until Task 6. If the crate does not compile, do Task 6 in the same working tree before considering Task 5 done — but keep tests for parse in this task.

- [ ] **Step 4: Run parse tests**

`cargo test --bin pchronicle-web -- parse_api_failure network_failure`

Expected: those tests PASS. Full bin may still fail to compile until Task 6.

- [ ] **Step 5: Commit** (only if asked; prefer after Task 6 if compile is broken)

---

### Task 6: Web banners, Physical, Analysis, Assistant

**Files:**
- Modify: `pchronicle-web/src/workspace.rs`
- Modify: `pchronicle-web/src/physical.rs`
- Modify: `pchronicle-web/src/analysis.rs` (analyze-error panel: show `request_id`)
- Modify: `pchronicle-web/src/agent.rs` (`query_sql` / `get_turn` use `Display` of `ApiFailure`)
- Modify: `pchronicle-web/assets/components.css` only if request-id row needs a class (reuse existing notice details)

**Interfaces:**
- Consumes: `ApiFailure`, `CompileFailure.request_id`
- Produces: `workspace_notice(failure: &ApiFailure) -> WorkspaceNotice`

- [ ] **Step 1: Write failing tests in `workspace.rs`**

```rust
#[test]
fn internal_notice_uses_request_id_and_hides_secret() {
    let failure = crate::api::parse_api_failure(
        500,
        r#"{"code":"internal","message":"internal server error","request_id":"deadbeefdeadbeef"}"#,
    );
    let notice = workspace_notice(&failure);
    assert_eq!(notice.title, "Something went wrong");
    assert!(notice.summary.contains("deadbeefdeadbeef") || notice.request_id.as_deref() == Some("deadbeefdeadbeef"));
    assert!(!notice.detail.contains("secret"));
}

#[test]
fn resource_exhausted_notice_asks_to_narrow_the_query() {
    let failure = crate::api::parse_api_failure(
        429,
        r#"{"code":"resource_exhausted","message":"find result exceeds row limit of 51","request_id":"rid"}"#,
    );
    let notice = workspace_notice(&failure);
    assert_eq!(notice.title, "The result is too large");
    assert!(notice.action.contains("Narrow the query"));
}
```

Extend `WorkspaceNotice`:

```rust
struct WorkspaceNotice {
    title: String,
    summary: String,
    action: String,
    detail: String,
    request_id: Option<String>,
    turn_id: Option<i64>,
}
```

- [ ] **Step 2: Run expecting FAIL**

- [ ] **Step 3: Implement mapping and UI**

```rust
fn workspace_notice(failure: &crate::api::ApiFailure) -> WorkspaceNotice {
    let (title, action) = match failure.code.as_str() {
        "invalid_request" => ("This request isn't valid", String::new()),
        "not_found" => ("Nothing matched", String::new()),
        "conflict" => (
            "This view is out of date",
            "Refresh the catalog and try again".into(),
        ),
        "unsupported" | "unplannable" => ("This isn't supported", String::new()),
        "resource_exhausted" => (
            "The result is too large",
            "Narrow the query or lower the row limit".into(),
        ),
        "unavailable" => (
            "The server isn't reachable",
            "Check that pchronicle serve is still running".into(),
        ),
        "internal" => (
            "Something went wrong",
            "The server log for this request ID has the cause".into(),
        ),
        _ => ("Request failed", String::new()),
    };
    let summary = if failure.message.is_empty() {
        title.to_string()
    } else {
        failure.message.clone()
    };
    WorkspaceNotice {
        title: title.into(),
        summary,
        action,
        detail: failure.raw.clone(),
        request_id: failure.request_id.clone(),
        turn_id: None,
    }
}
```

Banner rsx: show `notice.action` if non-empty; if `request_id` is some, show a copyable `code {id}` (existing `<pre>` in details plus a visible `Request ID` line). Do not put 5xx chain in summary.

Replace every `workspace_notice(message)` where `message: String` with `workspace_notice(&failure)`. `evidence_notice` stays for decode errors (not HTTP).

Physical: `error: Signal<Option<ApiFailure>>` or convert to `WorkspaceNotice` and render the same markup as workspace (duplicate the notice block or extract a tiny `ErrorNotice` component in `workspace.rs` / `components.rs`). Minimal path: store `Option<WorkspaceNotice>` built via a `pub(crate) fn notice_from_failure` moved to `api.rs` next to `ApiFailure` if `workspace.rs` helpers are private — **keep `workspace_notice` in `workspace.rs` and a `pub(crate)` copy in `api.rs` as `failure_notice_copy` used by tests; physical.rs calls `crate::api::banner_title` or export `workspace_notice` from a new `pchronicle-web/src/notice.rs`.**

Create `pchronicle-web/src/notice.rs` with `WorkspaceNotice`, `workspace_notice`, and the title table so workspace + physical share it. `main.rs` / `workspace.rs` `mod notice`. This avoids circular imports.

Analysis PlanError / SQL error panels: if the string came from compile, show `revision` error plus `request_id` when `CompileFailure` is stored. Trace where compile errors are stored; if only a String is kept, append `request_id` into that string via `CompileFailure::summary` including `request_id`.

```rust
impl CompileFailure {
    pub fn summary(&self) -> String {
        let mut text = match self.field.as_deref() {
            Some(field) => format!("{}: {}", field, self.message),
            None => self.message.clone(),
        };
        if let Some(id) = &self.request_id {
            text.push_str(" [request_id=");
            text.push_str(id);
            text.push(']');
        }
        text
    }
}
```

Agent:

```rust
Err(error) => format!("query_sql failed: {error}"),
```

already works if `ApiFailure: Display` includes `request_id`.

- [ ] **Step 4: Run tests**

From `pchronicle-web/`: `cargo test --bin pchronicle-web`

Expected: PASS (including the pre-existing `plan_prompt_sends_only_approved_catalog_and_scope_context` if still failing, **do not fix catalog `kind` in this plan** unless it blocks compile). If that test still fails for `kind`, leave it; this task is green when *new* tests pass and the crate compiles.

- [ ] **Step 5: Commit** (only if asked)

---

### Task 7: Serve guide + CLI `--log-level` docs

**Files:**
- Modify: `docs/src/pchronicle/guides/serve.md`
- Modify: `docs/src/pchronicle/guides/serve.zh.md`
- Modify: `docs/src/pchronicle/reference/cli.md` (global `--log-level` paragraph)
- Modify: `docs/src/pchronicle/reference/cli.zh.md`

**Interfaces:** none

- [ ] **Step 1: Add a section (no TDD)** after the listen/open sections in `serve.md`:

```markdown
## Logs and failed requests

`pchronicle serve` writes Warehouse request logs to stderr at `--log-level`
(default `info`), tracing target `pchronicle.serve`. Startup logs the listen
address, Dataset names, and catalog snapshot. Each `/api` request logs method,
path, status, elapsed time, and a truncated query string. Query and compile
handlers also log truncated SQL.

Failed responses include `code`, `message`, and `request_id`. The Web banner
shows the same `request_id`. Internal failures redact details in JSON; the
stderr ERROR line has `root_cause` and `chain` for that id.

`--log-level error` keeps only internal failures. `--log-level` does not read
`RUST_LOG`.
```

Chinese equivalent in `serve.zh.md`. In `cli.md` replace “stderr diagnostics without changing stdout” with a sentence that `serve` uses the same flag for Warehouse tracing (`pchronicle.serve`).

- [ ] **Step 2: Commit** (only if asked)

---

## Spec coverage

| Spec section | Task |
|---|---|
| JSON `code`/`message`/`request_id` | 1, 2 |
| 5xx redact + ERROR `root_cause`/`chain`/`handler` | 1, 3 |
| Middleware header + JSON inject + INFO/WARN | 2 |
| Skip static INFO | 2 |
| Incoming `x-request-id` rules | 1, 2 |
| FTS diagnostics on WARN | 2, 3 |
| Query/compile INFO truncation 512 | 3 |
| `CliBoundaryError` mapping | 3 |
| physical no new `contains("not found")` | 3 |
| Catalog refresh / acceleration root_cause | 3 |
| `run_serve` tracing init, ignore `RUST_LOG` | 4 |
| Startup INFO | 4 |
| Web `ApiFailure` | 5 |
| Banner map + request_id + no 5xx root in UI | 6 |
| Assistant `query_sql` id | 6 |
| Guides | 7 |
| Gateway/Control/OTel excluded | all (not implemented) |

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-30-pchronicle-serve-web-error-logging.md`. Two execution options:

**1. Subagent-Driven (recommended)** — fresh subagent per task, review between tasks

**2. Inline Execution** — this session, batch with checkpoints

Which approach?

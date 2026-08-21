# Storyline Unified Unknown Fields Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `StorylinePresence` and format-specific residual blobs with one bounded, namespaced unknown-fields mechanism that survives same-format, cross-format, and Storyline Lance round trips.

**Architecture:** `StorylineDocument` owns sparse RFC 6901 pointer maps grouped by source format and source document. A shared codec utility validates pointers, computes per-trajectory wildcard counts, enforces the 4096/1 MiB admission limits, carries foreign residuals in a `_storyline` envelope, and restores target-format residuals without overwriting canonical Storyline fields. Lance stores the maps on the run row and externalizes large individual values into the existing content-addressed `objects.lance` store.

**Tech Stack:** Rust 2021, serde/serde_json/serde_yaml, BLAKE3, Arrow, Lance 9, DataFusion, cargo test.

**Spec:** `docs/superpowers/specs/2026-08-19-storyline-unknown-fields-design.md`

## Global Constraints

- Known source fields with missing and explicit `null` are equivalent and produce no residual entry.
- An unknown key whose value is `null` is retained because the key itself is unknown to Storyline.
- Exact locations use RFC 6901 JSON Pointer; wildcard paths are derived only through format-aware array positions.
- Admission defaults are 4096 entries and 1 MiB of source IDs, pointers, and compact JSON values per Storyline across all sources.
- Exceeding either admission limit rejects the complete Storyline; truncation and unlimited configuration are forbidden.
- `_storyline` is reserved for transport, never recaptured as an unknown source key.
- Target-format canonical fields win; conflicting residual values fail closed with source, document ID, trajectory, and pointer context.
- Physical single-object/array/JSONL shape is canonicalized by the target codec and is not retained.
- Logical document-level residual is copied into every Storyline split from that document; large repeated values may deduplicate only inside Lance `objects.lance`.
- Do not modify TTAS, Queue/Sampler, Search, or `persisting-dlcapt`.
- Preserve the user's current edits in `crates/persisting-pchronicle/src/store/files/atif_stream.rs`, `crates/persisting-pchronicle/src/store/files/mod.rs`, and `crates/persisting-pchronicle/src/store/files/json_stream.rs`; the projected ATIF query path is outside this feature.
- Use targeted `persisting-pchronicle` tests with `--no-default-features` or `--features lance-store`; do not use workspace-wide acceptance commands.

## File Structure

- Create `crates/persisting-pchronicle/src/formats/unknown_fields.rs`: residual types, limits, JSON Pointer operations, format-aware count normalization, source document IDs, envelope parsing/writing, and carrier bindings.
- Modify `crates/persisting-pchronicle/src/formats/storyline.rs`: replace `presence` with `unknown_fields` and `unknown_key_counts`; keep `FieldPresence<T>` only where it is a canonical Storyline field such as tool results.
- Modify `crates/persisting-pchronicle/src/document.rs`: canonical container policy and common JSON codec orchestration.
- Modify `crates/persisting-pchronicle/src/atif.rs` and `crates/persisting-pchronicle/src/convert/atif.rs`: capture and restore ATIF unknown members without missing/null sidecars.
- Modify `crates/persisting-pchronicle/src/formats/actf.rs` and `crates/persisting-pchronicle/src/convert/actf.rs`: replace `persisting.dev/actf/v1` blobs with exact pointers.
- Modify `crates/persisting-pchronicle/src/formats/openai_corpus.rs`: replace `persisting.dev/openai-msg/v1` blobs with canonical row generation plus exact pointers.
- Modify `crates/persisting-pchronicle/src/agenticmd/convert.rs`: carry the same Storyline residual in existing frontmatter metadata and capture AgenticMD-only keys.
- Modify `crates/persisting-pchronicle/src/store/files/atif_reader.rs`: route full ATIF materialization through the new codec while leaving projected streaming untouched.
- Modify `crates/persisting-pchronicle/src/store/storyline/{model,rows,content,mutation,mod}.rs`: run-row persistence, schema upgrade, limits, and per-value content offload.
- Create `crates/persisting-pchronicle/tests/unknown_fields_roundtrip.rs`: cross-format and Lance acceptance matrix.
- Modify `crates/persisting-pchronicle/README.md` and affected focused tests to document the new semantic lossless boundary.

---

### Task 1: Residual Core Types, Limits, and Pointer Operations

**Files:**
- Create: `crates/persisting-pchronicle/src/formats/unknown_fields.rs`
- Modify: `crates/persisting-pchronicle/src/formats/mod.rs`
- Modify: `crates/persisting-pchronicle/src/model.rs`
- Test: `crates/persisting-pchronicle/src/formats/unknown_fields.rs`

**Interfaces:**
- Produces: `UnknownFieldLimits`, `SourceUnknownFields`, `StorylineUnknownFields`, `UnknownKeyCounts`, `UnknownFieldCounts`, `validate_json_pointer`, `restore_json_pointer`, `canonical_source_document_id`, `compute_unknown_key_counts`, `validate_unknown_fields_with`, and `validate_unknown_fields`.
- Consumes: `DocumentFormat::as_str()`, `InputIssue`, `InputResult`, `serde_json::Value`.

- [ ] **Step 1: Write failing unit tests for pointer validation, restoration, byte accounting, and limits**

```rust
fn normalize_test_pointer(source: &str, pointer: &str) -> InputResult<String> {
    assert_eq!(source, "atif");
    Ok(pointer.replacen("/steps/0/", "/steps/*/", 1))
}

#[test]
fn unknown_fields_validate_pointer_counts_and_limits() {
    let mut fields = StorylineUnknownFields::default();
    fields.insert(
        "atif",
        "source-1",
        "/steps/0/vendor~1field",
        json!({"kept": true}),
    ).unwrap();
    let counts = fields.validate_with(
        UnknownFieldLimits::default(),
        |source, pointer| normalize_test_pointer(source, pointer),
    ).unwrap();
    assert_eq!(counts["atif"]["/steps/*/vendor~1field"], 1);

    let too_many = UnknownFieldLimits { max_fields: 0, max_bytes: 1_048_576 };
    assert!(fields.validate_with(too_many, normalize_test_pointer).is_err());
    assert!(validate_json_pointer("/bad~2escape").is_err());
}

#[test]
fn restore_pointer_rejects_canonical_collision() {
    let mut target = json!({"steps": [{"message": "canonical"}]});
    let error = restore_json_pointer(
        &mut target,
        "/steps/0/message",
        json!("residual"),
        PointerWrite::InsertOnly,
    ).unwrap_err();
    assert!(error.to_string().contains("/steps/0/message"));
}
```

- [ ] **Step 2: Run the focused tests and verify the module is absent**

Run: `cargo test -p persisting-pchronicle --no-default-features unknown_fields --lib`

Expected: FAIL because `formats::unknown_fields` and its types do not exist.

- [ ] **Step 3: Implement the residual types and deterministic size calculation**

```rust
pub const DEFAULT_MAX_UNKNOWN_FIELDS: usize = 4096;
pub const DEFAULT_MAX_UNKNOWN_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownFieldLimits {
    pub max_fields: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceUnknownFields {
    pub source_document_id: String,
    pub fields: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StorylineUnknownFields {
    pub sources: BTreeMap<String, SourceUnknownFields>,
}

pub type UnknownFieldCounts = BTreeMap<String, u64>;
pub type UnknownKeyCounts = BTreeMap<String, UnknownFieldCounts>;
```

Implement `UnknownFieldLimits::validate()` so zero/unbounded values are rejected. Implement `StorylineUnknownFields::insert()` so a source format cannot silently change `source_document_id`. Define logical byte size as the source ID once per source plus every pointer UTF-8 length plus `serde_json::to_vec(value)?.len()`.

`compute_unknown_key_counts(fields)` dispatches to `normalize_unknown_pointer(source, pointer)` without applying a quota. `validate_unknown_fields(fields, limits)` first validates finite limits and logical size, then returns the same computed counts. The initial normalizer preserves the exact pointer for sources without an adapter; Tasks 4-7 add format-aware array normalization branches while keeping unknown future source namespaces carryable.

- [ ] **Step 4: Implement strict RFC 6901 decoding and restore policies**

```rust
pub(crate) enum PointerWrite {
    InsertOnly,
    ReplaceResidualOwned,
}

pub(crate) fn restore_json_pointer(
    target: &mut Value,
    pointer: &str,
    value: Value,
    write: PointerWrite,
) -> Result<()>;

pub(crate) fn canonical_source_document_id(value: &Value) -> Result<String>;
```

`validate_json_pointer` must accept `""` and slash-prefixed pointers, decode only `~0` and `~1`, reject bad escapes, and never create missing array slots. `canonical_source_document_id` removes a root `_storyline`, recursively sorts object keys, serializes compact JSON, and returns the BLAKE3 hex digest.

- [ ] **Step 5: Run the core tests**

Run: `cargo test -p persisting-pchronicle --no-default-features unknown_fields --lib`

Expected: PASS, including exact boundary cases at 4096 entries and 1 MiB.

- [ ] **Step 6: Commit the core utility**

```bash
git add crates/persisting-pchronicle/src/formats/unknown_fields.rs crates/persisting-pchronicle/src/formats/mod.rs crates/persisting-pchronicle/src/model.rs
git commit -m "feat(pchronicle): add bounded unknown fields core"
```

### Task 2: Replace `StorylinePresence` in the Authoritative Model

**Files:**
- Modify: `crates/persisting-pchronicle/src/formats/storyline.rs`
- Modify: `crates/persisting-pchronicle/src/model.rs`
- Modify: `crates/persisting-pchronicle/src/lib.rs`
- Modify: `crates/persisting-pchronicle/src/convert/{atif,actf,events}.rs`
- Modify: `crates/persisting-pchronicle/src/formats/openai_corpus.rs`
- Modify: `crates/persisting-pchronicle/src/agenticmd/convert.rs`
- Modify: `crates/persisting-pchronicle/src/document.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/model.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/rows.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/tests.rs`
- Modify: focused in-scope Storyline test constructors returned by `rg -l 'presence:' crates/persisting-pchronicle --glob '!src/store/files/atif_stream.rs'`
- Test: `crates/persisting-pchronicle/src/formats/storyline.rs`
- Test: `crates/persisting-pchronicle/src/document.rs`

**Interfaces:**
- Consumes: Task 1 residual types and limits.
- Produces: `StorylineDocument::{unknown_fields, unknown_key_counts}`, canonical ATIF container selection, and model validation without `StorylinePresence`.

- [ ] **Step 1: Replace presence-focused tests with canonical semantic tests**

```rust
fn atif_fixture_value() -> Value {
    json!({
        "schema_version": "ATIF-v1.7",
        "trajectory_id": "one",
        "agent": {"name": "agent", "version": "1"},
        "steps": []
    })
}

#[test]
fn storyline_serialization_has_no_presence_sidecar() {
    let story = StorylineDocument::new("session", "agent");
    let value = serde_json::to_value(story).unwrap();
    assert!(value.get("presence").is_none());
    assert!(value.get("unknown_fields").is_none());
}

#[test]
fn atif_singleton_object_and_array_encode_canonically() {
    let object = atif_fixture_value();
    let from_object = decode_json_storylines(DocumentFormat::Atif, &object.to_string(), "a.json").unwrap();
    let from_array = decode_json_storylines(DocumentFormat::Atif, &json!([object]).to_string(), "a.json").unwrap();
    assert_eq!(
        encode_json_storylines(DocumentFormat::Atif, &from_object).unwrap(),
        encode_json_storylines(DocumentFormat::Atif, &from_array).unwrap(),
    );
}
```

- [ ] **Step 2: Run focused model/document tests and verify old semantics fail the new expectations**

Run: `cargo test -p persisting-pchronicle --no-default-features --lib storyline_serialization_has_no_presence_sidecar`

Run: `cargo test -p persisting-pchronicle --no-default-features --lib atif_singleton_object_and_array_encode_canonically`

Expected: FAIL because `presence` is still serialized and singleton array shape is preserved.

- [ ] **Step 3: Replace the field and delete the sidecar enums**

```rust
pub struct StorylineDocument {
    // existing fields
    #[serde(default, skip_serializing_if = "StorylineUnknownFields::is_empty")]
    pub unknown_fields: StorylineUnknownFields,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown_key_counts: UnknownKeyCounts,
    pub turns: Vec<StorylineTurn>,
}
```

Delete `PresenceState`, `StorylinePresence`, `StorylineRootField`, `StorylineAgentField`, `StorylineTurnField`, and `StorylineCollectionShape`. Keep `FieldPresence<T>` because `StorylineToolCall::result` is an explicit canonical three-state field. Update every in-scope struct literal to use empty unknown fields/counts.

- [ ] **Step 4: Make validation recompute and compare counts**

```rust
impl StorylineDocument {
    pub fn refresh_unknown_key_counts(&mut self) -> InputResult<()> {
        self.unknown_key_counts = compute_unknown_key_counts(&self.unknown_fields)?;
        Ok(())
    }
}
```

`StorylineDocument::validate` recomputes counts without a quota and rejects any mismatch, including non-empty residual paired with empty counts. Admission boundaries separately call `validate_unknown_fields` with their selected limits, so callers may intentionally configure limits above or below the defaults. Constructors set both fields empty; every codec calls `refresh_unknown_key_counts` after capture/envelope merge.

- [ ] **Step 5: Remove container provenance and choose one canonical ATIF encoding rule**

Change `encode_json_storylines` so one top-level ATIF root encodes as an object and two or more roots encode as an array. Preserve the provided `stories` order and parent/child order; delete `prepare_atif_collection`, shape conflict checks, and ordinal sorting. Change `atif_collection_to_storylines` to accept only `&AtifTrajectory` and stop writing shape/ordinal metadata.

- [ ] **Step 6: Keep the Lance feature compiling with the new authoritative fields**

Replace `StoryRunRow::presence` with `unknown_fields` and `unknown_key_counts`; replace the run batch value with nullable `unknown_fields_json` and `unknown_key_counts_json`. Retain a nullable legacy `presence_json` physical column written as null, but ignore it on read. Update storage test constructors. Schema-upgrade behavior and configurable storage limits remain Task 8.

- [ ] **Step 7: Run no-Lance and Lance model tests**

Run: `cargo test -p persisting-pchronicle --no-default-features --lib`

Expected: PASS. Tests that asserted missing/null or singleton-array structural preservation are replaced with canonical semantic assertions.

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib three_table_roundtrip`

Expected: PASS with unknown fields/counts crossing the in-memory three-table split/reconstruct boundary.

- [ ] **Step 8: Commit the authoritative model change**

```bash
git add crates/persisting-pchronicle/src/formats/storyline.rs crates/persisting-pchronicle/src/model.rs crates/persisting-pchronicle/src/lib.rs crates/persisting-pchronicle/src/convert/atif.rs crates/persisting-pchronicle/src/convert/actf.rs crates/persisting-pchronicle/src/convert/events.rs crates/persisting-pchronicle/src/formats/openai_corpus.rs crates/persisting-pchronicle/src/agenticmd/convert.rs crates/persisting-pchronicle/src/document.rs crates/persisting-pchronicle/src/store/storyline/model.rs crates/persisting-pchronicle/src/store/storyline/rows.rs crates/persisting-pchronicle/src/store/storyline/tests.rs crates/persisting-pchronicle/src/store/catalog/tests.rs crates/persisting-pchronicle/src/tests.rs
git commit -m "refactor(pchronicle): replace storyline presence sidecar"
```

### Task 3: Unified `_storyline` Envelope and Carrier Binding

**Files:**
- Modify: `crates/persisting-pchronicle/src/formats/unknown_fields.rs`
- Modify: `crates/persisting-pchronicle/src/document.rs`
- Test: `crates/persisting-pchronicle/src/formats/unknown_fields.rs`

**Interfaces:**
- Consumes: `StorylineUnknownFields` from Task 1 and `StorylineDocument` fields from Task 2.
- Produces: `DocumentCodecOptions`, option-aware document entry points, `CarrierBinding`, `take_unknown_fields_envelope`, `attach_carried_unknown_fields`, and `write_foreign_unknown_fields_envelope`.

- [ ] **Step 1: Write failing envelope tests for carrier distribution and reserved-key rejection**

```rust
#[test]
fn envelope_distributes_foreign_sources_by_carrier() {
    let mut raw = json!({
        "attempts": {"1": {}},
        "_storyline": {"unknown_fields": {"version": 1, "by_trajectory": {
            "/attempts/1": {"sources": {
                "atif": {"source_document_id": "a", "fields": {"/vendor": 7}}
            }}
        }}}
    });
    let envelope = take_unknown_fields_envelope(&mut raw).unwrap();
    assert!(raw.get("_storyline").is_none());
    let mut stories = vec![StorylineDocument::new("s", "a")];
    attach_carried_unknown_fields(
        envelope,
        &[CarrierBinding { story_index: 0, pointer: "/attempts/1".into() }],
        &mut stories,
        UnknownFieldLimits::default(),
    ).unwrap();
    assert_eq!(stories[0].unknown_fields.sources["atif"].fields["/vendor"], 7);
}
```

- [ ] **Step 2: Run the test and verify envelope helpers are missing**

Run: `cargo test -p persisting-pchronicle --no-default-features envelope_ --lib`

Expected: FAIL because the envelope API is undefined.

- [ ] **Step 3: Implement versioned envelope DTOs and exact carrier matching**

```rust
pub(crate) struct CarrierBinding {
    pub story_index: usize,
    pub pointer: String,
}

pub(crate) fn take_unknown_fields_envelope(
    document: &mut Value,
) -> InputResult<BTreeMap<String, StorylineUnknownFields>>;

pub(crate) fn write_foreign_unknown_fields_envelope(
    target_format: DocumentFormat,
    document: &mut Value,
    stories: &[StorylineDocument],
    carriers: &[CarrierBinding],
) -> Result<()>;
```

Represent the payload as `_storyline.unknown_fields` with integer `version: 1` and `by_trajectory.<carrier>.sources`. Reject non-object `_storyline`, missing/non-1 versions, extra envelope keys, duplicate carriers, bad carrier pointers, unbound carriers, and any source namespace that changes its `source_document_id` during merge. Exclude `sources[target_format.as_str()]` when writing a target document.

- [ ] **Step 4: Recompute counts after attachment and enforce total per-trajectory limits**

Merge all carried namespaces before calling validation so a document cannot bypass 4096/1 MiB by splitting data across source formats. Ensure the envelope structure itself is not included in byte accounting.

- [ ] **Step 5: Add option-aware public codec entry points**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DocumentCodecOptions {
    pub unknown_fields: UnknownFieldLimits,
}

pub fn decode_json_storylines_with_options(
    format: DocumentFormat,
    input: &str,
    relative_path: impl AsRef<Path>,
    options: DocumentCodecOptions,
) -> InputResult<Vec<StorylineDocument>>;

pub fn encode_json_storylines_with_options(
    format: DocumentFormat,
    stories: &[StorylineDocument],
    options: DocumentCodecOptions,
) -> Result<Value>;
```

Keep the existing functions as compatibility wrappers using `DocumentCodecOptions::default()`. Validate options before parsing or encoding; zero limits fail even for empty residual.

- [ ] **Step 6: Run envelope/core tests**

Run: `cargo test -p persisting-pchronicle --no-default-features unknown_fields --lib`

Expected: PASS.

- [ ] **Step 7: Commit the envelope layer**

```bash
git add crates/persisting-pchronicle/src/formats/unknown_fields.rs crates/persisting-pchronicle/src/document.rs
git commit -m "feat(pchronicle): add unknown fields wire envelope"
```

### Task 4: ATIF Capture, Restore, and Full-Document Readers

**Files:**
- Modify: `crates/persisting-pchronicle/src/atif.rs`
- Modify: `crates/persisting-pchronicle/src/convert/atif.rs`
- Modify: `crates/persisting-pchronicle/src/document.rs`
- Modify: `crates/persisting-pchronicle/src/store/files/atif_reader.rs`
- Test: `crates/persisting-pchronicle/src/convert/atif.rs`
- Test: `crates/persisting-pchronicle/src/document.rs`
- Test: `crates/persisting-pchronicle/src/store/files/atif_reader.rs`

**Interfaces:**
- Consumes: core residual and envelope APIs.
- Produces: `atif_value_to_storylines`, `storylines_to_atif_value`, ATIF carrier bindings, and `normalize_atif_pointer`.

- [ ] **Step 1: Write failing tests for root, agent, step, tool, child, and null unknown values**

```rust
#[test]
fn atif_unknown_fields_round_trip_without_presence() {
    let input = json!({
        "schema_version": "ATIF-v1.7",
        "session_id": null,
        "trajectory_id": "t1",
        "vendor_root": null,
        "agent": {"name": "a", "version": "1", "vendor_agent": {"x": 1}},
        "steps": [{
            "step_id": 1, "source": "user", "message": "hi",
            "vendor_step": [1, 2]
        }]
    });
    let stories = decode_json_storylines(DocumentFormat::Atif, &input.to_string(), "t.json").unwrap();
    assert_eq!(stories[0].unknown_fields.sources["atif"].fields["/vendor_root"], Value::Null);
    assert_eq!(stories[0].unknown_key_counts["atif"]["/steps/*/vendor_step"], 1);
    let output = encode_json_storylines(DocumentFormat::Atif, &stories).unwrap();
    assert_eq!(output["vendor_root"], Value::Null);
    assert!(output.get("session_id").is_some()); // canonical effective identity
}
```

- [ ] **Step 2: Run ATIF tests and verify unknown keys are currently discarded**

Run: `cargo test -p persisting-pchronicle --no-default-features atif_unknown_fields --lib`

Expected: FAIL because ATIF serde DTOs discard `vendor_*` keys.

- [ ] **Step 3: Add flattened unknown maps to ATIF wire DTOs**

Add `#[serde(default, flatten)] pub unknown: Map<String, Value>` to `AtifTrajectory`, `AtifAgent`, `AtifStep`, and `AtifToolCall`. Update in-scope literals. `_storyline` is removed before deserialization and must never enter these maps.

- [ ] **Step 4: Capture absolute pointers while flattening embedded trajectories**

Change the recursive ATIF visitor to accept `(source_document_id, source_pointer)` and insert unknown members at paths such as `/agent/vendor_agent`, `/steps/0/vendor_step`, and `/subagent_trajectories/0/vendor_child`. All Storylines belonging to one top-level trajectory share its source document ID; each Storyline receives the entries belonging to its own subtree plus any truly document-shared entries.

- [ ] **Step 5: Restore ATIF-owned residual after canonical tree reconstruction**

Serialize the canonical `AtifTrajectory`, group flattened Storylines by source document ID, merge identical copied fields, and apply residual pointers with `PointerWrite::InsertOnly`. Treat ATIF known fields as canonical-owned; collision errors must include the pointer and trajectory. Generate carrier bindings for a top-level root and embedded child pointers before writing foreign envelopes.

- [ ] **Step 6: Normalize ATIF array positions for counts**

Implement `normalize_atif_pointer` with array positions only at `steps/<n>`, `steps/<n>/tool_calls/<n>`, and recursive `subagent_trajectories/<n>`. A numeric unknown object key outside those schema positions remains numeric rather than becoming `*`.

- [ ] **Step 7: Route full ATIF materialization through the same codec**

Make `document::decode_json_storylines` and `store/files/atif_reader.rs` use the same value-to-storylines helper for object, array, and line records. Keep root record order but do not retain input container shape. Do not edit `store/files/atif_stream.rs` or `store/files/json_stream.rs`.

- [ ] **Step 8: Run ATIF-focused tests**

Run: `cargo test -p persisting-pchronicle --no-default-features atif --lib`

Expected: PASS, including embedded subagents, null/missing canonicalization, pointer escaping, limit rejection, and object/array canonical equivalence.

- [ ] **Step 9: Commit the ATIF adapter**

```bash
git add crates/persisting-pchronicle/src/atif.rs crates/persisting-pchronicle/src/convert/atif.rs crates/persisting-pchronicle/src/document.rs crates/persisting-pchronicle/src/store/files/atif_reader.rs
git commit -m "feat(pchronicle): preserve ATIF unknown fields"
```

### Task 5: ACTF Exact-Path Residual and Multi-Attempt Merge

**Files:**
- Modify: `crates/persisting-pchronicle/src/convert/actf.rs`
- Modify: `crates/persisting-pchronicle/src/formats/actf.rs`
- Modify: `crates/persisting-pchronicle/src/document.rs`
- Test: `crates/persisting-pchronicle/src/convert/actf.rs`

**Interfaces:**
- Consumes: Task 3 envelope API and pointer write policies.
- Produces: ACTF residual capture/restore, `/attempts/<id>` carrier bindings, `normalize_actf_pointer`, and legacy `persisting.dev/actf/v1` migration.

- [ ] **Step 1: Rewrite the existing residual test to assert exact pointer maps**

```rust
#[test]
fn actf_residual_is_namespaced_exact_paths() {
    let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
    value["root_unknown"] = Value::Null;
    value["attempts"]["1"]["trajectory"]["steps"][0]["step_unknown"] = json!({"x": 1});
    let document: ActfDocument = serde_json::from_value(value.clone()).unwrap();
    let stories = actf_to_storylines(&document).unwrap();
    let source = &stories[0].unknown_fields.sources["actf"];
    assert_eq!(source.fields["/root_unknown"], Value::Null);
    assert_eq!(source.fields["/attempts/1/trajectory/steps/0/step_unknown"], json!({"x": 1}));
    assert!(!serde_json::to_string(&stories).unwrap().contains("persisting.dev/actf/v1"));
}
```

- [ ] **Step 2: Run the test and verify legacy blobs fail it**

Run: `cargo test -p persisting-pchronicle --no-default-features actf_residual_is_namespaced_exact_paths --lib`

Expected: FAIL because residual is stored under `Storyline.extra`.

- [ ] **Step 3: Replace nested residual blobs with pointer insertion**

Use `document.task_id` as `source_document_id`. Convert the current `root_metadata`, `attempt_residual`, `trajectory_residual`, `step_residual`, and `tool_residual` outputs into exact members at their original locations. Copy root-level residual entries into every attempt Storyline; store only the current attempt subtree on that Storyline. Do not store timestamp spelling, missing/null flags, or input-vs-command presentation when the ACTF encoder has a canonical spelling.

- [ ] **Step 4: Generate canonical ACTF then restore residual-owned paths**

Keep canonical Storyline mappings for task ID, correctness, score/status, message, reasoning, metrics, tool calls, and observations. Mark required ACTF placeholders that have no Storyline canonical owner as `ReplaceResidualOwned`; all other pre-existing target paths use `InsertOnly`. Merge stories with the same source document ID, deduplicate equal copied root entries, and fail on unequal values.

- [ ] **Step 5: Add carrier and count normalization rules**

Carrier for attempt `1` is `/attempts/1`. Array indices become `*` only below `/attempts/<dynamic-id>/trajectory/steps`, `assistant_content/tool_calls`, `tools`, and `observation`; numeric attempt IDs remain object keys.

- [ ] **Step 6: Migrate readable legacy ACTF residuals**

Add a crate-private migration helper that detects `persisting.dev/actf/v1`, reconstructs an ACTF value with the legacy converter, then captures it with the new pointer codec. Remove only that recognized key from Storyline `extra`; retain unrelated business extra. Fail when legacy attempt grouping is incomplete or conflicting.

- [ ] **Step 7: Run ACTF tests**

Run: `cargo test -p persisting-pchronicle --no-default-features actf --lib`

Expected: PASS for single/multiple attempts, unknown null values, shared-root deduplication, conflict failure, and no legacy extension keys in new Storyline output.

- [ ] **Step 8: Commit the ACTF adapter**

```bash
git add crates/persisting-pchronicle/src/formats/actf.rs crates/persisting-pchronicle/src/convert/actf.rs crates/persisting-pchronicle/src/document.rs
git commit -m "feat(pchronicle): unify ACTF unknown fields"
```

### Task 6: OpenAI Message Canonical Rows and Residual Paths

**Files:**
- Modify: `crates/persisting-pchronicle/src/formats/openai_corpus.rs`
- Modify: `crates/persisting-pchronicle/src/document.rs`
- Test: `crates/persisting-pchronicle/src/formats/openai_corpus.rs`

**Interfaces:**
- Consumes: pointer/envelope helpers and canonical Storyline model.
- Produces: OpenAI source grouping, row carriers, exact residual maps, `normalize_openai_pointer`, and legacy `persisting.dev/openai-msg/v1` migration.

- [ ] **Step 1: Write failing tests for canonical envelope output and unknown row members**

```rust
#[test]
fn openai_unknown_fields_use_exact_row_paths() {
    let input = json!({"root_vendor": 1, "session_steps": [{
        "session_id": "s", "step_id": 1,
        "messages": [{"role": "user", "content": "hi", "message_vendor": null}],
        "response": {"role": "assistant", "content": "ok"},
        "row_vendor": [3, 2, 1]
    }]});
    let stories = parse_openai_msg_corpus_value(&input, "corpus.json").unwrap();
    let fields = &stories[0].unknown_fields.sources["openai-msg"].fields;
    assert_eq!(fields["/root_vendor"], 1);
    assert_eq!(fields["/session_steps/0/row_vendor"], json!([3, 2, 1]));
    assert_eq!(fields["/session_steps/0/messages/0/message_vendor"], Value::Null);
}
```

- [ ] **Step 2: Run the test and verify legacy metadata fails it**

Run: `cargo test -p persisting-pchronicle --no-default-features openai_unknown_fields_use_exact_row_paths --lib`

Expected: FAIL because OpenAI residual is stored in format extension blobs.

- [ ] **Step 3: Define the OpenAI canonical output shape**

Always synthesize `{"session_steps": [...]}` for JSON output. Generate one canonical record for each agent response turn, with session ID, step ID, request messages, response, model/run IDs, timestamps, and metrics derived from Storyline. Do not retain whether input was an array or envelope, whether output lived in `messages` or `response`, timestamp numeric spelling, or missing/null presentation.

- [ ] **Step 4: Capture all unconsumed members as exact pointers**

Use validated `relative_path` as `source_document_id`. Copy root members other than `session_steps` into every session Storyline. For each session, capture only its rows' unconsumed members, including nested message/tool-call members, at absolute canonical envelope pointers. A captured unknown object member stores its complete subtree.

- [ ] **Step 5: Restore rows, foreign envelopes, and ordering**

Group Storylines by source document ID, order canonical rows by retained source ordinal when available as a Storyline ordering hint and otherwise by stable Storyline/turn order, apply OpenAI-owned residual, then write foreign sources under root `_storyline`. Use `/session_steps/<row-index>` carrier pointers. Filtered exports may contain ordinal gaps but cannot contain duplicate target carriers.

- [ ] **Step 6: Normalize OpenAI pointers and migrate legacy blobs**

Wildcard array positions for `session_steps`, `messages`, `tool_calls`, and other codec-declared arrays; keep numeric object keys literal. Migrate recognized legacy OpenAI metadata by using the old recovery function once, recapturing the reconstructed file, then deleting only `persisting.dev/openai-msg/v1`.

- [ ] **Step 7: Run OpenAI tests**

Run: `cargo test -p persisting-pchronicle --no-default-features openai --lib`

Expected: PASS for multi-session files, unknown null values, canonical envelope output, unsafe relative path rejection, conflicts, and legacy migration.

- [ ] **Step 8: Commit the OpenAI adapter**

```bash
git add crates/persisting-pchronicle/src/formats/openai_corpus.rs crates/persisting-pchronicle/src/document.rs
git commit -m "feat(pchronicle): unify OpenAI unknown fields"
```

### Task 7: AgenticMD Frontmatter Transport

**Files:**
- Modify: `crates/persisting-pchronicle/src/agenticmd/convert.rs`
- Modify: `crates/persisting-pchronicle/src/agenticmd/validate.rs`
- Test: `crates/persisting-pchronicle/src/agenticmd/convert.rs`

**Interfaces:**
- Consumes: Storyline residual fields and common validation.
- Produces: AgenticMD capture/restore at logical `/frontmatter` and `/blocks/<n>/header` pointers without a second Markdown block protocol.

- [ ] **Step 1: Write a failing AgenticMD foreign-residual round-trip test**

```rust
#[test]
fn agenticmd_frontmatter_carries_unknown_sources() {
    let mut story = StorylineDocument::new("s", "a");
    story.unknown_fields.insert("atif", "source", "/vendor", json!(7)).unwrap();
    story.refresh_unknown_key_counts().unwrap();
    let encoded = encode_agenticmd(&story).unwrap();
    let decoded = parse_agenticmd(&encoded).unwrap();
    assert_eq!(decoded.unknown_fields, story.unknown_fields);
    assert_eq!(decoded.unknown_key_counts, story.unknown_key_counts);
}
```

- [ ] **Step 2: Run the test and confirm model validation/capture is incomplete**

Run: `cargo test -p persisting-pchronicle --no-default-features agenticmd_frontmatter_carries_unknown_sources --lib`

Expected: FAIL until frontmatter handling explicitly validates and preserves the residual.

- [ ] **Step 3: Carry residual in existing Storyline frontmatter metadata**

Keep `frontmatter.storyline.unknown_fields` and `unknown_key_counts`; do not introduce `_storyline` in Markdown body or block headers. Recompute counts when parsing and reject mismatches rather than trusting serialized counts.

- [ ] **Step 4: Capture AgenticMD-only unknown keys**

Capture unconsumed top-level frontmatter and block header fields under source `agenticmd`, using the source document hash after removing `frontmatter.storyline`. Restore them during encode only where they do not collide with authoritative frontmatter or block fields. Normalize only `/blocks/<n>` as an array position.

- [ ] **Step 5: Run AgenticMD tests**

Run: `cargo test -p persisting-pchronicle --no-default-features agenticmd --lib`

Expected: PASS for authoritative Storyline metadata, human-readable fallback parsing, unknown null fields, and collision rejection.

- [ ] **Step 6: Commit the AgenticMD transport**

```bash
git add crates/persisting-pchronicle/src/agenticmd/convert.rs crates/persisting-pchronicle/src/agenticmd/validate.rs
git commit -m "feat(pchronicle): carry unknown fields through AgenticMD"
```

### Task 8: Lance Run Rows and Legacy Schema Upgrade

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/storyline/model.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/rows.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/mutation.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/tests.rs`
- Test: `crates/persisting-pchronicle/src/store/storyline/model.rs`
- Test: `crates/persisting-pchronicle/src/store/storyline/rows.rs`
- Test: `crates/persisting-pchronicle/src/store/storyline/tests.rs`

**Interfaces:**
- Consumes: authoritative unknown fields/counts and default/configured limits.
- Produces: automatic nullable-column upgrade for legacy runs datasets and configurable storage admission limits.

- [ ] **Step 1: Write failing legacy-schema append and configured-limit tests**

```rust
#[tokio::test]
async fn legacy_runs_schema_upgrades_before_append() {
    let temporary = tempfile::tempdir().unwrap();
    let store = create_legacy_presence_store(temporary.path()).await.unwrap();
    let mut story = StorylineDocument::new("new", "agent");
    story.unknown_fields.insert("atif", "source", "/vendor", json!(1)).unwrap();
    story.refresh_unknown_key_counts().unwrap();
    store.replace_storyline(&story).await.unwrap();
    assert_eq!(store.get_storyline_full("new").await.unwrap().unwrap().unknown_fields, story.unknown_fields);
}
```

- [ ] **Step 2: Run the legacy test and verify append rejects the old schema**

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib legacy_runs_schema_upgrades_before_append`

Expected: FAIL because the existing runs table lacks the new nullable columns.

- [ ] **Step 3: Validate residual on every split/write boundary**

Add `max_unknown_fields` and `max_unknown_bytes` as finite positive fields on `StorylineContentOptions`, defaulting to 4096 and 1 MiB. `split_storyline` and `next_storyline_stream_chunk` validate logical hydrated residual before row creation and report actual/limit values.

- [ ] **Step 4: Add nullable columns to legacy runs datasets before append**

When opening a committed runs table for writing, inspect its Arrow schema. If either new column is missing, call Lance `Dataset::add_columns(NewColumnTransform::SqlExpressions(...))` with `CAST(NULL AS STRING)` for that column while holding the existing store write guard. Pin the returned version in the new snapshot; do not mutate a snapshot during read-only open.

- [ ] **Step 5: Test legacy and new schemas**

Complete `create_legacy_presence_store(path: &Path) -> Result<StorylineLanceStore>` in the test module by writing the pre-change 20-column runs batch plus matching empty steps/tool-calls/object datasets and CURRENT pointer. Verify read returns empty residual before migration, append one new Storyline, and verify both rows read. Verify zero limit options are rejected and configured smaller positive limits reject over-budget input.

- [ ] **Step 6: Run focused Lance tests**

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib legacy_runs_schema`

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib unknown_field_limit`

Expected: PASS.

- [ ] **Step 7: Commit Lance schema and admission changes**

```bash
git add crates/persisting-pchronicle/src/store/storyline/model.rs crates/persisting-pchronicle/src/store/storyline/rows.rs crates/persisting-pchronicle/src/store/storyline/mutation.rs crates/persisting-pchronicle/src/store/storyline/mod.rs crates/persisting-pchronicle/src/store/storyline/tests.rs
git commit -m "feat(pchronicle): persist storyline unknown fields"
```

### Task 9: Per-Value `objects.lance` Offload and Hydration

**Files:**
- Modify: `crates/persisting-pchronicle/src/store/storyline/content.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/mod.rs`
- Modify: `crates/persisting-pchronicle/src/store/storyline/tests.rs`
- Test: `crates/persisting-pchronicle/src/store/storyline/content.rs`

**Interfaces:**
- Consumes: `unknown_fields_json`, existing `ContentRef`, `PendingContent`, `objects.lance` commit/hydrate lifecycle.
- Produces: `externalize_unknown_field_values` and `hydrate_unknown_field_values` at residual value boundaries.

- [ ] **Step 1: Write failing tests for repeated-value dedup and descriptor collision**

```rust
#[tokio::test]
async fn repeated_unknown_value_is_stored_once() {
    let large = json!({"payload": "x".repeat(DEFAULT_CONTENT_OFFLOAD_THRESHOLD)});
    let mut first = story("residual-first");
    first.unknown_fields.insert("actf", "task-1", "/shared", large.clone()).unwrap();
    first.refresh_unknown_key_counts().unwrap();
    let mut second = story("residual-second");
    second.unknown_fields.insert("actf", "task-1", "/shared", large.clone()).unwrap();
    second.refresh_unknown_key_counts().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(temporary.path()).await.unwrap();
    store.replace_storylines(&[first, second]).await.unwrap();
    let paths = store.current_table_paths().await.unwrap().unwrap();
    let objects = open_objects(&paths.objects, paths.objects_version).await.unwrap();
    assert_eq!(objects.count_rows(None).await.unwrap(), 1);
    let hydrated = store.get_storyline_full("residual-first").await.unwrap().unwrap();
    assert_eq!(hydrated.unknown_fields.sources["actf"].fields["/shared"], large);
}
```

- [ ] **Step 2: Run the test and verify duplicated residual is inline**

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store repeated_unknown_value_is_stored_once --lib`

Expected: FAIL because content externalization does not inspect nested residual values.

- [ ] **Step 3: Externalize each residual value before run-batch serialization**

Parse/operate on typed `StorylineUnknownFields`, serialize each value independently, and call the existing BLAKE3/zstd `build_object` with `LogicalType::Json` when the compact value reaches `offload_threshold`. Replace only the internal run-row value with `Value::String(ContentRef::encode())`. Force offload when a user string starts with `CONTENT_REF_MAGIC`, even if small.

- [ ] **Step 4: Hydrate nested descriptors before public model reconstruction**

Collect all descriptors found at `sources.*.fields.*`, batch-resolve them with the existing content index, verify content ID/raw length/codec, parse the bytes back into one JSON `Value`, and replace the descriptor. Missing objects and invalid JSON fail closed. `unknown_key_counts` remains inline and is recomputed after hydration.

- [ ] **Step 5: Ensure admission limits use logical values**

Validate before externalization on write and after hydration on read. Add a regression test proving a 1 MiB+1 logical value is rejected even when its compressed object would be tiny.

- [ ] **Step 6: Run content/store tests**

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib repeated_unknown_value_is_stored_once`

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib logical_unknown_limit`

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --lib content_ref`

Expected: PASS with one content object for repeated large values and exact public hydration.

- [ ] **Step 7: Commit content offload**

```bash
git add crates/persisting-pchronicle/src/store/storyline/content.rs crates/persisting-pchronicle/src/store/storyline/mod.rs crates/persisting-pchronicle/src/store/storyline/tests.rs
git commit -m "feat(pchronicle): offload large unknown field values"
```

### Task 10: Cross-Format Acceptance, Documentation, and Cleanup

**Files:**
- Create: `crates/persisting-pchronicle/tests/unknown_fields_roundtrip.rs`
- Modify: `crates/persisting-pchronicle/tests/storyline_lance_roundtrip.rs`
- Modify: `crates/persisting-pchronicle/tests/import_roundtrip_fixtures.rs`
- Modify: `crates/persisting-pchronicle/README.md`
- Modify: `crates/persisting-pchronicle/src/formats/{storyline,unknown_fields}.rs`
- Modify: any in-scope file returned by `rg -n 'StorylinePresence|PresenceState|StorylineCollectionShape|persisting\.dev/(actf|openai-msg)' crates/persisting-pchronicle docs/superpowers/specs/2026-08-19-storyline-unknown-fields-design.md`

**Interfaces:**
- Consumes: all prior tasks.
- Produces: end-to-end semantic-lossless guarantees, final stale-symbol cleanup, and user documentation.

- [ ] **Step 1: Add a cross-format round-trip matrix test**

```rust
fn atif_with_unknowns() -> Value {
    json!({
        "schema_version": "ATIF-v1.7",
        "trajectory_id": "t1",
        "vendor_root": null,
        "agent": {"name": "agent", "version": "1"},
        "steps": [{"step_id": 1, "source": "user", "message": "hi", "vendor_step": 7}]
    })
}

fn assert_unknown_fields_equal(left: &Value, right: &Value, format: DocumentFormat) -> Result<()> {
    let left = decode_json_storylines(format, &left.to_string(), "left.json")?;
    let right = decode_json_storylines(format, &right.to_string(), "right.json")?;
    assert_eq!(left.len(), right.len());
    for (left, right) in left.iter().zip(&right) {
        assert_eq!(left.unknown_fields, right.unknown_fields);
    }
    Ok(())
}

#[test]
fn foreign_unknowns_survive_atif_actf_atif() -> Result<()> {
    let atif = atif_with_unknowns();
    let stories = decode_json_storylines(DocumentFormat::Atif, &atif.to_string(), "a.json")?;
    let actf = encode_json_storylines(DocumentFormat::Actf, &stories)?;
    assert!(actf.get("_storyline").is_some());
    let through = decode_json_storylines(DocumentFormat::Actf, &actf.to_string(), "b.actf.json")?;
    let recovered = encode_json_storylines(DocumentFormat::Atif, &through)?;
    assert_unknown_fields_equal(&atif, &recovered, DocumentFormat::Atif)?;
    Ok(())
}
```

Add reverse ACTF/OpenAI paths, a three-format hop, unknown null values, numeric object keys, malformed pointers, foreign envelope conflicts, filtered/path-invalid output, 4096/1 MiB exact boundaries, and object-order-insensitive comparison. The comparator deletes known null object members before comparing canonical known fields but never deletes an unknown null key.

- [ ] **Step 2: Run the new test before final cleanup**

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --test unknown_fields_roundtrip`

Expected: PASS for completed adapters; any failure is fixed in its owning codec rather than weakened in the comparator.

- [ ] **Step 3: Update Lance and fixture integration tests**

Replace assertions for ATIF missing/null three-state and singleton-array shape with canonical semantic assertions. Add a Storyline Lance test that imports a multi-attempt ACTF document with shared unknown values, verifies per-trajectory logical copies, verifies `objects.lance` deduplication, and exports the unknown values exactly.

- [ ] **Step 4: Remove stale sidecar and format-extension code**

Run: `rg -n 'StorylinePresence|PresenceState|StorylineCollectionShape|root_nulls|agent_nulls|turn_nulls|tool_call_extra_nulls|persisting\.dev/(actf|openai-msg)' crates/persisting-pchronicle`

Expected: no production matches. Test fixture strings may retain legacy extension keys only inside explicit migration tests.

- [ ] **Step 5: Update pChronicle documentation**

Document that known null/missing and input container shape are canonicalized; Storyline-unmodeled keys use namespaced exact-pointer residual; cross-format multi-hop uses `_storyline`; per-trajectory defaults are 4096/1 MiB and reject on overflow; `objects.lance` is an internal self-invisible optimization. Remove claims that ATIF null/missing/value and singleton array shape are retained.

- [ ] **Step 6: Run targeted final verification**

Run: `cargo test -p persisting-pchronicle --no-default-features --lib`

Expected: PASS.

Run: `cargo test -p persisting-pchronicle --no-default-features --features lance-store --test unknown_fields_roundtrip --test storyline_lance_roundtrip --test import_roundtrip_fixtures`

Expected: PASS.

Run: `cargo clippy -p persisting-pchronicle --no-default-features --features lance-store --lib --tests -- -D warnings`

Expected: PASS with no warnings. Failures from the explicitly excluded subsystems are not invoked by these commands.

Run: `git diff --check`

Expected: PASS. Verify separately that the user's pre-existing `atif_stream.rs`, `files/mod.rs`, and `json_stream.rs` edits are unchanged by this implementation.

- [ ] **Step 7: Commit acceptance and docs**

```bash
git add crates/persisting-pchronicle/tests/unknown_fields_roundtrip.rs crates/persisting-pchronicle/tests/storyline_lance_roundtrip.rs crates/persisting-pchronicle/tests/import_roundtrip_fixtures.rs crates/persisting-pchronicle/README.md crates/persisting-pchronicle/src/formats/storyline.rs crates/persisting-pchronicle/src/formats/unknown_fields.rs
git commit -m "test(pchronicle): verify unified unknown fields round trips"
```

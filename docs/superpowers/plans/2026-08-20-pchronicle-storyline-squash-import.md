# pChronicle Storyline Squash Import Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every `pchronicle import --output-format storyline` invocation publish one atomic Storyline Lance Store at the output root.

**Architecture:** Preserve imports keep their existing per-file staging path. Storyline imports adapt directory, file, and stdin inputs into one lazy `Iterator<Item = Result<StorylineDocument>>`, feed it to one root `StorylineLanceStore::replace_storyline_stream` call, and retain input paths only in CLI diagnostics and aggregate response metadata.

**Tech Stack:** Rust, Tokio, Clap, anyhow, serde_json, pChronicle Storyline model, StorylineLanceStore, DataFusion-backed CLI query tests.

**Spec:** `docs/superpowers/specs/2026-08-20-pchronicle-storyline-squash-import-design.md`

## Global Constraints

- `--output-format preserve` retains its current byte-for-byte relative-path behavior.
- Directory, regular-file, and stdin Storyline imports all write one Store at `OUTPUT`.
- The resulting physical Source is `.` and every normalized row has `_file_ = '.'`.
- Preserve `run_id`, `document_id`, and `session_id`; never prefix or rewrite identities.
- Reject duplicate `document_id` and duplicate `session_id` globally with both diagnostic Source paths.
- Source paths are diagnostics only and must not be added to Storyline or Lance schemas.
- Keep the serialized `ImportResponse` schema unchanged.
- Explicit `--max-input-bytes` remains per Source; add no aggregate directory limit.
- Publish only after all inputs and Store indexes succeed; leave no output on any failure.
- Do not modify TTAS, Queue, Search, or standalone `persisting-dlcapt`.
- The shared worktree already contains overlapping user edits in the target files. Preserve those edits and leave implementation changes unstaged rather than committing unrelated hunks.

## File map

- `crates/persisting-pchronicle-cli/src/exchange.rs`: split decode from physical staging, implement the lazy multi-Source Storyline iterator, global collision diagnostics, and one root Store write.
- `crates/persisting-pchronicle-cli/src/lib.rs`: update Clap help text; keep CLI types and response schema stable.
- `crates/persisting-pchronicle-cli/src/tests.rs`: replace old nested-layout assertions and add root Store, stdin, collision, and late-failure regression coverage.
- `crates/persisting-pchronicle-cli/README.md`: describe Storyline squash behavior.
- `docs/src/pchronicle/reference/cli.md`: document layout, response semantics, `_file_`, and collision policy.
- `docs/src/pchronicle/guides/exchange.md`: update the English import workflow.
- `docs/src/pchronicle/guides/exchange.zh.md`: update the Chinese import workflow.

---

### Task 1: Build one root Store for every Storyline input shape

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/tests.rs:1400-1690`
- Modify: `crates/persisting-pchronicle-cli/src/exchange.rs:1-155,594-805`

**Interfaces:**
- Consumes: existing `ImportFileCandidate`, `decode_json_storylines`, `read_bounded`, and `StorylineLanceStore::replace_storyline_stream`.
- Produces: `DecodedImportSource`, `StorylineImportInputs<'a>`, and `StorylineImportIterator<'a>` used by Task 2 for provenance-aware collision checks.

- [ ] **Step 1: Replace the single-file nested-layout test with a failing root-layout test**

Rename the test to `import_storyline_output_writes_one_root_lance_store` and assert both the root layout and query Source:

```rust
assert!(output.join("CURRENT").is_file());
assert!(output.join("generations").is_dir());
assert!(output.join("objects.lance").is_dir());
assert!(!output.join("session_steps.json").exists());

let cli = Cli::try_parse_from([
    "pchronicle",
    "query",
    output.to_str().unwrap(),
    "SELECT _file_ AS source_file, COUNT(*) AS runs FROM dataset.runs GROUP BY _file_",
    "--format",
    "jsonl",
])?;
let mut query_stdout = Vec::new();
run(cli, false, &mut query_stdout, &mut Vec::new()).await?;
let row: Value = serde_json::from_slice(&query_stdout)?;
assert_eq!(row["source_file"], ".");
assert_eq!(row["runs"], 2);
```

- [ ] **Step 2: Replace the directory per-Source test with a failing squash test**

Write two ATIF fixtures with distinct `trajectory_id` and `session_id`, retain the current response-count assertions, and replace nested `CURRENT` assertions with:

```rust
assert!(output.join("CURRENT").is_file());
assert!(!output.join("first/shared.json").exists());
assert!(!output.join("second/shared.json").exists());
```

Query grouped by `_file_` and assert one `.` row containing both runs. In `directory_import_reads_atif_jsonl_and_ndjson_in_both_output_modes`, keep the preserve assertion and change the Storyline assertion to `output.join("CURRENT").is_file()` plus absence of the nested path.

- [ ] **Step 3: Add a failing stdin Storyline root-layout test**

Add `storyline_import_from_stdin_writes_one_root_store` beside `import_reads_a_bounded_explicit_stdin_stream`:

```rust
let cli = Cli::try_parse_from([
    "pchronicle", "import", "--from", "-", "--stream", "--format", "atif",
    "--output", output.to_str().unwrap(), "--output-format", "storyline",
])?;
let mut stdin = input.as_slice();
run_with_stdin(cli, false, &mut stdin, &mut Vec::new(), &mut Vec::new()).await?;
assert!(output.join("CURRENT").is_file());
assert!(!output.join("trajectories.atif.json").exists());
```

- [ ] **Step 4: Run the layout tests and confirm RED**

Run:

```sh
cargo test -p persisting-pchronicle-cli \
  import_storyline_output_writes_one_root_lance_store -- --nocapture
cargo test -p persisting-pchronicle-cli \
  directory_storyline_output_squashes_sources_into_one_root_store -- --nocapture
cargo test -p persisting-pchronicle-cli \
  storyline_import_from_stdin_writes_one_root_store -- --nocapture
```

Expected: each test fails because `CURRENT` exists under an input-derived child path instead of `OUTPUT`.

- [ ] **Step 5: Separate Source decoding from preserve-file staging**

Replace the write-coupled result with a decoded unit:

```rust
struct DecodedImportSource {
    diagnostic_path: PathBuf,
    metadata: ImportedSource,
    storylines: std::vec::IntoIter<StorylineDocument>,
}

fn decode_import_source(
    requested_format: ExchangeFormat,
    output_format: ImportOutputFormat,
    input_path: Option<&Path>,
    decode_relative_path: Option<&Path>,
    logical_source_path: Option<&Path>,
    input: &[u8],
    unknown_field_warnings: &mut persisting_pchronicle::model::UnknownFieldImportWarnings,
) -> Result<DecodedImportSource>
```

Move UTF-8 conversion, format resolution, Storyline decoding, unknown-field observation, logical source naming, trajectory count, and byte count into this synchronous helper. Do not perform global duplicate checks here. Preserve imports call the existing `validate_import_storylines` before writing the original bytes.

Rename the physical writer to `stage_preserved_import_source`; it calls `decode_import_source`, creates the original relative path below staging, writes and syncs the exact bytes, and returns `ImportedSource`. Remove the Storyline Store branch from this helper.

- [ ] **Step 6: Add the lazy Storyline input iterator**

Implement the following input state and iterator in `exchange.rs`:

```rust
enum StorylineImportInputs<'a> {
    Stdin(Option<&'a mut dyn Read>),
    Files {
        candidates: &'a [ImportFileCandidate],
        next: usize,
    },
}

struct StorylineImportIterator<'a> {
    requested_format: ExchangeFormat,
    output_format: ImportOutputFormat,
    max_input_bytes: usize,
    inputs: StorylineImportInputs<'a>,
    current: std::vec::IntoIter<StorylineDocument>,
    current_diagnostic_path: PathBuf,
    imported_sources: Vec<ImportedSource>,
    unknown_field_warnings: persisting_pchronicle::model::UnknownFieldImportWarnings,
    failed: bool,
}

impl Iterator for StorylineImportIterator<'_> {
    type Item = Result<StorylineDocument>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(storyline) = self.current.next() {
                return Some(Ok(storyline));
            }
            if self.failed {
                return None;
            }
            match self.decode_next_source() {
                Ok(Some(decoded)) => {
                    self.current_diagnostic_path = decoded.diagnostic_path;
                    self.imported_sources.push(decoded.metadata);
                    self.current = decoded.storylines;
                }
                Ok(None) => return None,
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            }
        }
    }
}
```

Implement these helper signatures on the iterator:

```rust
fn stdin(
    requested_format: ExchangeFormat,
    max_input_bytes: usize,
    stdin: &mut dyn Read,
) -> StorylineImportIterator<'_>;

fn files(
    requested_format: ExchangeFormat,
    max_input_bytes: usize,
    candidates: &[ImportFileCandidate],
) -> StorylineImportIterator<'_>;

fn decode_next_source(&mut self) -> Result<Option<DecodedImportSource>>;

fn into_result_parts(
    self,
) -> (
    Vec<ImportedSource>,
    persisting_pchronicle::model::UnknownFieldImportWarnings,
);
```

`decode_next_source` matches `StorylineImportInputs`: the file branch increments `next`, opens the selected candidate, bounded-reads it with the existing `import source <relative path>` label, and passes its physical and relative paths to `decode_import_source`; the stdin branch `take()`s its reader, bounded-reads it with label `stdin`, and passes no physical or relative path. A consumed stdin or exhausted candidate slice returns `Ok(None)`.

- [ ] **Step 7: Dispatch Storyline output to one Store at staging root**

Refactor `run_import` so the preserve branch retains the current loop, while Storyline uses:

```rust
let store = StorylineLanceStore::open(staging.path())
    .await
    .context("create squashed Storyline Lance Dataset")?;
let mut import = if args.stream {
    StorylineImportIterator::stdin(args.format, max_input_bytes, stdin)
} else {
    StorylineImportIterator::files(args.format, max_input_bytes, &candidates)
};
let report = store
    .replace_storyline_stream(&mut import)
    .await
    .context("write squashed Storyline Lance Dataset")?;
anyhow::ensure!(
    store.current_table_paths().await?.is_some(),
    "squashed Storyline Lance Dataset has no committed snapshot"
);
let (imported_sources, unknown_field_warnings) = import.into_result_parts();
```

Check `report.storylines` against the checked sum in `imported_sources`. Keep the existing staging sync, no-replace rename, parent sync, response serialization, and warning output unchanged after this branch returns its metadata.

- [ ] **Step 8: Run the layout tests and preserve regressions**

Run:

```sh
cargo test -p persisting-pchronicle-cli \
  import_storyline_output_writes_one_root_lance_store -- --nocapture
cargo test -p persisting-pchronicle-cli \
  directory_storyline_output_squashes_sources_into_one_root_store -- --nocapture
cargo test -p persisting-pchronicle-cli \
  storyline_import_from_stdin_writes_one_root_store -- --nocapture
cargo test -p persisting-pchronicle-cli \
  directory_import_reads_atif_jsonl_and_ndjson_in_both_output_modes -- --nocapture
cargo test -p persisting-pchronicle-cli \
  import_recurses_directories_and_preserves_relative_source_paths -- --nocapture
```

Expected: all selected tests pass.

- [ ] **Step 9: Review the Task 1 diff without staging overlapping user work**

Run:

```sh
git diff --check -- \
  crates/persisting-pchronicle-cli/src/exchange.rs \
  crates/persisting-pchronicle-cli/src/tests.rs
git diff --stat -- \
  crates/persisting-pchronicle-cli/src/exchange.rs \
  crates/persisting-pchronicle-cli/src/tests.rs
```

Expected: no whitespace errors. Leave both files unstaged because they contained pre-existing changes before this plan.

### Task 2: Enforce global identity uniqueness with provenance-aware errors

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/tests.rs:1930-2020`
- Modify: `crates/persisting-pchronicle-cli/src/exchange.rs:594-805`

**Interfaces:**
- Consumes: `StorylineImportIterator<'a>` and its `current_diagnostic_path` from Task 1.
- Produces: `record_import_identity`, global document/session maps, and stable invalid-input diagnostics.

- [ ] **Step 1: Add failing cross-Source collision tests**

Add one table-driven test `storyline_squash_rejects_global_identity_collisions` with two cases. Construct minimal ATIF documents so the document case has equal `trajectory_id` and distinct `session_id`, while the session case has distinct `trajectory_id` and equal `session_id`. Store them as `first.json` and `nested/second.json`, import the directory as Storyline, and assert:

```rust
let message = format!("{error:#}");
assert!(message.contains(field), "{message}");
assert!(message.contains(value), "{message}");
assert!(message.contains("first.json"), "{message}");
assert!(message.contains("nested/second.json"), "{message}");
assert!(!output.exists());
```

Add a same-Source duplicate case and assert the same path appears in both labeled positions in the error.

- [ ] **Step 2: Run the collision test and confirm RED**

Run:

```sh
cargo test -p persisting-pchronicle-cli \
  storyline_squash_rejects_global_identity_collisions -- --nocapture
```

Expected: the old implementation either accepts duplicate sessions or emits the Store's generic duplicate-document error without both Source paths.

- [ ] **Step 3: Add document and session provenance maps**

Extend the iterator:

```rust
seen_document_ids: HashMap<String, PathBuf>,
seen_session_ids: HashMap<String, PathBuf>,
```

Import `HashMap` beside the existing `HashSet`. Add:

```rust
fn record_import_identity(
    seen: &mut HashMap<String, PathBuf>,
    field: &str,
    value: &str,
    diagnostic_path: &Path,
) -> Result<()> {
    if let Some(first_path) = seen.get(value) {
        return Err(cli_boundary_error(
            BoundaryCode::InvalidRequest,
            format!(
                "import contains duplicate {field} '{value}' in Sources '{}' and '{}'",
                first_path.display(),
                diagnostic_path.display()
            ),
        ));
    }
    seen.insert(value.to_owned(), diagnostic_path.to_path_buf());
    Ok(())
}
```

Before yielding each current Storyline, call it first with `story.document_id()` and then with `&story.session_id`. On error, set the iterator's terminal failure flag and yield the error exactly once. Do not apply these global maps to preserve output.

- [ ] **Step 4: Add a failing late-Source atomicity regression**

Add `storyline_squash_late_source_failure_removes_staging`. Write `a-valid.json` as an ATIF JSON array of 256 documents with unique trajectory/session identities, then write invalid JSON to `z-invalid.json`. Import as Storyline and assert the error names `z-invalid.json`, `OUTPUT` does not exist, and no sibling entry starts with `.pchronicle-import-`.

- [ ] **Step 5: Run collision and atomicity tests to GREEN**

Run:

```sh
cargo test -p persisting-pchronicle-cli \
  storyline_squash_rejects_global_identity_collisions -- --nocapture
cargo test -p persisting-pchronicle-cli \
  storyline_squash_late_source_failure_removes_staging -- --nocapture
cargo test -p persisting-pchronicle-cli \
  import_is_create_only_and_rejects_duplicate_documents -- --nocapture
cargo test -p persisting-pchronicle-cli \
  directory_import_failure_does_not_publish_partial_output -- --nocapture
```

Expected: all selected tests pass; the existing preserve-mode shared-session case remains accepted.

- [ ] **Step 6: Review the Task 2 diff without staging overlapping user work**

Run `git diff --check -- crates/persisting-pchronicle-cli/src/exchange.rs crates/persisting-pchronicle-cli/src/tests.rs` and leave the files unstaged.

### Task 3: Update CLI language and user documentation

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs:375-410`
- Modify: `crates/persisting-pchronicle-cli/README.md:94-108`
- Modify: `docs/src/pchronicle/reference/cli.md:168-205`
- Modify: `docs/src/pchronicle/guides/exchange.md:10-38`
- Modify: `docs/src/pchronicle/guides/exchange.zh.md:10-40`

**Interfaces:**
- Consumes: the Task 1 root Store behavior and Task 2 collision semantics.
- Produces: accurate help and documentation with no old per-Source Store promise.

- [ ] **Step 1: Add a failing Clap help assertion**

In `command_tree_contains_the_product_commands`, locate the `import` command and `output-format` argument, then assert its help contains `squash into one Storyline Lance Store at the Dataset root`.

- [ ] **Step 2: Update `ImportOutputFormat` and `ImportArgs` help**

Use these exact descriptions:

```rust
/// Decode all input Sources into one squashed Storyline Lance Store at the Dataset root.
Storyline,

/// Physical Dataset output: preserve source files, or squash into one Storyline Lance Store at the Dataset root.
#[arg(long, value_enum, default_value_t = ImportOutputFormat::Preserve)]
output_format: ImportOutputFormat,
```

- [ ] **Step 3: Update the README and reference contract**

Replace every statement that Storyline writes one Store per Source with the following facts:

- all decoded Sources feed one Store at the output root;
- the result has one physical Source `.` and `_file_ = '.'`;
- `sources` in the import response still counts logical inputs;
- `document_id` and `session_id` must be globally unique;
- original paths remain available in import errors but are not query provenance;
- use preserve mode when Source boundaries matter.

- [ ] **Step 4: Update both exchange guides symmetrically**

Show the same `--output-format storyline` command but describe one squashed root Store. Add a short query example that omits `--source`, and state that `_file_` is `.` after squash. The Chinese guide must convey the same collision and provenance policy as the English guide.

- [ ] **Step 5: Run help and stale-language checks**

Run:

```sh
cargo test -p persisting-pchronicle-cli command_tree_contains_the_product_commands -- --nocapture
rg -n "one normalized Storyline Lance store per Source|one Storyline Lance store at each|each Source into its own|每个 Source.*Storyline|独立 Storyline" \
  crates/persisting-pchronicle-cli/README.md \
  docs/src/pchronicle/reference/cli.md \
  docs/src/pchronicle/guides/exchange.md \
  docs/src/pchronicle/guides/exchange.zh.md
```

Expected: the help test passes and `rg` returns no stale old-layout claim.

- [ ] **Step 6: Review documentation diffs without staging overlapping user work**

Run `git diff --check` for the five Task 3 files and leave them unstaged.

### Task 4: Focused verification and real-directory smoke test

**Files:**
- Verify only; no planned source edits.

**Interfaces:**
- Consumes: all behavior and documentation from Tasks 1-3.
- Produces: evidence that the implementation passes focused checks and the reported user workflow creates one root Store.

- [ ] **Step 1: Format the touched Rust package**

Run:

```sh
cargo fmt -p persisting-pchronicle-cli
cargo fmt -p persisting-pchronicle-cli -- --check
```

Expected: both commands exit zero and do not format excluded packages.

- [ ] **Step 2: Run the complete CLI test suite**

Run:

```sh
cargo test -p persisting-pchronicle-cli
```

Expected: all CLI unit and integration tests pass. If a pre-existing unrelated test fails, rerun that test against the pre-change state or otherwise establish evidence before classifying it as unrelated.

- [ ] **Step 3: Run focused Clippy**

Run:

```sh
cargo clippy -p persisting-pchronicle-cli --all-targets -- -D warnings
```

Expected: exit zero with no warnings.

- [ ] **Step 4: Build release and smoke-test the user's directory**

Run:

```sh
cargo build -p persisting-pchronicle-cli --release
smoke_parent="$(mktemp -d)"
target/release/pchronicle import \
  --format actf \
  --from data/caiyuxuan/debug/ \
  --output "$smoke_parent/test" \
  --output-format storyline
test -f "$smoke_parent/test/CURRENT"
test ! -e "$smoke_parent/test/terminal_bench_2_1"
target/release/pchronicle query "$smoke_parent/test" \
  "SELECT _file_ AS source_file, COUNT(*) AS runs FROM dataset.runs GROUP BY _file_" \
  --format jsonl
```

Expected: import succeeds, the only query row has `source_file` equal to `.` and a positive run count, and no input-derived hierarchy exists. Keep the printed temporary path until results are recorded; remove only that exact `mktemp` directory afterward.

- [ ] **Step 5: Inspect final scope and working-tree safety**

Run:

```sh
git diff --check
git status --short
git diff --stat -- \
  crates/persisting-pchronicle-cli/src/exchange.rs \
  crates/persisting-pchronicle-cli/src/lib.rs \
  crates/persisting-pchronicle-cli/src/tests.rs \
  crates/persisting-pchronicle-cli/README.md \
  docs/src/pchronicle/reference/cli.md \
  docs/src/pchronicle/guides/exchange.md \
  docs/src/pchronicle/guides/exchange.zh.md
```

Expected: no whitespace errors, no excluded subsystem changes attributable to this implementation, and all pre-existing user modifications remain present and unstaged.

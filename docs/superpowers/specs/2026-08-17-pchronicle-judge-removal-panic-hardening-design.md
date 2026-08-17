# pChronicle Judge Removal and Panic Hardening Design

## Goal

Remove the complete pChronicle judgment vertical slice, eliminate production
`unwrap` and `expect` calls from the pChronicle library, and enforce strict
Clippy warnings in the active workspace.

## Scope

This change covers:

- `persisting-pchronicle` judgment execution, persistence, aggregation,
  protocol types, trajectory adapters, and public exports;
- `persisting-pchronicle-cli` judgment HTTP endpoints, Explorer projections,
  and their tests;
- the twelve `clippy::unwrap_used` or `clippy::expect_used` findings compiled
  by the default-feature `persisting-pchronicle` library target;
- the existing pVisor `clippy::type_complexity` finding that blocks workspace
  `-D warnings`;
- local and CI Clippy commands for the active workspace.

The standalone `persisting-dlcapt` component remains governed by its existing
strict workflow. Search and other subsystems excluded by `AGENTS.md` are not
enabled, modified, or included in the acceptance criteria.

## Judge Removal Boundary

The judgment capability is removed as one vertical slice rather than hidden
behind a feature flag or retained as a compatibility layer.

The implementation will remove:

- `judgment.rs`, `judge_service.rs`, and `judgment_summary.rs`;
- the trajectory `judge` and `judge_stats` adapters;
- judgment request, response, score, summary, scope, and method message types;
- judgment fields from ordinary trajectory statistics;
- public modules and re-exports for judgment behavior and storage;
- the `judgments.lance` path helper and judgment-specific layout exports;
- pChronicle's direct `reqwest` dependency;
- the CLI server's `/api/judgments` endpoint;
- judgment loading, aggregation, and presentation in Explorer responses;
- judgment-specific server tests and judgment-specific catalog fixture names.

Existing `judgments.lance` directories on disk are left untouched. The new
code contains no judgment-specific discovery or interpretation and does not
read, write, report, or delete them. Generic catalog traversal may still see
an unknown derived Lance directory and must continue to exclude it from
canonical trajectory discovery. This avoids a destructive migration while
making the runtime and public API removal complete.

This is an intentional breaking API change. No deprecated aliases, empty
response fields, or always-failing judgment entry points will remain.

## Production Panic Hardening

The production library target currently has twelve Clippy findings: ten
`expect` calls and two `unwrap` calls. Tests contain many more assertion-oriented
uses and are not part of this production-panic acceptance criterion.

The findings will be removed by behavior-preserving control flow:

- ACTF provenance and serialized-object assumptions become explicit errors;
- ACTF observation reference validation binds the present identifier directly;
- OpenAI corpus rows that violate the validated-object invariant return a
  conversion error instead of panicking;
- revision JSON serialization is collected as a fallible operation;
- catalog single-plan selection uses an explicit checked branch;
- index-build admission returns a fallible result and callers propagate a
  closed-semaphore error;
- a poisoned root-lock registry recovers the contained map instead of panicking.

No `allow` or `expect` lint annotations will be introduced for these findings.
The target state is zero `clippy::unwrap_used` and zero
`clippy::expect_used` diagnostics for:

```bash
cargo clippy -p persisting-pchronicle --lib --locked -- \
  -D clippy::unwrap_used -D clippy::expect_used
```

## Strict Clippy Policy

The ordinary Rust lint command and the main CI lint job will treat every
Clippy warning as an error. The obsolete comments describing strict Clippy as
unsafe to run will be removed, and the compatibility target will delegate to
the strict command rather than maintaining a weaker path.

The existing pVisor trajectory sink tuple will be replaced with a named type
alias or focused struct so `clippy::type_complexity` is fixed without a lint
suppression.

A separate pChronicle production-panic lint target will run the command above.
It deliberately checks `--lib`, not `--all-targets`, so tests may continue to
use `unwrap` and `expect` as concise assertion helpers. Normal `-D warnings`
still applies to tests through the workspace all-target Clippy command.

## Error and Compatibility Semantics

Malformed external or persisted conversion input must return the existing
pChronicle error type or an `anyhow::Error` at the owning storage boundary.
Internal concurrency failures must be propagated where recovery is not safe.
Mutex poisoning in the process-local lock registry is recoverable because the
registry stores only weak lock references; recovering the map does not bypass
the per-dataset asynchronous lock or cross-process storage fencing.

Removing the `judge` field from `TrajectoryStatsResponse` and removing
judgment HTTP response fields is intentionally not wire-compatible. Consumers
must stop requesting or decoding judgment data.

## Verification

The implementation is accepted when all of the following hold:

1. No judgment modules, public types, routes, Explorer fields, or direct
   pChronicle `reqwest` dependency remain.
2. Existing judgment files are not deleted by code or migration scripts.
3. The pChronicle library passes the production panic Clippy command.
4. pChronicle and pChronicle CLI targeted tests pass after obsolete judgment
   tests are removed and affected stats/Explorer fixtures are updated.
5. The active workspace passes all-target Clippy with `-D warnings`, excluding
   only `persisting-dlcapt`, which retains its separate strict workflow.
6. Rust formatting checks pass for all modified Rust files.

## Non-Goals

- Extracting judgment into a replacement crate.
- Preserving read-only access to historical judgment datasets.
- Deleting or migrating historical judgment data.
- Refactoring pChronicle's broader public facade or splitting its store,
  formats, conversion, or query subsystems.
- Cleaning `unwrap` or `expect` calls in tests, Search, `persisting-dlcapt`, or
  other crates.

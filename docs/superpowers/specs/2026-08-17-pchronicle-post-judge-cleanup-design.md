# pChronicle Post-Judge Cleanup Design

## Goal

Finish the bounded cleanup selected after removing pChronicle's judge subsystem:
remove orphaned Web CSS, replace the remaining production `unreachable!` calls
with explicit errors, and verify supported non-search feature combinations in
local and CI lint gates.

## Scope

This change includes only:

- pChronicle Web CSS class selectors with no static consumer anywhere under
  `pchronicle-web/src`;
- the four production `unreachable!` sites currently reported by
  `clippy::unreachable` in default-feature pChronicle builds;
- pChronicle feature checks for core-only, local Lance, default S3, and OSS
  builds;
- the local `just` lint entry point and the matching GitHub Actions lint step.

Search, TTAS, queues and samplers, and `persisting-dlcapt` remain out of scope.
The source-only `pc2-token-composition` class is also out of scope because it is
not an orphaned CSS selector and removing or styling it would be a UI decision.

## Web CSS Cleanup

The cleanup is conservative and token-based. A `.pc2-*` selector is removable
only when its class token does not occur in any Rust source under
`pchronicle-web/src`. This removes the four definite judge remnants
(`pc2-verdict-grid`, `pc2-verdict`, `pc2-rubric-list`, and `pc2-score-track`)
and the other selectors left behind by superseded Explorer layouts.

No retained selector is renamed, no layout value is changed, and no source
markup is changed. Verification repeats the cross-file class-token comparison
and runs the Web test suite and build checks.

## Explicit Error Semantics

Each remaining production `unreachable!` becomes an ordinary error at the
same abstraction boundary:

- projection sync reports incompatible non-canonical lineage;
- local manifest construction rejects an unsupported query format;
- Catalog projection binding reports a source-kind inconsistency;
- OpenAI corpus recovery rejects an unknown retained document kind.

The successful path remains unchanged. After conversion, the pChronicle panic
lint also denies `clippy::unreachable`, alongside `unwrap_used` and
`expect_used`.

## Feature Matrix

The local lint entry point and CI verify these non-search configurations:

1. `--no-default-features` for the lightweight format/event surface;
2. `--no-default-features --features lance-store` for local storage;
3. default features for the S3-compatible product build;
4. `--no-default-features --features oss-store` for the OSS backend.

Each variant builds the library with warnings denied and the production panic
lints enabled. The workspace-wide strict Clippy command remains the first gate.
CI invokes the same `just lint-rust` recipe used locally, avoiding two copies of
the feature policy.

## Verification

- The CSS/source class-token difference contains no CSS-only `.pc2-*` class.
- `clippy::unwrap_used`, `clippy::expect_used`, and `clippy::unreachable` are
  clean for all four supported pChronicle feature combinations.
- Workspace strict Clippy remains green with `persisting-dlcapt` excluded.
- pChronicle library, pChronicle CLI, and pChronicle Web tests remain green.
- Existing user-owned untracked review and RFC files remain untouched.


# Engineering Notes

These notes track implementation work that is useful to contributors, but is
not part of the product contract. They may describe unfinished work, internal
experiments, or test plans.

## Current notes

| Note | Audience | Purpose |
|---|---|---|
| [Releasing `persisting`](releasing.md) | Maintainers | Version, trusted-publisher, and stable release procedure |

## Fast local builds

The repository `rust-toolchain.toml` selects stable for normal compiler validation
and opt-in diagnostics. Normal development, test, and release builds all use
the toolchain's default LLVM backend.

Rust tests use `cargo nextest` for process isolation and parallel test
execution; install version `0.9.137` with
`cargo install cargo-nextest --version 0.9.137 --locked`, or use the
repository CI setup action.

The project always uses the platform's default linker so that builds remain
portable across developer machines and CI.

`just dev` is intentionally scoped to runtime crates and a no-default-feature
pChronicle check. Use `just gate` or the CI workflows for the full workspace,
all-targets, and storage-feature matrix.

`cargo nextest` does not run doctests. Keep documentation tests on the regular
Cargo runner when needed, for example `cargo test --doc -p <package>`.

### Nightly diagnostics (opt-in)

The repository keeps three expensive/nightly diagnostics out of the normal
edit loop:

- `just build-analysis persisting-pvisor` enables Cargo's `-Z build-analysis`
  for one package and writes per-session JSONL metrics under `$CARGO_HOME/log`.
  Inspect them with `just build-analysis-report` (or pass `report=timings` or
  `report=rebuilds`). The dedicated target directory prevents diagnostic
  artifacts from polluting the normal incremental cache.
- `just sanitize address persisting-agentctl` runs the selected crate's tests
  with LLVM AddressSanitizer. The recipe uses `-Z build-std`, so the nightly
  `rust-src` component is required. Other supported values are `leak`,
  `thread`, and `undefined`; availability depends on the host platform.

Sanitizer builds are deliberately not part of `just dev`/CI's default path:
they rebuild the standard library and are intended for focused debugging
sessions.

For supported behavior, start with [pVisor Guides](../pvisor/guides/index.md) or
[pChronicle Guides](../pchronicle/guides/index.md), and consult
[System Design](../system-design/index.md) for the rationale behind an
implementation.

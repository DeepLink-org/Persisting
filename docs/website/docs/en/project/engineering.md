# Engineering Notes

These notes track repository delivery work that is useful to contributors, but
is not part of the product contract. Product implementation status and
roadmap details belong to each product's Design pages.

## Contributor commands

Run these from the repository root. `just --list` shows the full recipe set.

| Command | What it does |
|---|---|
| `just test` | Workspace Rust tests through `cargo nextest`, then the Python suite |
| `just test <package>` | One crate or Cargo package (for example `pvisor` or `persisting-pvisor`) |
| `just docs-sync` | Install the locked documentation environment |
| `just docs-serve` | Local MkDocs preview with automatic reload when files change |
| `just docs-serve-dirty` | Preview with `--dirtyreload` when auto-reload stalls |
| `just docs-build` | Build the static documentation site |
| `just docs-links` | Strict MkDocs build that fails on dead links |
| `just examples` | All product example suites (pVisor, pChronicle, and pPilot) |
| `just gate` | Format, lint, and the full Rust test workspace |
| `just dev` | Scoped runtime-crate check; not the full workspace matrix |

`just test` uses the debug nextest profile for faster iteration. Pass a Cargo
package name or a short crate alias (`pvisor`, `ppilot`, `pchronicle`,
`pchronicle-cli`, `agentctl`, `capture`). The no-argument form also runs
`just test-py`.

## Current notes

| Note | Audience | Purpose |
|---|---|---|
| [Releasing `persisting`](releasing.md) | Maintainers | Version, trusted-publisher, and stable release procedure |
| [Reproducible examples](examples.md) | Contributors | Product CLI suites under `examples/` |

## Fast local builds

The repository `rust-toolchain.toml` selects stable for normal compiler validation
and opt-in diagnostics. Normal development, test, and release builds all use
the toolchain's default LLVM backend.

Rust tests use `cargo nextest` for process isolation and parallel test
execution; install version `0.9.137` with
`cargo install cargo-nextest --version 0.9.137 --locked`, or use the
repository CI setup action.

Local and ordinary CI builds use the platform's default linker. Linux wheels
use the manylinux_2_28 image (glibc 2.28) so rustc libstd and libkrun can
link `statx` / `copy_file_range`.

`just dev` is intentionally scoped to runtime crates and a no-default-feature
pChronicle check. Use `just gate` or the CI workflows for the full workspace,
all-targets, and storage-feature matrix.

`cargo nextest` does not run doctests. Keep documentation tests on the regular
Cargo runner when needed, for example `cargo test --doc -p <package>`.

### Nightly diagnostics (opt-in)

The repository keeps two expensive/nightly diagnostics out of the normal
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

For supported behavior, start with [pVisor Guides](../pvisor/guides/index.md),
pPilot orchestration, or
[pChronicle Guides](../pchronicle/guides/index.md), and consult
[System Design](../system-design/index.md) for the rationale behind an
implementation.

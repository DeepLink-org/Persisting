# Engineering Notes

These notes track repository delivery work that is useful to contributors, but
is not part of the product contract. Product implementation status and
roadmap details belong to each product's Design pages.

## Contributor commands

Run these from the repository root. `just --list` shows the full recipe set.

| Command | What it does |
|---|---|
| `just test` | Workspace Rust tests through `cargo nextest`, then the Python suite |
| `just test <package>` | One crate or Cargo package (for example `pchronicle` or `persisting-pchronicle`) |
| `just docs-sync` | Install the locked documentation environment |
| `just docs-serve` | Local Zensical preview with automatic reload when files change |
| `just docs-serve-dirty` | Local Zensical preview when automatic reload stalls |
| `just docs-build` | Build the static documentation site |
| `just examples` | pChronicle product example suite |
| `just dev` | Apply formatting, lint, then run Rust tests |
| `just ci` | Check lint without rewriting files, run Rust/Python tests and property tests, then build |
| `just check-quick` | Check core runtime crates and pChronicle without default features |

`just test` uses the debug nextest profile for faster iteration. Pass a Cargo
package name or a short crate alias (`pchronicle`,
`pchronicle-cli`, `agentctl`, `capture`). `just test pchronicle` runs both
`persisting-pchronicle` and `persisting-pchronicle-cli` (same as the CI
pchronicle shard); use `just test pchronicle-cli` for the CLI crate alone.
The no-argument form also runs `just test-py`.

## Current notes

| Note | Audience | Purpose |
|---|---|---|
| [Releasing `persisting`](releasing.md) | Maintainers | Version, trusted-publisher, and stable release procedure |
| [Reproducible examples](examples.md) | Contributors | Product CLI suites under `examples/` |

## Fast local builds

The repository `rust-toolchain.toml` selects stable with rustfmt and Clippy. Normal development, test, and release builds all use
the toolchain's default LLVM backend.

Rust tests use `cargo nextest` for process isolation and parallel test
execution; install version `0.9.137` with
`cargo install cargo-nextest --version 0.9.137 --locked`, or use the
repository CI setup action.

Local and ordinary CI builds use the platform's default linker. Linux wheels
use the manylinux_2_28 image (glibc 2.28) so rustc libstd can
link `statx` / `copy_file_range`.

`just check-quick` checks core runtime crates and pChronicle without default
features. `just dev` formats files before lint and Rust tests; `just ci` uses
read-only lint checks and also runs Python and property tests. The GitHub Actions
workflow additionally covers platform shards, Web builds, S3, and examples.

`cargo nextest` does not run doctests. Keep documentation tests on the regular
Cargo runner when needed, for example `cargo test --doc -p <package>`.

### CI build reuse

MinIO tools are cached by OS, architecture, Go version, and pinned source releases.
Benchmark revisions run sequentially on one runner and reuse a Cargo target
directory; their reports are saved separately. Wheel builds cache both Rust
workspaces, and the embedded Web resource fingerprint includes its lockfile.

Python-only jobs skip Rust installation. Build-only jobs skip cargo-nextest.
Documentation runs use a per-ref concurrency group so a PR cannot cancel a
main-branch documentation deployment.

For supported behavior, start with [pChronicle Guides](../pchronicle/guides/index.md), and consult
[System Design](../system-design/index.md) for the rationale behind an
implementation.

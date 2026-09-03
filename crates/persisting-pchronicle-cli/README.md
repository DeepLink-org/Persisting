# pChronicle CLI

**Standalone `pchronicle` CLI for onboarding, browsing, querying, importing,
exporting, and serving trajectory Datasets.**

Owns the `pchronicle` binary, loopback-only Warehouse HTTP, the write-capable
`--control` plane used by pPilot and pVisor, optional Gateway ingest/forwarding
flags, and the embed of staged `pchronicle-web` assets at build time.

Does not own trajectory models, Lance storage, Catalog, or the query engine —
those live in [`persisting-pchronicle`](../persisting-pchronicle/README.md).
Does not own the Web UI source —
[`pchronicle-web`](../../pchronicle-web/README.md) does. Does not start, schedule,
or isolate Agent Runs (pVisor / pPilot).

Current commands include `onboard`, `default`, `alias`, `ls`/`list`, `status`,
bounded read-only `query`, built-in `analysis`, assisted `agent` sessions,
Source-local `find`, create/append/replace `import`, destructive `drop`,
complete-trajectory `export`, directory `sync`, `echo`, and loopback-only
`serve`. Import and export support ATIF, OpenAI Messages, ACTF, and Storyline
JSON. `sync --from SOURCE --to WAREHOUSE --convert OUTPUT` polls a local source
directory, atomically mirrors supported JSON files into a local Warehouse
Dataset byte-for-byte, and rebuilds a Storyline Lance Dataset at the conversion
output on each coalesced batch; use `--once` for a finite run.

`pchronicle serve --control 127.0.0.1:0 URI` is normally launched by pPilot or
pVisor. `serve --listen` is the read-only Warehouse. Public bind addresses are
rejected. Small deterministic Datasets live in
[`../../examples/data`](../../examples/data).

## Develop

The CLI test suite is split by contract. All format-dependent integration tests
share `tests/common/mod.rs`; add new formats to that fixture catalog so the
command matrix cannot silently omit them.

| Layer | Contract | Test target |
|---|---|---|
| Unit | parsing, validation, bounds, atomic writes | `--lib` |
| Format matrix | every Warehouse fixture × catalog/query/import/export | `--test command_matrix` |
| Round trip | exact bytes and forced Storyline conversion | `--test import_export_roundtrip` |
| Process | exit codes and stdout/stderr separation | `--test binary_contract` |
| Local Warehouse | persistent default and serverless command workflow | `--test local_warehouse` |
| Built-in analysis | overview/Agent/Model/tool semantics and bounds | `--test analysis` |

```bash
just test persisting-pchronicle-cli
# or: just test-crate pchronicle-cli
cargo nextest run -p persisting-pchronicle-cli --tests --locked
just chronicle-binary
```

`just chronicle-web-build` stages the Dioxus assets that this crate embeds
(`web-assets/public`, with `web-fallback` when the staged tree is absent).

## Links

- [pChronicle get started](../../docs/src/pchronicle/get-started.md)
- [pChronicle CLI reference](../../docs/src/pchronicle/reference/cli.md)
- [Local read-only Dataset server](../../docs/src/pchronicle/guides/serve.md)
- [Gateway forwarding, rewriting, and capture](../../docs/src/pchronicle/guides/serve-gateway.md)
- [`persisting-pchronicle`](../persisting-pchronicle/README.md)
- [`pchronicle-web`](../../pchronicle-web/README.md)

# pChronicle CLI

Standalone command-line interface for browsing, querying, importing, exporting,
and serving pChronicle trajectory Datasets.

The current implementation provides `default`, `ls`/`list`, `status`, bounded
read-only `query`, built-in `analysis`, Source-local `find`, create-only
`import`, complete-trajectory `export`, and loopback-only `serve`. Import and
export support ATIF, OpenAI Messages, ACTF, and Storyline JSON. `search` and
`maintain` are reserved in the command tree but currently return a clear
not-implemented error.

## Local Warehouse

Set one local directory as the default Warehouse once:

```bash
pchronicle default ./trajectory-data
pchronicle default
```

The first command creates the directory when needed, stores its normalized
absolute path in the user settings, and prints it. The second command reports
the current value. Once configured, the path can be omitted from local read
commands:

```bash
pchronicle ls
pchronicle status
pchronicle query "SELECT * FROM dataset.runs"
pchronicle find --session-id session-42
pchronicle export --output runs.json --format storyline
```

Built-in analyses use the same default Warehouse and normalized logical tables:

```bash
pchronicle analysis overview
pchronicle analysis agents
pchronicle analysis models
pchronicle analysis tools
```

- `overview` reports Source readiness and total trajectories, Steps, Agents,
  Models, and tool calls;
- `agents` groups activity by Agent identity and version;
- `models` combines declared trajectory models with observed Step models;
- `tools` groups calls by normalized function name and reports duration
  coverage.

All analyses accept an optional explicit Dataset URI, `--format
table|jsonl|csv`, and a bounded `--limit`. Use `query` for custom or larger
analyses; `analysis` intentionally does not accept arbitrary SQL.

File imports can also omit `--output`; the CLI derives a create-only Dataset
subdirectory under the default Warehouse from the input file name:

```bash
pchronicle import --from ./training.json
```

An explicit Dataset URI still takes precedence. This basic Warehouse is just a
recursive local Dataset root: it has no server, authentication, background
process, or hidden database. Use global `--settings FILE` or the
`PCHRONICLE_SETTINGS` environment variable to isolate its settings in tests or
automation.

The read-only server uses a separate static mount configuration:

```toml
[[datasets]]
name = "evals"
uri = "../data/atif"
```

Use `--listen 127.0.0.1:8080` to select the local address and `--open` to open
the UI after the listener is ready. Public bind addresses are rejected because
this local Warehouse surface does not provide authentication. Relative local
Dataset paths are resolved from the directory containing `warehouse.toml`.

Small ATIF, OpenAI Messages, and ACTF Datasets for trying the commands live in
[`../../examples/data`](../../examples/data).

## Tests

The CLI test suite is split by contract:

- unit tests in `src/lib.rs` cover parsing, validation, encoding, limits, and
  failure atomicity;
- `tests/command_matrix.rs` applies the same catalog and exchange workflow to
  every supported example format;
- `tests/import_export_roundtrip.rs` verifies exact and forced-Storyline
  round trips;
- `tests/binary_contract.rs` verifies the executable's exit codes and its
  stdout/stderr boundary.

All format-dependent integration tests share `tests/common/mod.rs`. Add new
formats to that fixture catalog so the command matrix cannot silently omit
them.

| Layer | Contract | Test target |
|---|---|---|
| Unit | parsing, validation, bounds, atomic writes | `--lib` |
| Format matrix | every Warehouse fixture × catalog/query/import/export | `--test command_matrix` |
| Round trip | exact bytes and forced Storyline conversion | `--test import_export_roundtrip` |
| Process | exit codes and stdout/stderr separation | `--test binary_contract` |
| Local Warehouse | persistent default and serverless command workflow | `--test local_warehouse` |
| Built-in analysis | overview/Agent/Model/tool semantics and bounds | `--test analysis` |

Run the complete CLI gate with:

```bash
cargo test -p persisting-pchronicle-cli --tests
```

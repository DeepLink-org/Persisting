# pChronicle CLI

Standalone command-line interface for browsing, querying, importing, exporting,
and serving pChronicle trajectory Datasets.

The current implementation provides `pchronicle ls` (also available as
`pchronicle list`), `pchronicle status`, bounded read-only `pchronicle query`,
Source-local `pchronicle find`, and create-only local `pchronicle import` for
ATIF, OpenAI Messages, and ACTF files. Import also accepts a finite stdin stream
when `--from - --stream --format FORMAT` is explicit. `pchronicle export`
selects complete Trajectories from one immutable Catalog Snapshot and writes
ATIF, OpenAI Messages, ACTF, or Storyline JSON to a new local file or finite
stdout stream. `pchronicle serve --config warehouse.toml` mounts a static set
of Datasets behind a loopback-only, read-only API and embedded Web UI. Other
commands are present in the command tree and return a clear not-yet-implemented
error until their respective product increments land.

```toml
default_dataset = "evals"

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

Run the complete CLI gate with:

```bash
cargo test -p persisting-pchronicle-cli --tests
```

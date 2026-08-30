# pChronicle CLI

Standalone command-line interface for onboarding, browsing, querying, importing,
exporting, and serving pChronicle trajectory Datasets.

The current implementation provides `onboard`, `default`, `alias`, `ls`/`list`, `status`,
bounded read-only `query`, built-in `analysis`, assisted `agent` sessions,
Source-local `find`, create-only `import`, complete-trajectory `export`, and
loopback-only `serve`. Import and export support ATIF, OpenAI Messages, ACTF,
and Storyline JSON.

## Orchestrator control plane

`pchronicle serve --control 127.0.0.1:0 URI` starts the write-capable
storage control plane used by pPilot and pVisor. Pass multiple positional
`[NAME=]DATASET` values to mount several read-only Datasets; `--control` still requires a Dataset named
`default`. It owns Run lease acquisition
and renewal, fencing, terminal commits, Attempt registry access, and trajectory
append. The process publishes one structured readiness record through stdout,
including the bound loopback endpoint and one-time token, then serves versioned
authenticated requests until its parent exits.

This form of `serve` is normally launched automatically. Use
`--pchronicle-binary PATH` or `PERSISTING_PCHRONICLE_BIN` on pPilot to select
the executable. It does not start the read-only Warehouse unless `--listen` is
also supplied.

## Guided onboarding

Start with the built-in deterministic ATIF example, or apply the same walkthrough
to an existing Dataset:

```bash
pchronicle onboard
pchronicle onboard ./my-trajectories
pchronicle onboard query ./my-trajectories
```

The complete walkthrough covers concepts, catalog/status inspection, built-in
analysis, schema discovery, Step and tool-call SQL, unified FTS/JSONB `find`
queries, cross-format queries, Source-local ID lookup, Storyline Lance import
and export, and the read-only Web/API boundary. Jump directly to one section with:

```text
concepts  inspect  analyze  query  formats  find  exchange  serve
```

On a terminal, `onboard` renders its Markdown guide with terminal styling. When
stdout is redirected or piped, it emits the original Markdown without ANSI
escapes. `NO_COLOR` and `TERM=dumb` disable ANSI styling. The built-in example
uses temporary ATIF, ACTF, and OpenAI Messages Datasets. The Warehouse and
import/export exercise uses isolated settings. All are removed when the command
exits and do not read or modify the user's default Dataset or configuration.

## Default Dataset

Set one local directory as the default Dataset once:

```bash
pchronicle default set ./trajectory-data
pchronicle default show
```

The first command creates the directory when needed, stores its normalized
absolute path in the user config, and prints it. The second command reports
the current value. Once configured, the path can be omitted from local read
commands:

```bash
pchronicle ls
pchronicle status
pchronicle query --sql "SELECT * FROM dataset.runs"
pchronicle find --session-id session-42
pchronicle find --match "timeout" --format json
pchronicle find --match "$.answer=yes" --format json
pchronicle export --from ./trajectory-data --to runs.json --output-format storyline
```

Built-in analyses use the same default Dataset and normalized logical tables:

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

## Analyze with Codex or Claude

Launch an interactive coding Agent with a pChronicle analysis prompt and an
ephemeral Dataset skill:

```bash
pchronicle agent codex ./trajectory-data
pchronicle agent claude s3://bucket/evals
pchronicle agent codex ./trajectory-data \
  --ask "Compare successful and failed tool calls"
pchronicle agent claude ./trajectory-data \
  --ask "Compare model latency" --no-overview
pchronicle agent codex ./trajectory-data --dry-run
pchronicle agent codex
```

When the Dataset position is omitted, `agent` uses the configured default Dataset.
Local paths are normalized before launch. The child process inherits the
caller's working directory, terminal, authentication, and unrelated Agent
settings; the Dataset is not made the working directory. Codex receives a
session-only `skills.config` override that selects the temporary skill. Claude
receives a temporary plugin and appended system prompt. No persistent Agent
configuration file is changed. Interactive launch requires terminal stdin and
stdout; `--dry-run` remains available in pipes and CI.

By default, pChronicle launches the Agent immediately without querying the
Dataset, then waits for an investigation request. `--ask` supplies that question
at launch so analysis can continue without a second user turn; only the bounded
queries needed for that request are run. `--no-overview` is retained for
compatibility and has no effect because generic startup queries are deferred.
`--dry-run` emits a JSON launch plan without creating a temporary
injection, checking Agent installation or authentication, or launching a child.
The plan marks the question as redacted and reports its byte length without
echoing its content.

The injected skill exposes the normalized URI and current `pchronicle`
executable, then guides the Agent through bounded `status`, `analysis`, `find`,
and read-only `query` calls. This is Agent guidance, not a filesystem or network
sandbox: the child retains its existing tool permissions and credentials. Other
environment variables are inherited, while `PCHRONICLE_DATASET_URI` and
`PCHRONICLE_BIN` are set for the session. The normalized Dataset URI, current
executable, analysis guidance, and `--ask` text are model-visible; pChronicle
command results used during analysis also become model-visible. Do not populate
`--ask` with unreviewed Dataset or ticket content.

The Agent is instructed to treat Dataset content as untrusted evidence, default
to small query budgets, and retain Snapshot and Source-local identity context in
conclusions. The temporary skill or Claude plugin is removed after a normal
Agent exit. Native Agent resume commands do not guarantee that this ephemeral
injection remains available. A forcibly terminated Codex launcher can leave a
generic `pchronicle-agent-*` skill directory under the Codex skill root; it can
be removed once no matching session is running.

Canonical imports name both ends explicitly:

```bash
pchronicle import --from ./training.json --to ./trajectory-data/training
```

Directory inputs recursively import `.json`, `.jsonl`, and `.ndjson` Sources
while retaining relative paths in the default byte-preserving output.
`--output-format storyline` instead squashes every decoded Source into one
normalized Storyline Lance Store at the Dataset root. The result has one
physical Source named `.`, so `_file_` is `.` for all normalized rows while the
response `sources` count still reports the number of logical inputs. Squashing
requires globally unique `document_id` and `session_id` values; collision
errors name both input paths, but successful Stores do not retain those paths
as query provenance. Use preserve output when Source boundaries matter.

A validated non-empty canonical `events.lance` input is detected before JSON
scanning and always creates Storyline Lance. The operation accepts local or
object-store URIs, never mutates the source, and refuses an existing target.
Its response reports `fact_rows` and omits `input_bytes`; explicit
`--output-format preserve` is invalid for canonical events.

`serve [NAME=]DATASET...` converges deterministic sibling Storyline projections
before readiness and maintains them as canonical events are appended. Each
positional Dataset becomes a mount. Runtime
failures are retried without blocking durable writes. `status URI --format
json` reports each projection's state and watermark.

An explicit Dataset URI still takes precedence. This basic Warehouse is just a
recursive local Dataset root: it has no server, authentication, background
process, or hidden database. Use global `--config FILE` or the
`PCHRONICLE_CONFIG` environment variable to isolate its config in tests or
automation.

Runtime errors are concise by default. Pass global `--log-level debug`
to include the complete source chain for local diagnosis; successful list and
status output always uses fixed source-status messages and never includes those
diagnostics.

Pass one or more positional `[NAME=]DATASET` mounts. Use
`--listen 127.0.0.1:8080` to select the local address and `--open` to open the
UI after the listener is ready. Public bind addresses are rejected because
this local Warehouse surface does not provide authentication.

### Config-free ingest Gateway

Use `--gateway` with an address and an output Dataset URI. The Dataset is
mounted automatically, so it does not need to be repeated positionally:

```bash
pchronicle serve \
  --gateway auto \
  --gateway-dataset ../data/captures \
  --gateway-split '{user}/{date}/{hour}'
```

The endpoint accepts canonical trajectory batches at `POST /v1/events`.
`x-persisting-user-id` supplies `{user}`; missing users use `_unknown`.
`{date}` and `{hour}` are UTC and a run/session stays pinned to its first
partition. Only safe relative templates are accepted. `auto` selects
`127.0.0.1` and an ephemeral port. Readiness JSON version 2 includes the
resolved Gateway Dataset and split template.

### Forwarding Gateway compatibility

Pass an existing Gateway TOML file to make `serve` forward LLM requests and
capture canonical request/response events into one statically mounted Dataset:

```bash
pchronicle serve \
  --listen 127.0.0.1:8080 \
  --gateway-config gateway.toml \
  --gateway-dataset ../data/atif
```

`gateway.toml` remains the single source for proxy/admin listeners, model
routes, credentials, and network policy. Both listeners must use loopback
addresses. `--gateway-dataset` is the output Dataset URI and is auto-mounted.
For an S3/Azure/GCS Dataset, add
`--gateway-state ./gateway-state` for Gateway's local session index, WAL, and
optional projection; local Datasets use their own root by default.
`--gateway-stream-markdown` also maintains AgenticMD. Canonical Lance events
are always written directly to the selected Dataset, never through the
read-only Warehouse API. Add `--gateway-debug` to mirror
Gateway dispatch and capture diagnostics directly to stderr; these diagnostics
can include bounded request and response bodies.

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
cargo nextest run -p persisting-pchronicle-cli --tests --locked
```

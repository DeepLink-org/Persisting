# `pchronicle` command reference

`pchronicle` is the primary Dataset-oriented CLI for trajectory history. It
discovers Sources under a local path or S3 prefix, normalizes supported formats
into common SQL tables, and keeps result data on stdout while writing Snapshot
metadata and diagnostics to stderr.

This page describes the current command implementation. For the surrounding
product and storage boundary, see the [pChronicle product architecture](../design/architecture.md).

## Command status

| Command | Current behavior |
|---|---|
| `onboard` | Render a guided walkthrough over a temporary example or an explicit Dataset |
| `default` | Get or set one local directory as the default Warehouse |
| `ls` / `list` | Discover logical trajectory Sources |
| `status` | Report Dataset health, aggregate counts, and automatic projection state |
| `query` | Execute one bounded, read-only SQL statement |
| `analysis` | Run a built-in `overview`, `agents`, `models`, or `tools` report |
| `find` | Locate Run, Session, or Step candidates by Source-local ID |
| `import` | Create a new Dataset from exchange JSON or a canonical Event Store |
| `export` | Export complete trajectories as ATIF, ACTF, OpenAI Messages, or Storyline JSON |
| `echo` | Run a deterministic loopback-only LLM upstream for Gateway tests |
| `serve` | Compose loopback Warehouse, Control, Gateway, and automatic projection services |

The executable's `--help` is authoritative for individual flags and defaults.

## Guided onboarding

```bash
pchronicle onboard
pchronicle onboard ./dataset
pchronicle onboard query ./dataset
pchronicle onboard exchange
```

With no Dataset argument, the command creates deterministic temporary ATIF,
ACTF, and OpenAI Messages Datasets and removes them on exit. An explicit Dataset
is opened read-only. The default walkthrough executes every section; `all`,
`concepts`, `inspect`, `analyze`, `query`, `formats`, `find`, `exchange`, and
`serve` subcommands navigate directly to one section. Dataset-oriented sections
accept an optional explicit Dataset URI.

The expanded guide executes catalog, status, built-in analysis, schema discovery,
Step and tool-call SQL, cross-format SQL, Source-local lookup, isolated default
Warehouse setup, create-only import, strict export, and server guidance. Executed
operations use the same internal implementation as their product commands. The
Warehouse and exchange section always uses an isolated settings file and temporary
paths.

The guide is authored as Markdown. Interactive terminals receive a styled
rendering of the small supported subset; redirected stdout receives the original
Markdown with no ANSI escapes. ANSI styling is also disabled by `NO_COLOR` or
`TERM=dumb`. This presentation behavior is confined to `onboard` and does not
change any existing command's stdout/stderr contract.

## Local default Warehouse

A default Warehouse is a local Dataset root, not a daemon or hidden database:

```bash
pchronicle default ./trajectory-data
pchronicle default
```

The first command creates the directory when absent and stores its normalized
absolute path in the user settings. Once configured, local read commands may
omit the Dataset URI:

```bash
pchronicle ls
pchronicle status
pchronicle query "SELECT COUNT(*) AS runs FROM dataset.runs"
pchronicle analysis overview
pchronicle find --session-id session-42
pchronicle export --output sessions.json --format storyline
```

An explicit Dataset URI always takes precedence. Use global `--settings FILE`
or `PCHRONICLE_SETTINGS` to isolate settings in automation.

## Catalog and status

```bash
pchronicle ls ./dataset
pchronicle ls s3://bucket/prefix --format json
pchronicle ls ./dataset --physical --errors strict
pchronicle status ./dataset --format json
```

`ls` lists logical Sources rather than every Lance fragment. `--physical` adds
physical metadata. `status` aggregates the immutable Catalog Snapshot selected
for that invocation. Both commands bound discovery with `--max-files` and
`--max-entries`; `--errors report` keeps diagnosable Sources in the result,
while `strict` fails the command.

For every canonical `events.lance`, `status --format json` also reports the
deterministic sibling projection path, `fresh`, `stale`, `missing`, or `error`
state, source `fact_version`/`fact_rows`, and projection generation when one is
published. Inspection is read-only and never creates or repairs a projection.

## Read-only SQL

With one positional Dataset, it is mounted as SQL schema `dataset`:

```bash
pchronicle query ./dataset \
  "SELECT session_id, COUNT(*) AS steps
   FROM dataset.steps
   GROUP BY session_id
   ORDER BY session_id"
```

Named mounts support cross-Dataset SQL:

```bash
pchronicle query \
  --dataset live=./live \
  --dataset archive=s3://bucket/archive \
  "SELECT * FROM live.runs UNION ALL SELECT * FROM archive.runs"
```

The normalized schemas expose the relations available for each Source,
including `sources`, `runs`, `steps`, `tool_calls`, `events`, and
`trajectories`. Use `DESCRIBE dataset.steps` rather than relying on an exchange
format's physical fields.

`--format table|jsonl|csv`, `--output`, row and byte limits, discovery limits,
and a timeout bound result production. The output path is create-only. Only one
read-only SQL statement is accepted; DDL, DML, and other mutating statements
are rejected.

## Built-in analysis

Use built-in analysis for common, stable summaries:

```bash
pchronicle analysis overview [./dataset]
pchronicle analysis agents [./dataset]
pchronicle analysis models [./dataset]
pchronicle analysis tools [./dataset]
```

- `overview` reports Source readiness and total trajectories, Steps, Agents,
  Models, and tool calls;
- `agents` groups activity by Agent identity and version;
- `models` combines declared trajectory models with observed Step models;
- `tools` groups calls by normalized function name and reports duration
  coverage.

All four accept `--format table|jsonl|csv`, `--limit`, byte and discovery
limits, and a timeout. Use `query` for arbitrary SQL. There is no `users`
report because the normalized schema does not define a stable user identity.

## Find by Source-local ID

```bash
pchronicle find ./dataset --run-id run-42
pchronicle find ./dataset --session-id session-42
pchronicle find ./dataset --session-id session-42 --step-id 7
pchronicle find ./dataset --source nested/source.json --session-id session-42
```

External IDs are preserved and are only Source-local. Without `--source`, the
result can contain multiple candidates; use the returned `source_path` to make
a subsequent lookup unambiguous.

## Import and export

Import creates a new local Dataset and refuses an existing target:

```bash
pchronicle import --from input.json --output ./imported --format atif
pchronicle import --from input.json
pchronicle import --from ./corpus --output ./normalized \
  --output-format storyline
cat input.json | pchronicle import --from - --stream \
  --output ./imported --format openai-messages
```

Regular files can be auto-detected. A directory input recursively scans
`.json`, `.jsonl`, and `.ndjson` regular files, skips symbolic links encountered
during traversal, and keeps each Source's relative path in the default
`--output-format preserve` output. An explicitly named symbolic link is
accepted only when its target is a regular file. ATIF `.jsonl`/`.ndjson`
Sources decode every non-empty record.

`--output-format storyline` instead squashes every decoded input into one
normalized Storyline Lance Store at the output root. Directory, regular-file,
and stdin imports all produce root-level `CURRENT`, `generations`, and
`objects.lance` entries. Catalog discovery exposes that Store as one physical
Source named `.`, and `_file_` is `.` in all three normalized SQL tables. The
original input paths remain available in import diagnostics but are not stored
as query provenance.

A squash preserves `run_id`, `document_id`, and `session_id` values without
prefixing them. `document_id` and `session_id` must each be globally unique;
collisions fail the complete import and report both input paths. Select
`preserve` when Source-local duplicate identities or original Source boundaries
must remain queryable. The output root is published only after every Source and
the single Store snapshot succeed.

Successful import JSON always includes `dataset_uri`, `output_format`,
`sources`, `trajectories`, and `input_bytes`. `output_format` is exactly
`preserve` or `storyline-lance`. Single-file and stdin imports also include
`source_path` and `format`; directory imports omit those two source-specific
fields. For a squash, `sources` still counts logical inputs even though the
result has one physical Source. A single-file preserve import of ATIF JSON
Lines uses the canonical `trajectories.atif.jsonl` or
`trajectories.atif.ndjson` source name so later queries retain the
line-delimited container semantics.

That response shape has one contextual exception. A validated, non-empty
canonical Event Store is detected before JSON scanning and always creates a
Storyline Lance projection:

```bash
pchronicle import --from ./run/events.lance --output ./run/storyline
```

Canonical import accepts local and object-store URIs, does not mutate the
source, and is create-only. It reports `source_path: "events.lance"`, `format:
"events"`, `output_format: "storyline-lance"`, and `fact_rows`, while omitting
`input_bytes`. It rejects a JSON exchange `--format` and explicit
`--output-format preserve`.

If `--output` is omitted, pChronicle derives a child name under the configured
default Warehouse. Stdin requires `--stream`, an explicit input format, and a
finite input ending at EOF. `--max-input-bytes` is optional and applies to each
Source when set; omitting it leaves per-Source input size unbounded.

Export selects complete trajectories from one Catalog Snapshot:

```bash
pchronicle export --from ./imported --output restored.json --format atif
pchronicle export --from ./imported --output - --stream --format storyline
pchronicle export --from ./imported --output one.json --format actf \
  --source source.json --session-id session-42 --strict
```

Import supports `atif`, `actf`, `openai-messages`, and `storyline`. Export
supports the same four exchange formats. Output files are create-only unless
`--overwrite` is explicit. `--strict` refuses a conversion that cannot preserve
the original exchange document.

## Deterministic Echo upstream

`echo` runs a loopback-only LLM upstream for Gateway integration tests. It
supports Chat Completions, Messages, Responses, Gemini, streaming responses,
and plain or Base64 output:

```bash
pchronicle echo
pchronicle echo --listen 127.0.0.1:19080 --encoding base64
```

One request can override the server default with the
`x-persisting-echo-encoding: plain|base64` header.

## Read-only Warehouse server

`serve` uses an explicit static configuration; it does not use the local
default Warehouse setting:

```toml
[[datasets]]
name = "evals"
uri = "../data/atif"
```

```bash
pchronicle serve --config warehouse.toml --listen 127.0.0.1:8081 --open
pchronicle serve --storage ./trajectory-data --control 127.0.0.1:0
```

Relative local Dataset paths are resolved from the configuration file's
directory. At least one of `--listen`, `--control`, or `--gateway` is required.
`--config` and `--storage` are mutually exclusive: configuration mounts named
Datasets, while `--storage URI` mounts one Dataset named `default`. `--listen`
enables Warehouse HTTP; omitting it does not start Warehouse. `--control`
requires `--storage` and enables the authenticated write/control protocol on a
loopback listener. `--open` requires `--listen`.

Warehouse rejects non-loopback listeners because it has no authentication. Its
Dataset mounts and API are read-only; import, export, maintenance, and arbitrary
filesystem access are not exposed over HTTP.

With `--storage`, `serve` discovers validated non-empty canonical Event Stores
and converges their deterministic sibling `storyline` projections before it
publishes readiness. It continues discovery at runtime, using bounded
concurrency and retry for incremental sync or rebuild. Projection failures do
not block canonical durable writes, and a destination without matching lineage
is never overwritten. If `--listen` is enabled, each successful publication
triggers a complete Catalog rebuild outside the reader lock and an atomic
snapshot swap; a failed refresh retains the old queryable snapshot for retry.

`serve` can also compose the existing Gateway on separate loopback listeners:

```bash
pchronicle serve --config warehouse.toml \
  --listen 127.0.0.1:8080 \
  --gateway gateway.toml \
  --gateway-dataset evals \
  --gateway-stream-markdown
```

`--gateway` points to the complete Gateway TOML, so model routes, upstream
credentials, network policy, and proxy/admin listeners are not duplicated in
the pChronicle CLI. Canonical events go through an in-process sink directly to
the selected static Dataset while the Warehouse Web/API remains read-only. A
multi-Dataset Warehouse without `default_dataset` requires
`--gateway-dataset`; an object-store Dataset also requires a local
`--gateway-state` directory. `--debug` (alias `--gateway-debug`) mirrors
Gateway dispatch/capture diagnostics directly to stderr and may include
bounded request and response bodies.

The `pchronicle` executable is the public CLI for Dataset catalog, SQL,
analysis, find, exchange, deterministic Gateway testing, and read-only
Warehouse serving.

## Related workflows

- [Discover and query a Dataset](../guides/discover-and-query.md).
- [Import and export trajectories](../guides/exchange.md).
- [Serve a local read-only Warehouse](../guides/serve.md).
- [Configure Gateway forwarding, rewriting, and capture](../guides/serve-gateway.md).
- [Dataset, Source, and Snapshot](../concepts/dataset-and-source.md) explains the
  identity model behind the command arguments.

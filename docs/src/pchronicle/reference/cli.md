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
| `status` | Report Dataset health and aggregate counts |
| `query` | Execute one bounded, read-only SQL statement |
| `analysis` | Run a built-in `overview`, `agents`, `models`, or `tools` report |
| `find` | Locate Run, Session, or Step candidates by Source-local ID |
| `import` | Create a new local Dataset from ATIF, ACTF, OpenAI Messages, or Storyline JSON |
| `export` | Export complete trajectories to one of those exchange formats |
| `serve` | Serve statically configured Datasets through a loopback-only read API and Web UI |

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
cat input.json | pchronicle import --from - --stream \
  --output ./imported --format storyline
```

Regular files can be auto-detected. If `--output` is omitted, pChronicle derives
a child name under the configured default Warehouse. Stdin requires
`--stream`, an explicit format, and a finite input ending at EOF.

Export selects complete trajectories from one Catalog Snapshot:

```bash
pchronicle export --from ./imported --output restored.json --format atif
pchronicle export --from ./imported --output - --stream --format storyline
pchronicle export --from ./imported --output one.json --format actf \
  --source source.json --session-id session-42 --strict
```

Supported exchange formats are `atif`, `actf`, `openai-messages`, and
`storyline`. Output files are create-only unless `--overwrite` is explicit.
`--strict` refuses a conversion that cannot preserve the original exchange
document.

## Read-only Warehouse server

`serve` uses an explicit static configuration; it does not use the local
default Warehouse setting:

```toml
[[datasets]]
name = "evals"
uri = "../data/atif"
```

```bash
pchronicle serve --config warehouse.toml
pchronicle serve --config warehouse.toml --listen 127.0.0.1:8081 --open
```

Relative local Dataset paths are resolved from the configuration file's
directory. The server rejects non-loopback listeners because it has no
authentication. Its Dataset mounts and API are read-only; import, export,
maintenance, and arbitrary filesystem access are not exposed over HTTP.

`serve` can also compose the existing Gateway on separate loopback listeners:

```bash
pchronicle serve --config warehouse.toml \
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

The standalone command is the only public CLI for Dataset catalog, SQL,
analysis, find, exchange, and read-only Warehouse serving.

## Related workflows

- [Discover and query a Dataset](../guides/discover-and-query.md).
- [Import and export trajectories](../guides/exchange.md).
- [Serve a local read-only Warehouse](../guides/serve.md).
- [Dataset, Source, and Snapshot](../concepts/dataset-and-source.md) explains the
  identity model behind the command arguments.

# pChronicle CLI

This page is the English reference for the canonical `pchronicle` command
line. New commands and scripts should use the syntax documented here.

## Find the command you need

Start with the shortest path to a useful answer:

- **Try the product:** `pchronicle onboard query` uses temporary example data
 and needs no Dataset path.
- **Check a Dataset:** use `list`/`ls` and `stats overview` before writing SQL.
- **Locate a run or phrase:** use `find --run-id`, `--session-id`, or
 `--match`; inspect the returned identity before querying more data.
- **Ask a repeatable question:** use `query --sql` or `query --file` and set
 output and resource limits for automation.
- **Expose history:** use `serve` only after the read-only query works; the
 [serve guide](../guides/serve.md) explains the lifecycle and shutdown path.

For a first interaction, copy this sequence:

```bash
pchronicle onboard query
pchronicle list ./trajectory-data
pchronicle query ./trajectory-data --sql 'SELECT COUNT(*) FROM dataset.runs'
```

The second and third commands require a Dataset you already have. If you do
not have one, stop after `onboard query` and continue with the
[Dataset walkthrough](../get-started.md).

## Dataset

A Dataset is a **path**. pChronicle opens that path as the trajectory store.
It may be:

- a local directory or file, such as `./local/path`;
- an object-store URI prefix, such as `s3://bucket/prefix`;
- a dataset pin that resolves to either location, such as `@prod`.

## Global syntax

```text
pchronicle [-c FILE] [--log-level error|warn|info|debug] <COMMAND> ...
```

`-c, --config` selects the user configuration file. `--log-level` controls
stderr diagnostics without changing stdout results or exit status. For
`pchronicle serve`, the same flag also filters Warehouse request tracing
(`pchronicle.serve`).

## Commands

### Onboard

```text
pchronicle onboard [SECTION] [DATASET] [--no-pause]
```

```bash
pchronicle onboard
pchronicle onboard query @prod
```

The complete walkthrough covers Dataset discovery, health and built-in
analysis, normalized SQL, unified FTS/JSONB `find` expressions, cross-format
queries, Storyline Lance import/export, and the read-only Web/API boundary.
Use `pchronicle onboard find DATASET` to inspect the search grammar directly.

### Dataset pins

```text
pchronicle dataset pin|unpin|list|show|set|rename …
```

```bash
pchronicle dataset pin default ./trajectory-data
pchronicle dataset show default
pchronicle dataset pin prod s3://bucket/evals
pchronicle dataset pin secure s3://bucket/evals --ak "$AWS_ACCESS_KEY_ID" --sk "$AWS_SECRET_ACCESS_KEY"
pchronicle dataset pin minio s3://bucket/evals --endpoint http://127.0.0.1:9000 --region us-west-2 --ak 123 --sk 123
pchronicle dataset pin regional s3://bucket/evals --region us-west-2
pchronicle dataset pin team catalog://127.0.0.1:8081 --ak USER_AK --sk USER_SK
pchronicle dataset set prod s3://new-bucket/evals
pchronicle dataset list
pchronicle stats @prod
```

`default` is a reserved pin used when a command omits `DATASET_URI`. It must be
a local directory. Other pins only update user configuration; they do not move
or delete Dataset data. Pins are stored under `[pins.<name>]` in the user
config file (`-c` / `PCHRONICLE_CONFIG`). Example:

```toml
[pins.default]
uri = "/abs/path/to/warehouse"

[pins.prod]
uri = "s3://bucket/evals"
endpoint = "http://127.0.0.1:9000"
region = "us-west-2"
access_key = "..."
secret_key = "..."
```

Legacy keys such as `default_warehouse` or `aliases` are rejected. S3
credentials supplied with `--ak` and `--sk` are stored on the same pin table
and applied through the standard AWS environment variables when the pin is
used. They are not printed by `dataset list` or `dataset show`. `dataset list`
also includes the built-in `@codex`, `@claude`, and `@claude-code` pins for
the corresponding local Agent session roots.
For S3-compatible services such as MinIO, pass the endpoint with `--endpoint`.
Keep the Dataset URI in the form `s3://bucket/prefix`.
A `catalog://127.0.0.1:PORT` pin is a Directory locator. `@team/prod` fetches a
ticket and opens the ticket `uri`; `@team` by itself lists authorized
Datasets. Directory pins require `--ak` and `--sk` and reject `--endpoint` and
`--region`. See
[RFC-0013](../../rfcs/0013-pchronicle-warehouse-catalog.md).
For `http://` endpoints, pChronicle also enables `AWS_ALLOW_HTTP` automatically.
The optional `--region` is stored per pin. When an `s3://` pin omits it,
pChronicle applies `us-west-2` as `AWS_REGION` / `AWS_DEFAULT_REGION` before
opening the store. Endpoint, region, and credentials are applied before the
Tokio runtime starts so OpenDAL sees them reliably.

### Inspect and find

```text
pchronicle list|ls [DATASET] [OPTIONS]
pchronicle stats [DATASET] [OPTIONS]
pchronicle stats <overview|agents|models|tools> [DATASET] [OPTIONS]
pchronicle find [DATASET]
 (--run-id ID|--document-id ID|--session-id ID|--match EXPRESSION) [OPTIONS]
```

```bash
pchronicle list @prod --format json
pchronicle stats @prod --format json
pchronicle stats overview @prod
pchronicle find @prod --session-id session-42
pchronicle find ./dataset --match "timeout" --match "retry" --format json
pchronicle find ./dataset --match '$.tags=important' --match '$.priority=2' --format json
```

`list` (`ls`) discovers run sources. Bare `stats` reports Dataset health and
counts. `stats overview|agents|models|tools` runs the built-in statistical
reports.
`--match` is the unified search expression. Plain terms search Storyline Step
content with the indexed FTS/Jieba path; scoped forms such as `#system(prompt)`
select a field, and `AND`/`OR`/`NOT` combine predicates. JSONB predicates use
`$.path=value` (or `#json("$.path")=value`) and perform exact JSONPath value
matching. Repeat `--match` to require all expressions. A JSON-only expression
currently searches run-level JSONB columns; a mixed text/JSON expression
searches step-level JSONB columns. An explicit `#json.metrics(...)` selector
also targets step-level JSONB without a text term. CLI and Web share this
expression, the reported `search.scope`, and `snapshot_id`. The Web UI may
highlight and clip returned fields without changing the match set.
Each match includes a bounded `preview` field to make candidate selection
possible before a follow-up query. JSON output also reports `search.mode`
(`fts`, `json`, `fts+json`, or `identity`), `search.scope` (`steps` or `runs`), and FTS
availability/tokenizer metadata.
The current grammar is documented in the [query model](query-model.md).
[RFC-0012](../../rfcs/0012-pchronicle-find-query-syntax.md) records the accepted
decision; where they disagree, the installed CLI wins.

### Query

```text
pchronicle query [DATASET|--mount NAME=DATASET ...] (--sql SQL|--file FILE_OR_STDIN) [OPTIONS]
```

```bash
pchronicle query ./dataset --sql 'SELECT COUNT(*) FROM dataset.runs'
pchronicle query \
 --mount live=./live --mount archive=@archive \
 --file report.sql
```

Each invocation accepts one read-only statement with explicit resource limits. `--file -` reads SQL
from stdin. Use `--format`, `--output`, `--max-output-rows`,
`--max-output-bytes`, and `--timeout` to make pipeline behavior explicit.

### Import

```text
pchronicle import -f|--from SOURCE -t|--to NEW_DATASET
 [-i|--input-format auto|atif|actf|openai-messages|storyline|codex|claude-code|compact-jsonl]
 [-o|--output-format preserve|storyline|compact-jsonl]
 [|--replace] [--append] [--on-duplicate suffix|skip] [--yes]
 [--column NAME=JSON_PATH]... [OPTIONS]
```

```bash
pchronicle import -f input.json -t ./imported -i atif
cat input.json | pchronicle import -f - -t ./imported -i openai-messages
pchronicle import -f more.json -t ./normalized --append --on-duplicate skip
pchronicle import -f rebuilt.json -t ./normalized --replace --yes
pchronicle import -f ./jsonl-root -t ./records.lance \
 -o compact-jsonl \
 --column id=$.event.id --column timestamp=$.event.time \
 --column model=$.payload.model
```

`-` means stdin. Create is the default and requires a new destination.
`--append` requires an existing Storyline Dataset and either suffixes colliding
`document_id` values with `#N` (the default) or skips them. `--replace` moves the
old local Dataset aside, publishes the fully imported Dataset with a rename
transaction, and only then removes the old data. It requires interactive
confirmation or `--yes`; an existing object-store Dataset cannot currently be
replaced in place.

Compact JSONL is a record store, not a trajectory conversion. Either
`--input-format compact-jsonl` or `--output-format compact-jsonl` selects it.
It recursively reads local `.json`, `.jsonl`, and `.ndjson` files. JSON objects
and arrays are accepted; each JSON document becomes one record, with arrays kept intact.
Missing or invalid `id` and `timestamp` values receive stable
`source_filename#line_number` values. The default paths are `$.id` and
`$.timestamp`. `--column id=PATH` and
`--column timestamp=PATH` override those paths, while any other
`--column NAME=PATH` adds a nullable JSONB projection. Compact import supports
local `create` and confirmed `replace`, but not stdin, object-store targets, or
`append`.

### Sync

```text
pchronicle sync --from DIRECTORY --to DIRECTORY --convert DIRECTORY
 [--input-format FORMAT] [--column NAME=JSON_PATH]...
 [--interval DURATION] [--once]
```

`sync` is a resident polling worker for `.json`, `.jsonl`, and `.ndjson` files.
For run-data formats it coalesces changes into a pending set and, on each
interval, atomically mirrors the source files byte-for-byte into a local
Warehouse Dataset and writes a Storyline Lance Dataset to `--convert`.
Pending changes are cleared only after both outputs succeed; failures retain
the set and retry with bounded exponential backoff. Use `--once` for one
initial batch and exit. The two destinations must be local directories outside
the source directory.

With `--input-format compact-jsonl`, the source must be a local `.json`, `.jsonl`,
or `.ndjson` tree
and the same `--column` rules as compact import apply. Every successful batch
rescans the whole tree and atomically replaces the compact Lance snapshot at
`--convert`, so additions, changes, and deletions are reflected without
row-level incremental updates. In this mode `--to` is retained as a required
compatibility argument but is not written.

### Drop

```text
pchronicle drop DATASET [--yes]
```

Drop permanently removes a local Dataset directory or object-store prefix. It
requires interactive confirmation unless `--yes` is supplied and refuses
filesystem roots or whole object-store buckets.

### Export

```text
pchronicle export -f|--from DATASET -t|--to TARGET
 -o|--output-format atif|actf|openai-messages|storyline|compact-jsonl [OPTIONS]
```

```bash
pchronicle export -f ./imported -t restored.json -o atif
pchronicle export -f @prod -t - -o actf --session-id session-42
```

`--to -` writes stdout. File and object-store destinations are create-only
unless `--overwrite` is explicit.

### Agent analysis

```text
pchronicle agent <codex|claude> [DATASET]
 [--ask QUESTION|--ask-file FILE_OR_STDIN] [--no-overview] [--dry-run]
```

```bash
pchronicle agent codex ./dataset
pchronicle agent claude @prod --ask 'Compare model latency'
```

### Serve

```text
pchronicle serve
 [--listen LOOPBACK_ADDR] [--control LOOPBACK_ADDR] [--open]
 [--gateway ADDRESS --gateway-dataset DATASET [--gateway-split TEMPLATE]
 [--gateway-split-idle DURATION]]
 [--gateway-config FILE --gateway-dataset DATASET [--gateway-state DIRECTORY]]
 [--gateway-stream-markdown] [--gateway-debug]
 [--catalog-config FILE]
 [<[NAME=]DATASET> ...]
pchronicle serve catalog dataset add --catalog-config FILE NAME --uri URI [OPTIONS]
pchronicle serve catalog dataset remove --catalog-config FILE NAME...
pchronicle serve catalog dataset list --catalog-config FILE
pchronicle serve catalog issue --catalog-config FILE NAME
pchronicle serve catalog grant --catalog-config FILE NAME DATASET...
pchronicle serve catalog revoke --catalog-config FILE NAME DATASET...
```

```bash
pchronicle serve ./trajectory-data
pchronicle serve \
 --gateway auto \
 --gateway-dataset ./trajectory-data \
 --gateway-split '{user}/{date}/{hour}'
```

Every listener must use a loopback address. A bare single Dataset is mounted as
`default`; with several Datasets, use `NAME=DATASET` when a stable mount name is
needed. Control requires a mount named `default`.
`--catalog-config FILE` mounts every `[datasets.*]` library in the Directory
file into Warehouse and enables `catalog://` locators. It conflicts with
positional Dataset mounts. Pair Directory clients with
`dataset pin NAME catalog://127.0.0.1:PORT --ak --sk`.
`pchronicle serve catalog dataset add|remove|list` and
`issue|grant|revoke` rewrite that file and do not start HTTP; `issue` prints
the user secret once. Restart serve after changing libraries, users, or grants.
`catalog` is a reserved `serve` subcommand; mount a path of that name as
`./catalog`.
See [RFC-0013](../../rfcs/0013-pchronicle-warehouse-catalog.md) and
[RFC-0015](../../rfcs/0015-chronicle-manifest.md) for nested discovery sidecars.
The config-free Gateway accepts canonical trajectory events at
`POST /v1/events`. `--gateway-dataset` is an output URI and is auto-mounted;
it is no longer a mounted Dataset name. Split templates accept the exact
placeholders `{user}`, `{date}`, and `{hour}`. Existing canonical sources wait
30 minutes by default after their last event before automatic Storyline
projection; override this with `--gateway-split-idle DURATION`.
In Gateway mode, the Warehouse's single-trace event, Storyline, and trajectory
endpoints read the latest canonical manifest for an already discovered source,
so active traces do not wait for projection or a global Snapshot refresh.

## Output and exit status

stdout contains command results, exported content, or readiness JSON. stderr
contains diagnostics. Stable boundary exit codes are: `0` success, `2` invalid
input, `3` not found, `4` conflict, `5` resource limit, and `6` timeout or a
temporarily unavailable dependency. Unclassified internal errors use `1`.

Use [Discover and query](../guides/discover-and-query.md) for the locate-then-SQL
workflow, [Import and export](../guides/exchange.md) for interchange, and
[Serve Datasets locally](../guides/serve.md) for the read-only server. Snapshot
construction is explained in [Snapshot design](../design/catalog.md).

#### Catalog management

The Directory ACL file contains users, datasets (libraries), and grants.
Management commands create the file when it does not exist.

```text
pchronicle serve catalog issue --catalog-config FILE NAME
pchronicle serve catalog grant --catalog-config FILE NAME DATASET...
pchronicle serve catalog revoke --catalog-config FILE NAME DATASET...
pchronicle serve catalog dataset add --catalog-config FILE NAME --uri URI
 [--endpoint URL] [--region REGION] [--access-key KEY] [--secret-key KEY]
pchronicle serve catalog dataset remove --catalog-config FILE NAME...
pchronicle serve catalog dataset list --catalog-config FILE
```

`issue` generates a user AK/SK and prints the secret once. `dataset add`
registers the URI and optional backend storage credentials without creating or
deleting object-store data. `grant` / `revoke` add or remove library names on
that user (v1 grants are library membership, not fine-grained permission flags).

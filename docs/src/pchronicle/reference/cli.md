# pChronicle CLI

This page is the English reference for the canonical `pchronicle` command
line. New commands and scripts should use the syntax documented here.

## Dataset

A Dataset is a **path**. pChronicle opens that path as the trajectory store.
It may be:

- a local directory or file, such as `./local/path`;
- an object-store URI prefix, such as `s3://bucket/prefix`;
- a user alias that resolves to either location, such as `@prod`.

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

### Default Dataset

```text
pchronicle default <show|set LOCAL_DATASET|clear>
```

```bash
pchronicle default set ./trajectory-data
pchronicle default show
```

### Aliases

```text
pchronicle alias [list|add|remove|rename|get-url|set-url] [ARGUMENTS]
```

```bash
pchronicle alias add prod s3://bucket/evals
pchronicle alias add secure s3://bucket/evals --ak "$AWS_ACCESS_KEY_ID" --sk "$AWS_SECRET_ACCESS_KEY"
pchronicle alias add minio s3://bucket/evals --endpoint http://127.0.0.1:9000 --ak 123 --sk 123
pchronicle alias add regional s3://bucket/evals --region us-west-2
pchronicle alias add team catalog://127.0.0.1:8081 --ak USER_AK --sk USER_SK
pchronicle alias set-url prod s3://new-bucket/evals
pchronicle status @prod
```

Alias operations only update user configuration; they do not move or delete a
Dataset. S3 credentials supplied with `--ak` and `--sk` are stored separately
from the URI and applied through the standard AWS environment variables when
the alias is used. They are not printed by `alias list` or `alias get-url`.
`alias list` also includes the built-in `@codex`, `@claude`, and `@claude-code`
aliases for the corresponding local Agent session roots.
For S3-compatible services such as MinIO, pass the endpoint with `--endpoint`.
It is stored separately and applied as `AWS_ENDPOINT_URL_S3` when the alias is
used. Keep the Dataset URI in the form `s3://bucket/prefix`; do not put the
service host and port in that URI.
A `catalog://127.0.0.1:PORT` alias is a Directory locator. `@team/prod` fetches a
ticket and opens the ticket `uri` (a path); `@team` by itself is not a Dataset.
Directory aliases require `--ak` and `--sk` and reject `--endpoint` and
`--region`. Backend object-store keys stay on the Directory server. The
locator, ticket, and process model are specified in
[RFC-0013](../../rfcs/0013-pchronicle-warehouse-catalog.md).
For `http://` endpoints, pChronicle also enables `AWS_ALLOW_HTTP` automatically
for local S3-compatible services such as MinIO.
`alias set-url` accepts the same `--endpoint` option and preserves the existing
endpoint when changing between two S3 URIs without specifying a new one.
The optional `--region` is also stored per alias; when omitted, the S3 client
uses its default region (`us-west-2` when a fallback is required).

### Inspect and find

```text
pchronicle ls [DATASET] [OPTIONS]
pchronicle status [DATASET] [OPTIONS]
pchronicle find [DATASET]
  (--run-id ID|--document-id ID|--session-id ID|--match EXPRESSION) [OPTIONS]
```

```bash
pchronicle ls @prod --format json
pchronicle find @prod --session-id session-42
pchronicle find ./dataset --match "timeout" --match "retry" --format json
pchronicle find ./dataset --match '$.tags=important' --match '$.priority=2' --format json
```

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

### Built-in analysis

```text
pchronicle analysis <overview|agents|models|tools> [DATASET] [OPTIONS]
```

```bash
pchronicle analysis overview
pchronicle analysis tools @prod --format csv --limit 20
```

### Import

```text
pchronicle import -f|--from SOURCE -t|--to NEW_DATASET
  [-i|--input-format auto|atif|actf|openai-messages|storyline|codex|claude-code]
  [-o|--output-format preserve|storyline] [OPTIONS]
```

```bash
pchronicle import -f input.json -t ./imported -i atif
cat input.json | pchronicle import -f - -t ./imported -i openai-messages
```

`-` means stdin. Import is create-only and publishes the destination only after
the selected operation succeeds.

### Export

```text
pchronicle export -f|--from DATASET -t|--to TARGET
  -o|--output-format atif|actf|openai-messages|storyline [OPTIONS]
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
pchronicle serve catalog issue  --catalog-config FILE NAME
pchronicle serve catalog grant  --catalog-config FILE NAME DATASET...
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
`--catalog-config FILE` serves a path Directory instead of opening libraries
in the parent process. Pair it with `alias add NAME catalog://127.0.0.1:PORT --ak --sk`.
`pchronicle serve catalog issue|grant|revoke` rewrites that file and does not
start HTTP; `issue` prints the user secret once. Restart serve after changing
users or grants. `catalog` is a reserved `serve` subcommand; mount a path of
that name as `./catalog`.
See [RFC-0013](../../rfcs/0013-pchronicle-warehouse-catalog.md).
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

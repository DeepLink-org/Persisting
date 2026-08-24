# pChronicle CLI

This page is the English reference for the canonical `pchronicle` command
line. New commands and scripts should use the syntax documented here.

## Dataset

A Dataset is the single object operated on by pChronicle. It may be:

- a local directory or file, such as `./local/path`;
- an object-store URI prefix, such as `s3://bucket/prefix`;
- a user alias that points to either location, such as `@prod`.

## Global syntax

```text
pchronicle [-c FILE] [--log-level error|warn|info|debug] <COMMAND> ...
```

`-c, --config` selects the user configuration file. `--log-level` controls
stderr diagnostics without changing stdout results or exit status.

## Commands

### Onboard

```text
pchronicle onboard [SECTION] [DATASET] [--no-pause]
```

```bash
pchronicle onboard
pchronicle onboard query @prod
```

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
pchronicle alias set-url prod s3://new-bucket/evals
pchronicle status @prod
```

Alias operations only update user configuration; they do not move or delete a
Dataset.

### Inspect and find

```text
pchronicle ls [DATASET] [OPTIONS]
pchronicle status [DATASET] [OPTIONS]
pchronicle find [DATASET] (--run-id ID|--document-id ID|--session-id ID) [OPTIONS]
```

```bash
pchronicle ls @prod --format json
pchronicle find @prod --session-id session-42
```

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
  [-i|--input-format FORMAT] [-o|--output-format preserve|storyline] [OPTIONS]
```

```bash
pchronicle import -f input.json -t ./imported -i atif
cat input.json | pchronicle import -f - -t ./imported -i openai-messages
```

`-` means stdin. Import is create-only and publishes the destination only after
the selected operation succeeds.

### Export

```text
pchronicle export -f|--from DATASET -t|--to TARGET -o|--output-format FORMAT [OPTIONS]
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
  [--gateway-config FILE] [--gateway-dataset NAME] [--gateway-state DIRECTORY]
  [--gateway-stream-markdown] [--gateway-debug]
  <[NAME=]DATASET> ...
```

```bash
pchronicle serve ./trajectory-data
pchronicle serve \
  --listen 127.0.0.1:8080 \
  --gateway-config gateway.toml \
  --gateway-dataset evals \
  evals=./trajectory-data
```

Every listener must use a loopback address. A bare single Dataset is mounted as
`default`; with several Datasets, use `NAME=DATASET` when a stable mount name is
needed. Control requires a mount named `default`.

## Output and exit status

stdout contains command results, exported content, or readiness JSON. stderr
contains diagnostics. Stable boundary exit codes are: `0` success, `2` invalid
input, `3` not found, `4` conflict, `5` resource limit, and `6` timeout or a
temporarily unavailable dependency. Unclassified internal errors use `1`.

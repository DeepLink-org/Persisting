# Serve Datasets locally

`pchronicle serve` mounts one or more Datasets in the bundled read-only Web UI
and API. It is a local inspection surface, not a public or multi-tenant data
service.

## Command shape

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

Every listener must use a loopback address because the Web UI and read API do
not provide a public authentication boundary.

## Open one Dataset

```bash
pchronicle serve --open ./trajectory-data
```

A single bare Dataset is mounted as `default`. If no listener option is given,
the local Web UI uses an available loopback port.

## Mount several Datasets

```bash
pchronicle serve \
  --listen 127.0.0.1:8081 \
  evals=../data/atif archive=s3://example/archive
```

Mount names become SQL schema and API names. Use `NAME=DATASET` when a stable
name matters.
With several bare paths, pChronicle derives names from their last path
components. Those names can change with the paths, so reusable commands should
still set mount names explicitly.

## Serve a path Directory

```bash
pchronicle serve catalog issue --catalog-config catalog.toml alice
pchronicle serve catalog grant --catalog-config catalog.toml alice prod evals
pchronicle serve --catalog-config catalog.toml --listen 127.0.0.1:8081
```

`catalog.toml` lists libraries (each a path) and users. `serve catalog issue`
writes a user with empty grants and prints the secret once on stdout; `grant` /
`revoke` change `datasets` without starting HTTP. Restart serve after editing
the file. The parent process does not open those paths itself. The Web UI sends
user access/secret keys as headers; queries run in a one-shot worker that
receives only that user's paths. From another terminal:

```bash
pchronicle alias add team catalog://127.0.0.1:8081 --ak USER_AK --sk USER_SK
pchronicle query @team/prod --sql 'SELECT 1'
```

`@team` is a Directory locator, not a Dataset. `@team/prod` fetches a ticket and
opens the ticket `uri` (a path). All `s3://` libraries in one Directory file must
share the same endpoint, region, and backend keys. The listener remains
loopback-only. The design is specified in
[RFC-0013](../../rfcs/0013-pchronicle-warehouse-catalog.md).

## Enable Control or Gateway integration

```bash
pchronicle serve \
  --control 127.0.0.1:0 \
  default=./trajectory-data

pchronicle serve \
  --gateway auto \
  --gateway-dataset ./trajectory-data \
  --gateway-split '{user}/{date}/{hour}'
```

`--gateway` starts a config-free canonical event ingest endpoint. It accepts
`POST /v1/events`, uses `x-persisting-user-id` for `{user}`, and automatically
mounts the output Dataset. `{date}` and `{hour}` use UTC; one run/session is
pinned to its first partition so a streaming or long-lived trajectory is not
split across event sources. `auto` means `127.0.0.1:0`. Existing canonical
sources wait 30 minutes by default after their last event before Storyline
projection; override this with `--gateway-split-idle DURATION`.

When a Warehouse listener is enabled, single-trace Gateway reads reopen the
latest canonical event manifest. Appending to an existing source therefore does
not wait for Snapshot refresh or Storyline projection; only new source files and
published projections require a global Snapshot update.

Control requires a mount named `default`. `--control`, `--gateway`, or `--gateway-config`
without `--listen` starts the requested integration without also starting the
Web UI. The process writes one machine-readable readiness record to stdout;
Control credentials are not written to stderr.

Mounted Datasets and HTTP operations are read-only. Import, export, maintenance,
and arbitrary filesystem access are not exposed through the API. Refreshes
replace the readable view only after the replacement is ready; a failed refresh
keeps the previous view available.

## Logs and failed requests

`pchronicle serve` writes Warehouse request logs to stderr at `--log-level`
(default `info`), tracing target `pchronicle.serve`. Startup logs the listen
address, Dataset names, and Snapshot id. Each `/api` request logs method,
path, status, elapsed time, and a truncated query string. Query and compile
handlers also log truncated SQL.

Failed responses include `code`, `message`, and `request_id`. The Web banner
shows the same `request_id`. Internal failures redact details in JSON; the
stderr ERROR line has `root_cause` and `chain` for that id.

`--log-level error` keeps only internal failures. `--log-level` does not read
`RUST_LOG`.

For Gateway behavior, continue with
[Gateway forwarding, rewriting, and capture](serve-gateway.md). For exact
flags, see the [`pchronicle` CLI reference](../reference/cli.md). Internal
refresh and versioning behavior belongs to [Snapshot design](../design/catalog.md).

Continue with the [local Web UI guide](ui.md) for task-oriented coverage of
Datasets, Runs, Analysis, Storage, and Assistant.

# pChronicle single-machine and self-service cases

These cases cover workflows that do not depend on a Catalog Server. Each case can run on a developer machine against a local directory or an object-store URI.

## Setup

`just cases pchronicle` sets `PCHRONICLE_CASE_FIXTURES` to the repository `examples/data` tree. When running by hand, export that variable first:

```bash
export PCHRONICLE_CASE_FIXTURES=/path/to/Persisting/examples/data
mkdir -p /tmp/pchronicle-cases
cd /tmp/pchronicle-cases
```

## S01: Browse a local Dataset

```bash
pchronicle import --from "$PCHRONICLE_CASE_FIXTURES/atif/support-ticket.json" --to ./trajectory-data --mode create
pchronicle list ./trajectory-data
pchronicle stats ./trajectory-data
```

Expected: the commands list runs, steps, and tool calls in the Dataset.

## S02: Run a SQL query

```bash
pchronicle import --from "$PCHRONICLE_CASE_FIXTURES/atif/support-ticket.json" --to ./trajectory-data --mode create
pchronicle query ./trajectory-data \
  --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
```

Expected: the query succeeds and returns a definite run count.

## S03: Run a built-in analysis

```bash
pchronicle import --from "$PCHRONICLE_CASE_FIXTURES/atif/support-ticket.json" --to ./trajectory-data --mode create
pchronicle stats overview ./trajectory-data
```

Expected: output includes run, step, and tool-call counts plus a time range.

## S04: Import and export

```bash
pchronicle import --from "$PCHRONICLE_CASE_FIXTURES/atif/support-ticket.json" --to ./trajectory-data --mode create
pchronicle export --from ./trajectory-data --to ./output.atif.json --output-format atif
test -s ./output.atif.json
```

Expected: the export file is non-empty and can be imported again.

## S05: Local Warehouse

```bash
pchronicle serve ./trajectory-data --listen 127.0.0.1:8081
```

Expected: the Web UI, `/api/query/tables`, `/api/catalog`, and Explorer APIs are available; no user credentials are required when Catalog is disabled.

## S06: Object-store Dataset

```bash
export AWS_ENDPOINT_URL_S3=http://127.0.0.1:9000
export AWS_ACCESS_KEY_ID=rustfsadmin
export AWS_SECRET_ACCESS_KEY=rustfsadmin
export AWS_REGION=us-east-1
pchronicle list s3://bucket/trajectory
```

Expected: pChronicle discovers and queries the Dataset through an S3-compatible endpoint. Endpoint and credentials are not written into the Dataset URI. Automated runs skip this case by default; set `PCHRONICLE_CASE_MODE=s3` with a reachable endpoint to execute it.

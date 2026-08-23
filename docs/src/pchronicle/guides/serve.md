# Serve a local read-only Warehouse

Use `serve` to inspect statically configured Datasets through the bundled Web
interface and read API. It is a local review surface, not a multi-tenant data
service.

## Configure mounts

```toml
[[datasets]]
name = "evals"
uri = "../data/atif"

[[datasets]]
name = "archive"
uri = "s3://example/archive"
```

Relative local paths are resolved from the configuration file's directory.
Mount names become SQL schemas; Dataset identity remains its normalized URI.

## Start the server

```bash
pchronicle serve --config warehouse.toml \
  --listen 127.0.0.1:8081 --open
```

The server accepts loopback listeners only because it has no authentication or
authorization layer. Do not place it behind a public listener as a substitute
for a production control plane.

Mounted Datasets and API operations are read-only. Import, export, maintenance,
and arbitrary filesystem access are not exposed over HTTP. A refresh constructs
a complete new Catalog Snapshot outside the reader lock before atomically
switching readers to it. A failed refresh retains the old queryable snapshot.

To capture new LLM traffic in the same process, continue with
[Gateway forwarding, rewriting, and capture](serve-gateway.md). For exact flags, see the
[`pchronicle` command reference](../reference/cli.md). For Snapshot behavior,
read the [Dataset Catalog design](../design/catalog.md).

`serve` only starts the services named on the command line. Omitting
`--listen`, `--control`, and `--gateway` starts Warehouse HTTP on `127.0.0.1`
with an ephemeral port. `--control` or `--gateway` without `--listen` still
does not start Warehouse. A storage URI can instead host the local
authenticated Control protocol used by pPilot and pVisor:

```bash
pchronicle serve --storage ./trajectory-data --control 127.0.0.1:0
pchronicle serve --storage ./tmp --storage ./data/evals --listen 127.0.0.1:9980
pchronicle serve --storage default=./tmp --storage evals=./data --control 127.0.0.1:0
pchronicle serve --storage @codex
```

`--config` and `--storage` are mutually exclusive, and `--control` requires
`--storage`. One `--storage URI` mounts a Dataset named `default`. Repeat
`--storage` to mount several Datasets; each default name is the URI's last
path component, and `NAME=URI` overrides it. `--control` uses the mount named
`default` (the implicit name for a single bare URI, or an explicit
`default=URI` among several). The process writes one machine-readable
readiness record to stdout; its Control token is never written to stderr.

For `--storage`, `serve` first discovers validated non-empty canonical
`events.lance` Stores and converges each deterministic sibling `storyline`;
readiness is emitted only after all startup targets are fresh. It then keeps
discovering and maintaining projections with bounded concurrency and retry.
Projection failures remain outside the durable canonical write path, and
foreign destinations without matching lineage are never overwritten. When
Warehouse HTTP is running, successful projection publication automatically
rebuilds and swaps the Warehouse Catalog. Observe the state without mutation:

```bash
pchronicle status ./trajectory-data --format json
```

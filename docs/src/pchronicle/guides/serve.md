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
a new Catalog Snapshot before switching readers to it.

To capture new LLM traffic in the same process, continue with
[Gateway forwarding, rewriting, and capture](serve-gateway.md). For exact flags, see the
[`pchronicle` command reference](../reference/cli.md). For Snapshot behavior,
read the [Dataset Catalog design](../design/catalog.md).

# Serve Datasets locally

```bash
pchronicle serve ./trajectory-data
pchronicle serve --listen 127.0.0.1:8080 train=./train eval=./eval
pchronicle serve --catalog-config catalog.toml --listen 127.0.0.1:8081
```

The listener is loopback-only. Named mounts provide stable SQL aliases.
`--open` opens the Web UI; `--home-link TEXT=PATH` adds same-origin navigation
links. `--catalog-config` enables authenticated Directory and data operations
and cannot be combined with positional Dataset mounts. Configure datasets and
user grants with `pchronicle serve catalog`; storage credentials remain scoped
to each backend and worker. The server emits a readiness JSON record containing
`warehouse_endpoint`.

Refresh builds a replacement Snapshot before switching readers. Failed refresh
keeps the previous view readable. Dataset writes remain CLI import operations.
Logs go to stderr; `--log-level` controls verbosity. Failed API requests include
`code`, `message`, and `request_id`; internal details remain in the server log.

[CLI reference](../reference/cli.md) · [Web UI](ui.md)

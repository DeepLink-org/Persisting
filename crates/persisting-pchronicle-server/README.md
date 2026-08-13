# pChronicle server

Loopback-only, read-only HTTP boundary for statically mounted pChronicle
Datasets. The crate owns request validation, response limits, API routing, and
serving the compiled `pchronicle-web` assets. It calls `persisting-pchronicle`
for catalog and query behavior; it does not define trajectory schemas or write
to mounted Datasets.

The public product entry point is:

```bash
pchronicle serve --config warehouse.toml
```

The standalone `pchronicle` command rejects non-loopback listeners because the
server has no authentication. See the
[`pchronicle` command reference](../../docs/src/design/cli-pchronicle.md) for
the Warehouse configuration and current contract.

Run the server contract tests with:

```bash
cargo test -p persisting-pchronicle-server --lib --test http_contract
```

Endpoint-specific test ownership is documented in [`tests/README.md`](tests/README.md).

# pChronicle Web

**Dioxus frontend for the pChronicle read-only Warehouse.**

Owns browser navigation, rendering, and client-side interaction. This is an
independent Cargo workspace (`pchronicle-web/Cargo.toml`), not a module of
`persisting-pchronicle` Core.

Does not own the HTTP API or Dataset storage.
[`persisting-pchronicle-cli`](../crates/persisting-pchronicle-cli/README.md)
exposes the read-only Warehouse API and embeds the staged build assets at
compile time (`web-assets/public`, with `web-fallback` when that tree is
absent). `just chronicle-web-build` compiles and stages those assets;
`Dioxus.toml` sets `out_dir` to `../crates/persisting-pchronicle-cli/web-assets`.

There is no `package.json`; development uses Dioxus (`dx`) and Cargo.

## Develop

```bash
just chronicle-web-dev
just chronicle-web-build
just chronicle-binary
cargo nextest run --manifest-path pchronicle-web/Cargo.toml --locked
```

`just chronicle-web-dev` runs `dx serve` against a separately running
pChronicle server. `just chronicle-binary` stages the UI and builds a runnable
`pchronicle` test binary.

## Links

- [Local Dataset Web UI](../docs/src/pchronicle/guides/ui.md)
- [Local read-only Dataset server](../docs/src/pchronicle/guides/serve.md)
- [`persisting-pchronicle-cli`](../crates/persisting-pchronicle-cli/README.md)

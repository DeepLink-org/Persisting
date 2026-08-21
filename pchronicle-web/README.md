# pChronicle Web

Dioxus frontend for the pChronicle read-only Warehouse. This is an independent
Web project, not a module of `persisting-pchronicle` Core. It consumes the
read-only HTTP API exposed by `persisting-pchronicle-cli` and owns browser
navigation, rendering, and client-side interaction only.

For local frontend development, start a compatible server and run:

```bash
just chronicle-web-dev
```

For the product binary, `just chronicle-web-build` compiles and stages assets
that the server embeds at build time. Verify the standalone project with:

```bash
cargo test --manifest-path pchronicle-web/Cargo.toml --locked
```

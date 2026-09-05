# Persisting Documentation

The documentation site is built with [Docusaurus](https://docusaurus.io/).
English and Chinese are separate documentation instances so each language has
stable routes, sidebars, and searchable content.

## Quick start

Run these commands from the repository root:

```bash
just docs-sync    # install website dependencies
just docs-serve   # start the local preview
just docs-build   # build docs/website/build
just docs-links   # build with broken-link checks enabled
```

The site entry point is `docs/website/`. Markdown is organized by product in
`docs/website/docs/en/` and `docs/website/docs/zh/`. The React homepage lives
in `docs/website/src/pages/index.js`; shared visual tokens live in
`docs/website/src/css/custom.css`.

The public reading path is intentionally progressive:

1. **Start here** explains which product matches the reader's task.
2. **Get Started** completes one useful Run or Dataset workflow.
3. **Concepts** names the stable objects and guarantees.
4. **Guides** solve task-shaped problems with verification steps.
5. **Design and Reference** document implementation boundaries and exact syntax.

The `ppilot` material remains outside the public navigation. Queue, search, and
standalone capture documentation remain separate project areas and are not part
of the default product onboarding path.

## Contributing

Edit the Markdown under `docs/website/docs/` and the React/CSS files under
`docs/website/src/`. Check command examples against the corresponding binary's
`--help`, then run `just docs-build` and `just docs-links` before opening a PR.

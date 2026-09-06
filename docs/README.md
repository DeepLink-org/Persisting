# Persisting Documentation

The documentation site is built with [Docusaurus](https://docusaurus.io/).
English and Chinese are separate documentation instances so each language has
stable routes, sidebars, and searchable content.

## Quick start

Run these commands from the repository root:

```bash
just docs-sync    # install website dependencies
just docs-serve   # build and start a stable static preview
just docs-serve-dirty # start the hot-reload development server
just docs-build   # build docs/website/build
just docs-links   # build with broken-link checks enabled
```

The published site includes a local search index, so search works on the static
GitHub Pages deployment without a separate service.

The canonical Markdown sources are `docs/src/en/` and `docs/src/zh/`. Docusaurus
reads these two trees directly; internal pPilot, Queue, and API material lives
under `docs/src/internal/` and stays outside the public site.
The Docusaurus shell lives in `docs/website/`, with the React homepage in
`docs/website/src/pages/index.js` and shared visual tokens in
`docs/website/src/css/custom.css`.

Use `docs-serve` for a stable preview of the generated site. Use
`docs-serve-dirty` only while editing and needing hot reload; it runs the
Docusaurus watcher and uses substantially more memory for a large bilingual
site. The stable preview is available at
`http://localhost:3000/Persisting/`, matching the GitHub Pages base path.

The public reading path is intentionally progressive:

1. **Start here** explains which product matches the reader's task.
2. **Get Started** completes one useful Run or Dataset workflow.
3. **Concepts** names the stable objects and guarantees.
4. **Guides** solve task-shaped problems with verification steps.
5. **Design and Reference** document implementation boundaries and exact syntax.

Internal orchestration material remains outside the public navigation. Queue,
search, and standalone capture documentation remain separate project areas and
are not part of the default product onboarding path.

## Contributing

Edit the Markdown under `docs/src/en/` or `docs/src/zh/`, and the React/CSS files under
`docs/website/src/`. Check command examples against the corresponding binary's
`--help`, then run `just docs-build` and `just docs-links` before opening a PR.

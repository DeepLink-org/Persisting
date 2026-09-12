# Persisting Documentation

The documentation site is built with [Zensical](https://zensical.org/). English and Chinese live in one site under `docs/src/en/` and `docs/src/zh/`, so the header language selector stays consistent while the URL remains stable per language.

```bash
just docs-sync          # create docs/.venv and install Zensical
just docs-serve         # build and serve the static site
just docs-serve-dirty   # hot-reload preview
just docs-build         # build docs/site
```

The landing page template is `docs/overrides/home.html`; visual tokens and the grid background are in `docs/src/stylesheets/extra.css`. Edit Markdown in `docs/src/en/` or `docs/src/zh/`, then run `just docs-build` before opening a PR.

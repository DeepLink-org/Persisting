# Persisting Documentation

The site uses Zensical 0.0.61. English and Chinese Markdown live under
`docs/src/en/` and `docs/src/zh/`, with matching relative paths.

```bash
just docs-sync          # install the same pinned Zensical version as CI
just docs-serve         # build both languages and serve on 127.0.0.1:3000
just docs-serve-dirty   # watch sources; rebuild both languages (refresh the browser)
just docs-build         # produce docs/site
python3 scripts/check-docs.py  # validate all generated pages and links
```

`scripts/build-docs.py` renders the English configuration, then renders Chinese
pages with Zensical's native Chinese theme and a translated navigation tree.
Both use `docs/zensical.toml` as the navigation source. The language selector
uses relative links to the corresponding article, so local preview and the
GitHub Pages `/Persisting/` deployment both work. Keep locale paths paired;
`check-docs.py` checks their links, navigation and HTML language attributes.

`docs/overrides/home.html` overrides the native content block for the full-width
homepage. The header, mobile drawer, sidebars and table of contents remain
Zensical components. `docs/src/stylesheets/extra.css` supplies the shared blue
gradient, grid, brand contrast and homepage layout. Use native Markdown fences,
`!!! note` / `!!! tip` callouts, and relative image paths.

CI uses the same bilingual build and page checks before uploading `docs/site`.

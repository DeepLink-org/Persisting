# Persisting Documentation

This folder contains the documentation for Persisting.

## Prerequisites

- Python 3.10+
- [uv](https://docs.astral.sh/uv/) package manager

## Quick Start

```bash
cd docs

# Local preview (auto-reload on src/ changes)
make serve

# Force rebuild if auto-reload isn't working
make serve-dirty

# Build static site
make build

# Check dead links
make check-links
```

## Structure

```
docs/
├── mkdocs.yml              # MkDocs configuration
├── pyproject.toml          # Python dependencies
├── Makefile                # Build commands
├── overrides/              # Theme customizations
│   └── home.html           # Custom homepage template
└── src/                    # Documentation source files
    ├── index.md            # Product overview
    ├── installation.md     # Start here: installation
    ├── quickstart.md       # Start here: first workflows
    ├── guide/              # Capability-oriented user guides
    ├── api/                # Public Python API reference
    ├── design/             # Architecture, internals, and CLI reference
    │   └── references/     # Research inputs; not product contracts
    └── dev/                # Contributor-facing implementation notes
```

## Translation

Documentation supports English and Chinese. For each user-facing `.md` file
where a translation is maintained:

- `file.md` — English version
- `file.zh.md` — Chinese version

Not all files have both language versions. The i18n plugin falls back to English when a Chinese version is missing.

## Contributing

1. `cd docs` and run `make serve` for local preview
2. Edit files under `src/` — browser should auto-reload
3. Submit a PR

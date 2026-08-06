# Persisting Documentation

This folder contains the documentation for Persisting.

## Prerequisites

- Python 3.10+
- [uv](https://docs.astral.sh/uv/) package manager

## Quick Start

Run the documentation tasks from the repository root:

```bash
# Install the locked documentation environment
just docs-sync

# Local preview (auto-reload on src/ changes)
just docs-serve

# Force rebuild if auto-reload isn't working
just docs-serve-dirty

# Build static site
just docs-build

# Check dead links
just docs-links
```

## Structure

```
docs/
├── mkdocs.yml              # MkDocs configuration
├── pyproject.toml          # Python dependencies
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

1. Run `just docs-serve` for local preview
2. Edit files under `src/` — browser should auto-reload
3. Submit a PR

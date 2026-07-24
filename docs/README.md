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
    ├── index.md            # Homepage
    ├── installation.md     # Installation guide
    ├── quickstart.md       # Quick start (5 paths)
    ├── guide/              # User guides
    │   ├── capture.md      # Trajectory capture
    │   ├── queue.md        # Streaming queue
    │   ├── tensor-memory.md # TTAS tensor memory
    │   ├── search.md       # Agent search
    │   ├── compute.md      # Task orchestration
    │   └── custom-backends.md # Custom storage backends
    ├── api/                # API reference
    │   ├── tensor-memory.md
    │   ├── queue.md
    │   ├── search.md
    │   └── ttas.md
    ├── design/             # Design documents
    │   ├── architecture.md
    │   ├── ttas.md
    │   ├── distributed-tiered-storage.md
    │   ├── capture.md
    │   ├── trajectory.md
    │   ├── compute.md
    │   ├── cli.md
    │   └── references/
    └── dev/                # Implementation tracking
        └── tiered-storage-steps.md
```

## Translation

Documentation supports English and Chinese. For each `.md` file:

- `file.md` — English version
- `file.zh.md` — Chinese version

Not all files have both language versions. The i18n plugin falls back to English when a Chinese version is missing.

## Contributing

1. `cd docs` and run `make serve` for local preview
2. Edit files under `src/` — browser should auto-reload
3. Submit a PR

# Persisting Documentation

This folder contains the documentation for Persisting.

## Prerequisites

- Python 3.10+
- [uv](https://docs.astral.sh/uv/) package manager

## Quick Start

```bash
# Serve documentation locally
make serve

# Build static site
make build

# Check for broken links
make check-links
```

## Structure

```
docs/
├── mkdocs.yml          # MkDocs configuration
├── pyproject.toml      # Python dependencies
├── Makefile            # Build commands
├── overrides/          # Theme customizations
│   └── home.html       # Custom homepage template
└── src/                # Documentation source files
    ├── index.md        # Homepage content
    ├── installation.md # Installation guide
    ├── quickstart.md   # Quick start guide
    ├── guide/          # User guide
    ├── design/         # Design documents
    ├── api_reference.md # API reference
    └── assets/         # Static assets
        └── stylesheets/
            └── home.css
```

## Contributing

1. Make changes to files in `src/`
2. Run `make serve` to preview locally
3. Submit a pull request

## Translation

Documentation supports English and Chinese. For each `.md` file:
- `file.md` - English version
- `file.zh.md` - Chinese version


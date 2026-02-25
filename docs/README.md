# Persisting Documentation

This folder contains the documentation for Persisting.

## Prerequisites

- Python 3.10+
- [uv](https://docs.astral.sh/uv/) package manager

## Quick Start

**注意**：请在 **`docs/` 目录下**执行 `make`，否则 `make serve` 监听的路径不对，改文件后页面不会自动刷新。

```bash
cd docs

# 本地预览（改 src/ 下文件后应自动刷新）
make serve

# 若未自动刷新：试 make serve-dirty，或浏览器强制刷新 (Ctrl+Shift+R / Cmd+Shift+R)
make serve-dirty

# 构建静态站
make build

# 检查死链
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

1. `cd docs` 后执行 `make serve` 启动本地预览
2. 修改 `src/` 下的文件，浏览器应自动刷新；若不刷新可试 `make serve-dirty` 或强制刷新页面
3. 提交 PR

## Translation

Documentation supports English and Chinese. For each `.md` file:
- `file.md` - English version
- `file.zh.md` - Chinese version


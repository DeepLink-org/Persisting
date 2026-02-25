.PHONY: help serve serve-dirty build clean install dev-install check-links sync

help:
	@echo "Available commands:"
	@echo "  make serve        - Serve documentation locally (auto-installs deps)"
	@echo "  make serve-dirty  - Serve with --dirtyreload (faster reload, try if serve doesn't update)"
	@echo "  make build       - Build documentation (auto-installs deps)"
	@echo "  make clean       - Clean build artifacts"
	@echo "  make check-links  - Check for broken links"
	@echo "  make sync        - Sync dependencies with pyproject.toml"

sync:
	uv sync --all-extras

# 必须在 docs/ 目录下执行 make serve，否则 watch 的路径不对，改文件不会触发刷新
serve:
	uv run mkdocs serve

# 若页面不随文件更新：先试 make serve-dirty；仍不行则浏览器强制刷新 (Ctrl+Shift+R / Cmd+Shift+R)
serve-dirty:
	uv run mkdocs serve --dirtyreload

build:
	uv run mkdocs build

clean:
	rm -rf site/
	rm -rf .cache/
	find . -type d -name "__pycache__" -exec rm -rf {} +

check-links:
	uv run mkdocs-linkcheck src


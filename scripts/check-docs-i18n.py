#!/usr/bin/env python3
"""Fail if translatable docs pages are missing a language twin.

For each ``docs/src/**/foo.md`` (not ``.zh.md``), require ``foo.zh.md``.
Also flag orphan ``.zh.md`` files whose English twin is missing.

Exempt (intentionally English-only or out of the published tree):

- RFC bodies under ``docs/src/rfcs/*.md`` except ``rfcs/index.md``. RFC
  bodies are historical decision records; translating them would create a
  second, drift-prone snapshot. ``rfcs/index.md`` stays bilingual so
  Chinese readers can find that policy.
- ``docs/src/archive/**`` is excluded from the MkDocs build (and, in this
  repo, archived pages live outside ``docs/src/`` entirely).
- Queue-subsystem pages listed in ``EN_ONLY`` stay English-only per
  AGENTS.md (out of documentation scope).
"""

from __future__ import annotations

import sys
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent / "docs" / "src"

EN_ONLY = {
    "api/index.md",
    "api/queue.md",
    "guide/custom-backends.md",
    "guide/queue.md",
}


def is_archive(rel: str) -> bool:
    return rel == "archive" or rel.startswith("archive/")


def is_rfc_body(rel: str) -> bool:
    return rel.startswith("rfcs/") and rel != "rfcs/index.md"


def is_translatable(rel: str) -> bool:
    if is_archive(rel) or rel in EN_ONLY or is_rfc_body(rel):
        return False
    return True


def chinese_twin(page: Path) -> Path:
    return page.with_name(page.name[:-3] + ".zh.md")


def english_twin(page: Path) -> Path:
    return page.with_name(page.name.removesuffix(".zh.md") + ".md")


def collect_violations(src: Path = SRC) -> list[str]:
    violations: list[str] = []
    for page in sorted(src.rglob("*.md")):
        rel = page.relative_to(src).as_posix()
        if is_archive(rel):
            continue
        if page.name.endswith(".zh.md"):
            if not english_twin(page).exists():
                violations.append(f"orphan zh: {rel}")
            continue
        if not is_translatable(rel):
            continue
        if not chinese_twin(page).exists():
            violations.append(f"missing zh: {rel}")
    return violations


def main() -> int:
    violations = collect_violations()
    if violations:
        for line in violations:
            print(line)
        return 1
    print("All translatable pages have Chinese counterparts.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

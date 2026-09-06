#!/usr/bin/env python3
"""Fail if published docs pages are missing a language twin.

The Docusaurus site keeps translations in parallel ``docs/src/en`` and ``docs/src/zh``
trees. Every public English page must therefore have a page at the same
relative path in the Chinese tree, and vice versa.

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

DOCS = Path(__file__).resolve().parent.parent / "docs" / "src"
LANGUAGES = ("en", "zh")

EN_ONLY = {
    "api/index.md",
    "api/queue.md",
    "guide/custom-backends.md",
    "guide/queue.md",
}

# Some reference material is intentionally authored only in Chinese while it
# is being stabilized. Keep it in the published tree without making CI block
# unrelated changes on a missing translation twin.
ZH_ONLY = {
    "pvisor/reference/cases.md",
}


def is_archive(rel: str) -> bool:
    return rel == "archive" or rel.startswith("archive/")


def is_rfc_body(rel: str) -> bool:
    return rel.startswith("rfcs/") and rel != "rfcs/index.md"


def is_translatable(rel: str) -> bool:
    if is_archive(rel) or rel in EN_ONLY or is_rfc_body(rel):
        return False
    return True


def collect_violations(src: Path = DOCS) -> list[str]:
    violations: list[str] = []
    trees = {language: src / language for language in LANGUAGES}
    pages = {
        language: {
            page.relative_to(root).as_posix()
            for page in sorted(root.rglob("*.md"))
        }
        for language, root in trees.items()
    }
    for language, other in (("en", "zh"), ("zh", "en")):
        for rel in sorted(pages[language]):
            if is_archive(rel) or rel in EN_ONLY or is_rfc_body(rel):
                continue
            if language == "zh" and rel in ZH_ONLY:
                continue
            if rel not in pages[other]:
                violations.append(f"missing {other}: {language}/{rel}")
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

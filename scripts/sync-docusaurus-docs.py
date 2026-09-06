#!/usr/bin/env python3
"""Materialize the public Docusaurus tree from the canonical docs/src tree."""
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'docs' / 'src'
DEST = ROOT / 'docs' / 'website' / 'docs'
EXCLUDED_DIRS = {'api', 'ppilot'}
EXCLUDED_FILES = {'guide/queue.md', 'guide/queue.zh.md', 'guide/custom-backends.md', 'guide/custom-backends.zh.md'}

def excluded(path: Path) -> bool:
    rel = path.relative_to(SOURCE).as_posix()
    return rel.split('/', 1)[0] in EXCLUDED_DIRS or rel in EXCLUDED_FILES

def main() -> None:
    if DEST.exists():
        shutil.rmtree(DEST)
    for lang in ('en', 'zh'):
        (DEST / lang).mkdir(parents=True)
    for path in SOURCE.rglob('*.md'):
        if excluded(path):
            continue
        rel = path.relative_to(SOURCE)
        if path.name.endswith('.zh.md'):
            target = DEST / 'zh' / rel.with_name(path.name[:-len('.zh.md')] + '.md')
        else:
            target = DEST / 'en' / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)
    assets = SOURCE / 'assets'
    for lang in ('en', 'zh'):
        if assets.exists():
            shutil.copytree(assets, DEST / lang / 'assets')

if __name__ == '__main__':
    main()

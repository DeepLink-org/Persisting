#!/usr/bin/env python3
"""Build both native Zensical navigation trees into one bilingual site."""
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

DOCS = Path(__file__).resolve().parents[1] / 'docs'


def build() -> None:
    zensical = shutil.which('zensical') or str(Path(sys.executable).with_name('zensical'))
    subprocess.run([zensical, 'build', '--strict'], cwd=DOCS, check=True)
    # Zensical has one canonical language/navigation per build. Reuse the same
    # content and search index, rendering Chinese HTML with its native zh theme.
    config = (DOCS / 'zensical.toml').read_text()
    before, nav = config.split('nav = [', 1)
    nav, after = nav.split('\n[project.theme]', 1)
    nav = nav.replace('"en/', '"zh/')
    for en, zh in {'Concepts': '概念', 'Guides': '指南', 'Design': '设计', 'Reference': '参考'}.items():
        nav = nav.replace('"' + en + '"', '"' + zh + '"')
    config = before + 'nav = [' + nav + '\n[project.theme]' + after
    config = config.replace('language = "en"', 'language = "zh"').replace('homepage = "en/"', 'homepage = "zh/"')
    with tempfile.TemporaryDirectory(prefix='.zensical-zh-', dir=DOCS) as temp:
        temp = Path(temp)
        config = config.replace('site_dir = "site"', f'site_dir = "{temp.name}/site"')
        config = config.replace('[project]\n', f'[project]\ncache_dir = "{temp.name}/cache"\n', 1)
        with tempfile.NamedTemporaryFile(mode='w', suffix='.toml', prefix='.zensical-zh-', dir=DOCS) as config_file:
            config_file.write(config)
            config_file.flush()
            subprocess.run([zensical, 'build', '--strict', '-f', config_file.name], cwd=DOCS, check=True)
        shutil.copytree(temp / 'site/zh', DOCS / 'site/zh', dirs_exist_ok=True)


if __name__ == '__main__':
    build()

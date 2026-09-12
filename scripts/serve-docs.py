#!/usr/bin/env python3
"""Serve a built Zensical site for local preview."""
from __future__ import annotations

import argparse
import subprocess
import sys
import threading
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


class PreviewHandler(SimpleHTTPRequestHandler):
    """Serve GitHub Pages-prefixed links from the root local preview."""

    def _strip_deploy_prefix(self) -> None:
        prefix = "/Persisting"
        if self.path == prefix or self.path.startswith(prefix + "/"):
            self.path = self.path[len(prefix):] or "/"

    def do_GET(self) -> None:  # noqa: N802 - stdlib handler API
        self._strip_deploy_prefix()
        super().do_GET()

    def do_HEAD(self) -> None:  # noqa: N802 - stdlib handler API
        self._strip_deploy_prefix()
        super().do_HEAD()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=3000)
    parser.add_argument("--directory", type=Path, default=Path("site"))
    parser.add_argument("--watch", action="store_true", help="Rebuild both languages when docs change")
    args = parser.parse_args()

    directory = args.directory.resolve()
    if not directory.is_dir():
        parser.error(f"directory does not exist: {directory}")

    if args.watch:
        docs = Path(__file__).resolve().parents[1] / "docs"
        def watch():
            def snapshot():
                files = [docs / "zensical.toml"]
                for folder in ("src", "overrides"):
                    files.extend(p for p in (docs / folder).rglob("*") if p.is_file())
                return {str(p): p.stat().st_mtime_ns for p in files}
            previous = snapshot()
            tick = threading.Event()
            while not tick.wait(1):
                current = snapshot()
                if current != previous:
                    result = subprocess.run([sys.executable, str(docs.parent / "scripts/build-docs.py")])
                    if result.returncode:
                        print("Docs rebuild failed; see errors above.", flush=True)
                    previous = current
        threading.Thread(target=watch, daemon=True).start()

    handler = partial(PreviewHandler, directory=str(directory))
    server = ThreadingHTTPServer((args.host, args.port), handler)
    print(f"Serving {directory} at http://{args.host}:{args.port}/", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()

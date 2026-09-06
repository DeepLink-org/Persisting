#!/usr/bin/env python3
"""Serve the Docusaurus build under the same base URL used by GitHub Pages."""

from __future__ import annotations

import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


class BasePathHandler(SimpleHTTPRequestHandler):
    base_path = "/Persisting"

    def do_GET(self) -> None:  # noqa: N802 - stdlib handler API
        if self.path == self.base_path:
            self.send_response(301)
            self.send_header("Location", f"{self.base_path}/")
            self.end_headers()
            return
        if self.path.startswith(f"{self.base_path}/"):
            self.path = self.path[len(self.base_path):] or "/"
        super().do_GET()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=3000)
    args = parser.parse_args()

    handler = partial(BasePathHandler, directory=str(args.directory))
    server = ThreadingHTTPServer((args.host, args.port), handler)
    print(f"Serving {args.directory} at http://{args.host}:{args.port}/Persisting/")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()

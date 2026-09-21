"""本机 HTTP 服务：POST /search、POST /login。"""

from __future__ import annotations

import argparse
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.parse import urlparse

from persisting.ldap_auth.client import LdapClient
from persisting.ldap_auth.config import LdapAuthConfig


def _read_json(handler: BaseHTTPRequestHandler) -> dict[str, Any]:
    """读取可选 JSON body；空 body 视为 {}。"""
    length = int(handler.headers.get("Content-Length", "0") or 0)
    if length <= 0:
        return {}
    raw = handler.rfile.read(length)
    if not raw.strip():
        return {}
    data = json.loads(raw.decode("utf-8"))
    if not isinstance(data, dict):
        raise ValueError("JSON body must be an object")
    return data


def _send(handler: BaseHTTPRequestHandler, status: int, payload: dict[str, Any]) -> None:
    """写 JSON 响应。"""
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json; charset=utf-8")
    handler.send_header("Content-Length", str(len(body)))
    handler.end_headers()
    handler.wfile.write(body)


def make_handler(client: LdapClient) -> type[BaseHTTPRequestHandler]:
    """生成绑定了 LdapClient 的请求处理器类。"""

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, fmt: str, *args: Any) -> None:
            sys.stderr.write("%s - %s\n" % (self.address_string(), fmt % args))

        def do_POST(self) -> None:  # noqa: N802
            path = urlparse(self.path).path.rstrip("/") or "/"
            try:
                if path == "/search":
                    body = _read_json(self)
                    filt = body.get("filter")
                    used, rows = client.search(str(filt) if filt is not None else None)
                    _send(self, 200, {"ok": True, "filter": used, "entries": rows})
                    return
                if path == "/login":
                    body = _read_json(self)
                    username = body.get("username")
                    password = str(body.get("password", ""))
                    result = client.login(
                        str(username) if username is not None else None,
                        password,
                    )
                    _send(self, 200, result)
                    return
                _send(
                    self,
                    404,
                    {"error": "not_found", "detail": "use POST /search or POST /login"},
                )
            except Exception as exc:  # noqa: BLE001 — 统一收口为 JSON 错误
                payload = LdapClient.error_payload(exc)
                status = 401 if payload.get("error") == "ldap_error" else 400
                if payload.get("error") == "not_found":
                    status = 404
                if payload.get("error") == "internal":
                    status = 500
                _send(self, status, payload)

        def do_GET(self) -> None:  # noqa: N802
            _send(
                self,
                405,
                {"error": "method_not_allowed", "detail": "POST /search or POST /login"},
            )

    return Handler


def serve(config: LdapAuthConfig, bind_password: str, port: int, host: str | None = None) -> None:
    """在 host:port 启动服务（阻塞）；host 默认取配置，可为 0.0.0.0 以接受局域网访问。"""
    listen_host = host or config.host
    client = LdapClient(config, bind_password)
    handler = make_handler(client)
    httpd = ThreadingHTTPServer((listen_host, port), handler)
    print(
        f"ldap_auth listening on http://{listen_host}:{port}  "
        f"POST /search  POST /login",
        flush=True,
    )
    httpd.serve_forever()


def build_parser() -> argparse.ArgumentParser:
    """启动参数：--config / --port / --password [/ --host]。"""
    p = argparse.ArgumentParser(description="Persisting local LDAP auth service")
    p.add_argument("--config", required=True, help="local JSON config path")
    p.add_argument("--port", type=int, required=True, help="listen port")
    p.add_argument(
        "--host",
        default=None,
        help="listen address (default: config host, use 0.0.0.0 for LAN)",
    )
    p.add_argument(
        "--password",
        required=True,
        help="AD bind password (service account); not stored in config file",
    )
    return p


def main(argv: list[str] | None = None) -> int:
    """CLI 入口。"""
    args = build_parser().parse_args(argv)
    config = LdapAuthConfig.load(args.config)
    serve(config, args.password, args.port, host=args.host)
    return 0

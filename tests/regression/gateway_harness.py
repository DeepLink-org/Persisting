"""Shared process and filesystem helpers for Gateway black-box regressions."""

from __future__ import annotations

import contextlib
import json
import os
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

PROXY_ENV_NAMES = {"all_proxy", "http_proxy", "https_proxy", "no_proxy"}


def append_jsonl(path: Path, value: dict[str, Any]) -> None:
    with path.open("a", encoding="utf-8") as handle:
        json.dump(value, handle, ensure_ascii=False, sort_keys=True)
        handle.write("\n")
        handle.flush()


def resolve_binary(env_name: str, default_path: Path, build_hint: str) -> Path:
    path = Path(os.environ.get(env_name, default_path)).expanduser().resolve()
    if not path.is_file() or not os.access(path, os.X_OK):
        raise RuntimeError(
            f"required binary is missing or not executable: {path}\n"
            f"build it first with: {build_hint}\n"
            f"or set {env_name} to an existing binary"
        )
    return path


def require_subcommand(binary: Path, subcommand: str, build_hint: str) -> None:
    result = subprocess.run(
        [str(binary), subcommand, "--help"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip().splitlines()[0] if result.stderr.strip() else "unsupported"
        raise RuntimeError(
            f"binary does not support {subcommand!r}: {binary}\n"
            f"reported: {detail}\n"
            f"rebuild it with: {build_hint}"
        )


def wait_http(url: str, process: subprocess.Popen[bytes], label: str) -> None:
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(
                f"{label} exited before becoming ready (status {process.returncode})"
            )
        try:
            with urllib.request.urlopen(url, timeout=0.5) as response:
                if 200 <= response.status < 300:
                    return
        except (OSError, urllib.error.URLError, TimeoutError):
            time.sleep(0.05)
    raise TimeoutError(f"timed out waiting for {label} at {url}")


def wait_logged_url(
    log_path: Path,
    prefix: str,
    process: subprocess.Popen[bytes],
    label: str,
) -> str:
    """Discover the address selected by a server that bound port zero."""
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if process.poll() is not None:
            details = log_path.read_text(encoding="utf-8", errors="replace")
            raise RuntimeError(
                f"{label} exited before publishing its address "
                f"(status {process.returncode}):\n{details}"
            )
        content = log_path.read_text(encoding="utf-8", errors="replace")
        for line in content.splitlines():
            if line.startswith(prefix):
                candidate = line[len(prefix) :].split()[0].rstrip("/")
                parsed = urllib.parse.urlparse(candidate)
                if parsed.scheme == "http" and parsed.hostname and parsed.port:
                    return candidate
        time.sleep(0.05)
    raise TimeoutError(f"timed out waiting for {label} address in {log_path}")


def stop_process(
    process: subprocess.Popen[bytes] | None,
    *,
    label: str,
    require_success: bool = False,
) -> None:
    if process is None:
        return
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
            raise RuntimeError(f"{label} did not stop after SIGTERM")
    if require_success and process.returncode != 0:
        raise RuntimeError(f"{label} exited with status {process.returncode}")


@contextlib.contextmanager
def without_proxy_environment():
    saved = {name: value for name, value in os.environ.items() if name.lower() in PROXY_ENV_NAMES}
    for name in saved:
        os.environ.pop(name, None)
    try:
        yield
    finally:
        os.environ.update(saved)

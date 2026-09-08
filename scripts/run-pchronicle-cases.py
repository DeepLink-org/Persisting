#!/usr/bin/env python3
"""Run executable bash examples embedded in pChronicle cases documents."""
from __future__ import annotations

import argparse
import datetime as dt
import os
import re
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

CASE_RE = re.compile(r"^##\s+([SP]\d{2})：?\s*(.*)$")
REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FIXTURES = REPO_ROOT / "examples" / "data"


def parse(path: Path):
    lines = path.read_text(encoding="utf-8").splitlines()
    out = []
    i = 0
    while i < len(lines):
        match = CASE_RE.match(lines[i])
        if not match:
            i += 1
            continue
        ident, title = match.groups()
        i += 1
        code = None
        while i < len(lines) and not CASE_RE.match(lines[i]):
            if lines[i].strip() == "```bash":
                i += 1
                buf = []
                while i < len(lines) and lines[i].strip() != "```":
                    buf.append(lines[i])
                    i += 1
                code = "\n".join(buf)
                break
            i += 1
        if code:
            out.append((ident, title, code))
    return out


def resolve_pchronicle(value: str) -> str:
    """Resolve to an absolute executable path.

    Case bodies run with cwd set to a temporary workspace, so relative paths
    like ``target/release/pchronicle`` must be absolutized against the repo
    checkout before substitution.
    """
    candidate = Path(value).expanduser()
    search = []
    if candidate.is_absolute():
        search.append(candidate)
    else:
        search.append(Path.cwd() / candidate)
        search.append(candidate)
    for path in search:
        if path.is_file() and os.access(path, os.X_OK):
            return str(path.resolve())
    which = shutil.which(value)
    if which:
        return which
    for relative in ("target/release/pchronicle", "target/debug/pchronicle"):
        path = Path.cwd() / relative
        if path.is_file() and os.access(path, os.X_OK):
            return str(path.resolve())
    raise SystemExit(
        f"pchronicle not found: {value!r}; build it or pass --pchronicle /absolute/path/to/pchronicle"
    )


def should_skip_manual(code: str) -> str | None:
    mode = os.environ.get("PCHRONICLE_CASE_MODE", "")
    if re.search(r"(^|\n)\s*pchronicle serve\s", code) and "serve" not in mode.split(","):
        return "server command requires a running client"
    if re.search(r"\bs3://", code) and "s3" not in mode.split(","):
        return "object-store case requires PCHRONICLE_CASE_MODE=s3 and a reachable endpoint"
    return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--document", type=Path, required=True)
    parser.add_argument("--pchronicle", default="pchronicle")
    parser.add_argument("--case", default="")
    parser.add_argument("--list", action="store_true")
    parser.add_argument("--keep", action="store_true")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument(
        "--fixtures",
        type=Path,
        default=DEFAULT_FIXTURES,
        help="Directory exposed to cases as PCHRONICLE_CASE_FIXTURES",
    )
    args = parser.parse_args()

    cases = parse(args.document)
    if args.list:
        for ident, title, _ in cases:
            print(f"{ident}\t{title}")
        return 0

    pchronicle = resolve_pchronicle(args.pchronicle)
    fixtures = args.fixtures.expanduser().resolve()
    if not fixtures.is_dir():
        raise SystemExit(f"fixtures directory not found: {fixtures}")

    wanted = {item for item in args.case.split(",") if item}
    cases = [case for case in cases if not wanted or case[0] in wanted]
    results = []

    for ident, title, code in cases:
        reason = should_skip_manual(code)
        if reason is not None:
            results.append((ident, "MANUAL", reason))
            print(f"{ident} MANUAL {title}")
            continue

        root = Path(tempfile.mkdtemp(prefix=f"pchronicle-{ident.lower()}-"))
        env = os.environ.copy()
        env["PCHRONICLE_CASE_WORKSPACE"] = str(root)
        env["PCHRONICLE_CASE_FIXTURES"] = str(fixtures)
        env["PCHRONICLE_BIN"] = pchronicle
        rendered = code.replace("pchronicle", pchronicle)
        try:
            completed = subprocess.run(
                ["bash", "-euo", "pipefail", "-c", rendered],
                cwd=root,
                env=env,
                text=True,
                capture_output=True,
                timeout=args.timeout,
            )
            ok = completed.returncode == 0
            status = "PASS" if ok else "FAIL"
            detail = (completed.stdout + completed.stderr).strip()
        except subprocess.TimeoutExpired as exc:
            status = "FAIL"
            detail = f"timeout after {args.timeout}s\n{exc.stdout or ''}"
        results.append((ident, status, detail))
        print(f"{ident} {status} {title}")
        if detail:
            print(detail[-2000:])
        if not args.keep:
            shutil.rmtree(root, ignore_errors=True)

    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        now = dt.datetime.now(dt.timezone.utc).isoformat()
        lines = [
            f"# pChronicle cases\n\nGenerated {now}.\n",
            "| Case | Status | Detail |",
            "|---|---|---|",
        ]
        lines += [
            f"| {ident} | {status} | {detail.replace(chr(10), ' ')[:500]} |"
            for ident, status, detail in results
        ]
        args.report.write_text("\n".join(lines) + "\n", encoding="utf-8")

    return 1 if any(status == "FAIL" for _, status, _ in results) else 0


if __name__ == "__main__":
    raise SystemExit(main())

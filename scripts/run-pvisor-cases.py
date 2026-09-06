#!/usr/bin/env python3
"""Execute the bash cases embedded in docs/src/zh/pvisor/reference/cases.md.

The Markdown document is the source of truth for the `pvisor run` behaviour
contract.  A case heading has the form ``- [ ] **A01：...**`` and may carry a
machine-readable annotation before its first bash fence:

    <!-- pvisor-case: expect=nonzero requires=rootless,curl -->

Unannotated cases default to ``expect=success`` with no extra prerequisites.

A case may declare a second bash fence, introduced by ``<!-- pvisor-assert -->``,
that runs after the command fence and turns the prose expectation into a real
check.  Assertion scripts get the helper vocabulary documented in the case list
(``bundle_expect``, ``bundle_contains``, ``stdout_has``, ...) plus
``$PVISOR_CASE_STDOUT`` holding the command fence's combined output.

The runner executes every selected case in a private temporary workspace and
can emit a Markdown report suitable for CI artifacts.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import os
from pathlib import Path
import re
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time

CASE_RE = re.compile(r"^- \[ \] \*\*([A-J][0-9]{2})：(.+?)\*\*\s*$")
META_RE = re.compile(r"^\s*<!--\s*pvisor-case:\s*(.*?)\s*-->\s*$")
ASSERT_RE = re.compile(r"^\s*<!--\s*pvisor-assert\s*-->\s*$")
VALID_EXPECTATIONS = {"success", "nonzero", "any"}
VALID_METADATA_KEYS = {"expect", "requires"}

HELPER_FILENAME = "pvisor-case-helper.py"

# Assertion vocabulary injected ahead of every `<!-- pvisor-assert -->` block.
# Keep this list in sync with the "断言助手" table in the case list.
ASSERT_PREAMBLE = """
_pvisor_helper() { "$PVISOR_CASE_PYTHON" "$PVISOR_CASE_HELPER" "$@"; }
bundle_path() { _pvisor_helper bundle path "$@"; }
bundle_get() { _pvisor_helper bundle get "$@"; }
bundle_expect() { _pvisor_helper bundle expect "$@"; }
bundle_contains() { _pvisor_helper bundle contains "$@"; }
record_get() { _pvisor_helper record get "$@"; }
record_expect() { _pvisor_helper record expect "$@"; }
record_contains() { _pvisor_helper record contains "$@"; }
stdout_has() {
  if ! grep -Fq -- "$1" "$PVISOR_CASE_STDOUT"; then
    printf 'assert: command output does not contain %s\\n' "$1" >&2
    return 1
  fi
}
stdout_matches() {
  if ! grep -Eq -- "$1" "$PVISOR_CASE_STDOUT"; then
    printf 'assert: command output does not match %s\\n' "$1" >&2
    return 1
  fi
}
"""

# Standalone helper so assertion scripts can inspect the Run Bundle without
# embedding heredocs in the Markdown document.
HELPER_SOURCE = '''#!/usr/bin/env python3
"""Run Bundle accessors for the pVisor Markdown case runner."""

from __future__ import annotations

import json
import os
from pathlib import Path
import sys


FAMILIES = {"bundle": "run-bundle.json", "record": "run.json"}


def newest(root: Path, filename: str) -> Path:
    candidates = [path for path in root.rglob(filename) if path.is_file()]
    if not candidates:
        raise SystemExit(f"assert: no {filename} under {root}")
    return max(candidates, key=lambda path: path.stat().st_mtime)


def lookup(document: object, dotted: str) -> object:
    cursor = document
    for segment in dotted.split("."):
        if isinstance(cursor, list):
            try:
                cursor = cursor[int(segment)]
            except (ValueError, IndexError) as error:
                raise SystemExit(f"assert: {dotted}: bad list index {segment!r}") from error
        elif isinstance(cursor, dict):
            if segment not in cursor:
                raise SystemExit(f"assert: {dotted}: missing key {segment!r}")
            cursor = cursor[segment]
        else:
            raise SystemExit(f"assert: {dotted}: {segment!r} has no container to index")
    return cursor


def render(value: object) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str):
        return value
    if isinstance(value, float) and value.is_integer():
        return str(int(value))
    if isinstance(value, (int, float)):
        return str(value)
    return json.dumps(value, ensure_ascii=False, sort_keys=True)


def main(argv: list[str]) -> int:
    if len(argv) < 2 or argv[0] not in FAMILIES:
        raise SystemExit("assert: usage: helper <bundle|record> <path|get|expect|contains> ...")
    filename = FAMILIES[argv[0]]
    command, arguments = argv[1], argv[2:]
    default_root = os.environ.get("PVISOR_CASE_ROOT", ".")

    def artifact(index: int) -> Path:
        root = Path(arguments[index] if len(arguments) > index else default_root)
        return newest(root, filename)

    if command == "path":
        print(artifact(0))
        return 0

    if command == "get":
        if not arguments:
            raise SystemExit("assert: usage: helper get <dotted.path> [ROOT]")
        document = json.loads(artifact(1).read_text(encoding="utf-8"))
        print(render(lookup(document, arguments[0])))
        return 0

    if command == "expect":
        if len(arguments) < 2:
            raise SystemExit("assert: usage: helper expect <dotted.path> <value> [ROOT]")
        source = artifact(2)
        document = json.loads(source.read_text(encoding="utf-8"))
        actual = render(lookup(document, arguments[0]))
        if actual != arguments[1]:
            raise SystemExit(
                f"assert: {arguments[0]} is {actual!r}, expected {arguments[1]!r} ({source})"
            )
        return 0

    if command == "contains":
        if len(arguments) < 2:
            raise SystemExit("assert: usage: helper contains <dotted.path> <substring> [ROOT]")
        source = artifact(2)
        document = json.loads(source.read_text(encoding="utf-8"))
        haystack = render(lookup(document, arguments[0]))
        if arguments[1] not in haystack:
            raise SystemExit(
                f"assert: {arguments[0]} does not contain {arguments[1]!r}; "
                f"value is {haystack!r} ({source})"
            )
        return 0

    raise SystemExit(f"assert: unknown helper command {command!r}")


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
'''


@dataclasses.dataclass(frozen=True)
class Case:
    case_id: str
    title: str
    code: str
    assertion: str
    expect: str
    requires: tuple[str, ...]


@dataclasses.dataclass(frozen=True)
class Result:
    case: Case
    status: str
    duration: float
    returncode: int | None
    output: str
    assert_output: str = ""
    reason: str = ""


def parse_metadata(text: str, case_id: str) -> tuple[str, tuple[str, ...]]:
    values: dict[str, str] = {}
    for item in shlex.split(text):
        if "=" not in item:
            raise ValueError(f"{case_id}: invalid metadata item {item!r}")
        key, value = item.split("=", 1)
        values[key] = value
    unknown = set(values) - VALID_METADATA_KEYS
    if unknown:
        raise ValueError(f"{case_id}: unknown metadata keys: {sorted(unknown)}")
    expect = values.get("expect", "success")
    if expect not in VALID_EXPECTATIONS:
        raise ValueError(f"{case_id}: invalid expectation {expect!r}")
    requires = tuple(filter(None, values.get("requires", "").split(",")))
    return expect, requires


def read_fence(lines: list[str], index: int, case_id: str) -> tuple[list[str], int]:
    """Collect a fenced block body, returning it with the index past the fence."""
    index += 1
    body: list[str] = []
    while index < len(lines) and lines[index].strip() != "```":
        body.append(lines[index])
        index += 1
    if index == len(lines):
        raise ValueError(f"{case_id}: unterminated bash fence")
    return body, index + 1


def parse_cases(document: Path) -> list[Case]:
    lines = document.read_text(encoding="utf-8").splitlines()
    cases: list[Case] = []
    index = 0
    while index < len(lines):
        match = CASE_RE.match(lines[index])
        if not match:
            index += 1
            continue
        case_id, title = match.groups()
        index += 1
        metadata = ""
        code: list[str] | None = None
        assertion: list[str] | None = None
        next_is_assertion = False
        while index < len(lines) and not CASE_RE.match(lines[index]):
            meta = META_RE.match(lines[index])
            if meta:
                metadata = meta.group(1)
            if ASSERT_RE.match(lines[index]):
                next_is_assertion = True
            if lines[index].strip() == "```bash":
                body, index = read_fence(lines, index, case_id)
                if next_is_assertion:
                    if assertion is not None:
                        raise ValueError(f"{case_id}: more than one assertion fence")
                    assertion = body
                    next_is_assertion = False
                elif code is None:
                    code = body
                continue
            index += 1
        if code is None:
            raise ValueError(f"{case_id}: no bash fence")
        expect, requires = parse_metadata(metadata, case_id)
        cases.append(
            Case(
                case_id,
                title,
                "\n".join(code).strip(),
                "\n".join(assertion or []).strip(),
                expect,
                requires,
            )
        )
    duplicate_ids = sorted(
        case_id
        for case_id in {case.case_id for case in cases}
        if sum(case.case_id == case_id for case in cases) != 1
    )
    if duplicate_ids:
        raise ValueError(f"duplicate case ids: {duplicate_ids}")
    return cases


def executable(value: str | None) -> str | None:
    if not value:
        return None
    candidate = Path(value).expanduser()
    if candidate.parent != Path(".") or candidate.is_absolute():
        return str(candidate.resolve()) if candidate.is_file() else None
    return shutil.which(value)


def resolve_pvisor(argument: str | None, repository: Path) -> Path:
    candidates = [argument, os.environ.get("PVISOR_BIN")]
    candidates.extend(
        str(repository / relative)
        for relative in ("target/release/pvisor", "target/debug/pvisor")
    )
    candidates.append("pvisor")
    for candidate in candidates:
        resolved = executable(candidate)
        if resolved:
            return Path(resolved)
    raise FileNotFoundError(
        "pvisor not found; build it or pass --pvisor /absolute/path/to/pvisor"
    )


def rootless_available() -> bool:
    unshare = shutil.which("unshare")
    if sys.platform != "linux" or not unshare:
        return False
    result = subprocess.run(
        [unshare, "--user", "--mount", "--pid", "--fork", "true"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return result.returncode == 0


def container_runtime() -> str | None:
    configured = os.environ.get("PVISOR_CASE_CONTAINER_RUNTIME")
    if configured:
        return executable(configured)
    return executable("crun") or executable("runc")


def requirement_reason(requirement: str) -> str | None:
    """Return None when the prerequisite holds, else a human-readable reason.

    Requirements are descriptive metadata only. Cases are intentionally not
    skipped: defaults are rendered and the real pVisor/runtime result is
    reported, including missing KVM, rootfs, images, or optional features.
    """
    environment = os.environ
    # Requirements document useful capabilities, but never suppress a case.
    # The runner always executes with safe defaults so the report contains the
    # real pVisor/runtime error instead of hiding it as SKIP.
    if requirement in {
        "linux", "kvm", "rootless", "curl", "python3", "rootfs", "image",
        "firmware", "container", "container-runtime", "agent", "lance",
    }:
        return None
    return f"unknown requirement {requirement!r}"


def free_loopback_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def prepare_workspace(workspace: Path) -> None:
    workspace.mkdir(parents=True)
    (workspace / "pvisor.toml").write_text(
        '[run]\ncommand = ["/bin/true"]\n', encoding="utf-8"
    )
    (workspace / "spec-without-extension").write_text(
        '[run]\ncommand = ["/bin/true"]\n', encoding="utf-8"
    )
    (workspace / "run-spec.json").write_text(
        '{"run_id":"case-i02","agent":{"name":"case-i02"},'
        '"invocation":{"kind":"process","program":"/bin/true"}}\n',
        encoding="utf-8",
    )


def render_command(code: str, case_root: Path, pvisor: Path, ports: dict[str, str]) -> str:
    """Substitute the document's placeholder paths with real fixtures.

    Placeholders keep the document readable for a human running a case by
    hand. Rendering defaults stay permissive so `--run-unavailable` still has
    something to execute; the prerequisite probes above decide whether a case
    is meaningful in the first place.
    """
    environment = os.environ
    default_rootfs = "/" if sys.platform == "linux" else "/path/to/rootfs"
    default_firmware = "/path/to/libkrunfw"
    firmware_cache = Path.home() / ".cache/persisting/pvisor/firmware/5.5.0"
    if (firmware_cache / "linux-x86_64").is_dir():
        default_firmware = str(firmware_cache / "linux-x86_64")
    replacements = {
        "/tmp/pvisor-cases": str(case_root),
        "./target/release/pvisor": str(pvisor),
        "/path/to/rootfs": environment.get("PVISOR_CASE_ROOTFS", default_rootfs),
        "/path/to/image": environment.get("PVISOR_CASE_IMAGE", "ubuntu:latest"),
        "/path/to/libkrunfw": environment.get("PVISOR_CASE_FIRMWARE", default_firmware),
        "/usr/local/bin/agent": environment.get("PVISOR_CASE_AGENT", "/bin/true"),
        "alpine:latest": environment.get("PVISOR_CASE_CONTAINER_IMAGE", "ubuntu:latest"),
        **ports,
    }
    for source, target in replacements.items():
        code = code.replace(source, target)
    return code


def expected(expectation: str, returncode: int) -> bool:
    return (
        expectation == "any"
        or (expectation == "success" and returncode == 0)
        or (expectation == "nonzero" and returncode != 0)
    )


def run_script(script: str, workspace: Path, environment: dict[str, str], timeout: float):
    return subprocess.run(
        ["bash", "-euo", "pipefail", "-c", script],
        cwd=workspace,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=timeout,
        check=False,
    )


def execute_case(
    case: Case,
    root: Path,
    pvisor: Path,
    timeout: float,
    run_unavailable: bool = False,
) -> Result:
    missing = [reason for item in case.requires if (reason := requirement_reason(item))]
    if missing and not run_unavailable:
        return Result(case, "SKIP", 0.0, None, "", reason="; ".join(missing))
    case_root = root / case.case_id.lower()
    workspace = case_root / "workspace"
    prepare_workspace(workspace)
    stdout_log = case_root / "command.log"
    # Documented listen addresses must map to the same free port in both the
    # command and the assertion so an assertion can grep for the real address.
    ports = {
        "127.0.0.1:18080": f"127.0.0.1:{free_loopback_port()}",
        "127.0.0.1:19090": f"127.0.0.1:{free_loopback_port()}",
    }
    command = render_command(case.code, case_root, pvisor, ports)
    environment = os.environ.copy()
    environment.update(
        {
            "PATH": f"{pvisor.parent}{os.pathsep}{environment.get('PATH', '')}",
            "PERSISTING_RUN_HOME": str(case_root / "records"),
            "PVISOR_CASE_ROOT": str(case_root),
            "PVISOR_CASE_WORKSPACE": str(workspace),
            "PVISOR_CASE_RECORDS": str(case_root / "records"),
            "PVISOR_CASE_STDOUT": str(stdout_log),
            "PVISOR_CASE_HELPER": str(root / HELPER_FILENAME),
            "PVISOR_CASE_PYTHON": sys.executable or "python3",
        }
    )
    started = time.monotonic()
    try:
        process = run_script(command, workspace, environment, timeout)
    except subprocess.TimeoutExpired as error:
        output = error.stdout or ""
        if isinstance(output, bytes):
            output = output.decode(errors="replace")
        return Result(case, "FAIL", time.monotonic() - started, None, output, reason="runner timeout")
    stdout_log.write_text(process.stdout, encoding="utf-8")
    if not expected(case.expect, process.returncode):
        return Result(
            case,
            "FAIL",
            time.monotonic() - started,
            process.returncode,
            process.stdout,
            reason=f"expected {case.expect}, exit={process.returncode}",
        )
    if not case.assertion:
        return Result(case, "PASS", time.monotonic() - started, process.returncode, process.stdout)
    environment["PVISOR_CASE_EXIT"] = str(process.returncode)
    # The assertion shares the command's placeholder rendering so a documented
    # listen address or fixture path resolves to the same value in both halves.
    assertion = render_command(case.assertion, case_root, pvisor, ports)
    try:
        checks = run_script(ASSERT_PREAMBLE + "\n" + assertion, workspace, environment, timeout)
    except subprocess.TimeoutExpired as error:
        output = error.stdout or ""
        if isinstance(output, bytes):
            output = output.decode(errors="replace")
        return Result(
            case,
            "FAIL",
            time.monotonic() - started,
            process.returncode,
            process.stdout,
            output,
            "assertion timeout",
        )
    status = "PASS" if checks.returncode == 0 else "FAIL"
    reason = "" if status == "PASS" else f"assertion failed, exit={checks.returncode}"
    return Result(
        case,
        status,
        time.monotonic() - started,
        process.returncode,
        process.stdout,
        checks.stdout,
        reason,
    )


def markdown_report(results: list[Result], pvisor: Path) -> str:
    counts = {status: sum(item.status == status for item in results) for status in ("PASS", "FAIL", "SKIP")}
    asserted = sum(bool(item.case.assertion) for item in results)
    lines = [
        "# pVisor case execution report",
        "",
        f"- Generated: {dt.datetime.now(dt.timezone.utc).isoformat()}",
        f"- pVisor: `{pvisor}`",
        f"- Result: {counts['PASS']} PASS / {counts['FAIL']} FAIL / {counts['SKIP']} SKIP",
        f"- Cases with assertions: {asserted}/{len(results)}",
        "",
        "| Case | Status | Exit | Assert | Time | Reason |",
        "|---|---:|---:|:-:|---:|---|",
    ]
    for result in results:
        exit_code = "-" if result.returncode is None else str(result.returncode)
        has_assert = "yes" if result.case.assertion else "-"
        lines.append(
            f"| {result.case.case_id} | {result.status} | {exit_code} | {has_assert} | "
            f"{result.duration:.2f}s | {result.reason.replace('|', '&#124;')} |"
        )
    for result in results:
        if not result.output and not result.assert_output:
            continue
        lines.extend(["", f"## {result.case.case_id} — {result.status}", ""])
        for label, body in (("command output", result.output), ("assertion output", result.assert_output)):
            if not body:
                continue
            lines.extend(
                [
                    f"<details><summary>{label}</summary>",
                    "",
                    "```text",
                    body.rstrip(),
                    "```",
                    "",
                    "</details>",
                    "",
                ]
            )
    return "\n".join(lines) + "\n"


def main() -> int:
    repository = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--document",
        type=Path,
        default=repository / "docs/src/zh/pvisor/reference/cases.md",
    )
    parser.add_argument("--pvisor", help="pvisor executable (or set PVISOR_BIN)")
    parser.add_argument("--case", action="append", dest="case_ids", help="case ID; repeatable")
    parser.add_argument("--list", action="store_true", help="list cases without executing")
    parser.add_argument("--timeout", type=float, default=120.0, help="per-case timeout in seconds")
    parser.add_argument("--report", type=Path, help="write a Markdown execution report")
    parser.add_argument("--keep", action="store_true", help="retain temporary case directories")
    parser.add_argument(
        "--run-unavailable",
        action="store_true",
        help="execute cases even when declared prerequisites are missing; report real failures",
    )
    parser.add_argument(
        "--strict-skips", action="store_true", help="treat skipped prerequisites as failures"
    )
    args = parser.parse_args()

    cases = parse_cases(args.document)
    if args.case_ids:
        requested = {item.upper() for value in args.case_ids for item in value.split(",")}
        known = {case.case_id for case in cases}
        unknown = requested - known
        if unknown:
            parser.error(f"unknown cases: {', '.join(sorted(unknown))}")
        cases = [case for case in cases if case.case_id in requested]
    if args.list:
        for case in cases:
            requirements = ",".join(case.requires) or "-"
            marker = "assert" if case.assertion else "-"
            print(f"{case.case_id}\t{case.expect}\t{marker}\t{requirements}\t{case.title}")
        return 0

    pvisor = resolve_pvisor(args.pvisor, repository)
    root = Path(tempfile.mkdtemp(prefix="pvisor-cases-"))
    (root / HELPER_FILENAME).write_text(HELPER_SOURCE, encoding="utf-8")
    print(f"pVisor: {pvisor}")
    print(f"case root: {root}")
    results: list[Result] = []
    for case in cases:
        result = execute_case(case, root, pvisor, args.timeout, args.run_unavailable)
        results.append(result)
        detail = f" ({result.reason})" if result.reason else ""
        print(f"[{result.status}] {case.case_id} {case.title}{detail}")
        if result.status == "FAIL":
            for body in (result.output, result.assert_output):
                if body:
                    print(body.rstrip())

    report = markdown_report(results, pvisor)
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(report, encoding="utf-8")
        print(f"report: {args.report}")
    failed = sum(result.status == "FAIL" for result in results)
    skipped = sum(result.status == "SKIP" for result in results)
    asserted = sum(bool(result.case.assertion) for result in results)
    print(f"summary: {len(results) - failed - skipped} PASS / {failed} FAIL / {skipped} SKIP")
    print(f"assertions: {asserted}/{len(results)} cases carry an assertion block")
    if args.keep:
        print(f"retained: {root}")
    else:
        shutil.rmtree(root, ignore_errors=True)
    return 1 if failed or (args.strict_skips and skipped) else 0


if __name__ == "__main__":
    raise SystemExit(main())

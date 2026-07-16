#!/usr/bin/env bash
# Reject release archives that expose private deployment details or credentials.
set -euo pipefail

archive="${1:?usage: validate-dlcapt-archive.sh <archive.tar.gz>}"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

tar -xzf "${archive}" -C "${tmp}"

python3 - "${tmp}/dlcapt-deploy" <<'PY'
from __future__ import annotations

import pathlib
import re
import sys

deploy = pathlib.Path(sys.argv[1])
if not deploy.is_dir():
    print("FAIL: archive does not contain dlcapt-deploy/", file=sys.stderr)
    sys.exit(1)

forbidden_literals = (
    "0.0.0.0",
    "wind-tunnel",
    "ailab-pj",
    "ssd1",
    "pjlab",
    "pjh-service",
)
private_config_name = re.compile(r"(?i)(?:^|[-_.])(?:online|beta)(?:[-_.]|$)")
private_config_reference = re.compile(
    r"(?im)\b[A-Za-z0-9_.-]*(?:online|beta)[A-Za-z0-9_.-]*\.toml\b"
)
credential_assignment = re.compile(
    r"""(?im)(?:^\s*(?:export\s+)?|[\{\[,]\s*)
    (?P<quote>["']?)(?P<name>
        api[_-]?key|
        (?:access[_-]?)?token|
        password|
        secret(?:[_-]?[A-Za-z0-9_-]+)?|
        aws[_-]?(?:access[_-]?key(?:[_-]?id)?|secret[_-]?access[_-]?key)
    )(?P=quote)\s*(?:=|:)\s*(?P<value>[^,\r\n}\#]*)
    (?=\s*(?:,|}|\#|$))""",
    re.VERBOSE,
)
private_key = re.compile(r"-----BEGIN (?:[A-Z0-9 ]+ )?PRIVATE KEY-----")


def is_text(data: bytes) -> bool:
    """Classify by bytes, not filename, to avoid scanning binaries."""
    if b"\0" in data:
        return False
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        return False
    controls = sum(
        ord(char) < 32 and char not in "\n\r\t\f\b" for char in text
    )
    return controls <= max(1, len(text) // 100)


def is_empty_value(value: str) -> bool:
    value = value.strip()
    return value in ("", '""', "''")


violations: list[str] = []
for path in sorted(deploy.rglob("*")):
    relative = path.relative_to(deploy).as_posix()
    if path.is_symlink():
        violations.append(f"{relative}: symlinks are not allowed in release archives")
        continue
    if not path.is_file():
        continue
    if private_config_name.search(path.name):
        violations.append(f"{relative}: private online/beta config name")

    data = path.read_bytes()
    if not is_text(data):
        continue
    text = data.decode("utf-8")
    lowered = text.lower()
    for value in forbidden_literals:
        if value in lowered:
            violations.append(f"{relative}: forbidden public value {value!r}")
    if private_config_reference.search(text):
        violations.append(f"{relative}: private online/beta config reference")
    if private_key.search(text):
        violations.append(f"{relative}: private key material")
    for match in credential_assignment.finditer(text):
        if not is_empty_value(match.group("value")):
            violations.append(
                f"{relative}: non-empty credential {match.group('name')!r}"
            )

if violations:
    for violation in violations:
        print(f"FAIL: {violation}", file=sys.stderr)
    sys.exit(1)

print("PASS public archive sanitization")
PY

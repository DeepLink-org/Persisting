#!/usr/bin/env python3
"""Validate the version contract for a stable Persisting release."""

from __future__ import annotations

import argparse
import ast
import re
import subprocess
import sys
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parents[2]
STABLE_TAG_RE = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")


class ReleaseValidationError(RuntimeError):
    """Raised when a checkout is not safe to publish."""


def _toml(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def _python_version(path: Path) -> str:
    module = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    for statement in module.body:
        if not isinstance(statement, (ast.Assign, ast.AnnAssign)):
            continue
        targets = statement.targets if isinstance(statement, ast.Assign) else [statement.target]
        if not any(
            isinstance(target, ast.Name) and target.id == "__version__" for target in targets
        ):
            continue
        value = statement.value
        if isinstance(value, ast.Constant) and isinstance(value.value, str):
            return value.value
        raise ReleaseValidationError(f"{path}: __version__ must be a string literal")
    raise ReleaseValidationError(f"{path}: __version__ assignment not found")


def read_versions(root: Path = ROOT) -> dict[str, str]:
    pyproject = _toml(root / "pyproject.toml")
    cargo = _toml(root / "Cargo.toml")
    try:
        return {
            "pyproject.toml": str(pyproject["project"]["version"]),
            "Cargo.toml": str(cargo["workspace"]["package"]["version"]),
            "persisting/__init__.py": _python_version(root / "persisting" / "__init__.py"),
        }
    except KeyError as error:
        raise ReleaseValidationError(f"missing version field: {error}") from error


def validate_versions(tag: str | None, root: Path = ROOT) -> str:
    versions = read_versions(root)
    unique_versions = set(versions.values())
    if len(unique_versions) != 1:
        details = ", ".join(f"{path}={version}" for path, version in versions.items())
        raise ReleaseValidationError(f"release versions do not match: {details}")

    version = next(iter(unique_versions))
    if tag is not None:
        match = STABLE_TAG_RE.fullmatch(tag)
        if match is None:
            raise ReleaseValidationError(f"release tag {tag!r} must use the stable format vX.Y.Z")
        tag_version = tag.removeprefix("v")
        if tag_version != version:
            raise ReleaseValidationError(
                f"release tag version {tag_version!r} does not match package version {version!r}"
            )
    elif STABLE_TAG_RE.fullmatch(f"v{version}") is None:
        raise ReleaseValidationError(
            f"package version {version!r} is not a stable X.Y.Z release version"
        )
    return version


def _run(command: list[str], root: Path) -> None:
    result = subprocess.run(
        command,
        cwd=root,
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or f"exit status {result.returncode}"
        raise ReleaseValidationError(f"{' '.join(command)} failed: {detail}")


def validate_lockfile(root: Path = ROOT) -> None:
    _run(["cargo", "metadata", "--format-version", "1", "--no-deps", "--locked"], root)


def validate_main_ancestry(main_ref: str, root: Path = ROOT) -> None:
    _run(["git", "rev-parse", "--verify", f"{main_ref}^{{commit}}"], root)
    _run(["git", "merge-base", "--is-ancestor", "HEAD", main_ref], root)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", help="stable release tag (vX.Y.Z); omit for a build-only check")
    parser.add_argument("--main-ref", help="require HEAD to be reachable from this git ref")
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--skip-lockfile", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()

    try:
        version = validate_versions(args.tag, args.root)
        if not args.skip_lockfile:
            validate_lockfile(args.root)
        if args.main_ref:
            validate_main_ancestry(args.main_ref, args.root)
    except ReleaseValidationError as error:
        print(f"release validation failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error

    print(version)


if __name__ == "__main__":
    main()

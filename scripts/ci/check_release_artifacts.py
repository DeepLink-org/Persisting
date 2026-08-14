#!/usr/bin/env python3
"""Check that a stable release contains one wheel for every supported platform."""

from __future__ import annotations

import argparse
import re
import sys
import zipfile
from email.parser import BytesParser
from pathlib import Path

DEFAULT_MAX_BYTES = 100_000_000
PLATFORM_PATTERNS = {
    "linux-x86_64": re.compile(r"^manylinux.*_x86_64$"),
    "macos-arm64": re.compile(r"^macosx.*_arm64$"),
}


class ArtifactValidationError(RuntimeError):
    """Raised when the release artifact set is incomplete or inconsistent."""


def _metadata_version(wheel: Path) -> str:
    with zipfile.ZipFile(wheel) as archive:
        metadata_files = [
            name for name in archive.namelist() if name.endswith(".dist-info/METADATA")
        ]
        if len(metadata_files) != 1:
            raise ArtifactValidationError(
                f"{wheel.name}: expected one METADATA file, found {len(metadata_files)}"
            )
        metadata = BytesParser().parsebytes(archive.read(metadata_files[0]))
    version = metadata.get("Version")
    if not version:
        raise ArtifactValidationError(f"{wheel.name}: METADATA has no Version")
    return version


def validate_artifacts(
    directory: Path,
    version: str,
    *,
    max_bytes: int = DEFAULT_MAX_BYTES,
) -> dict[str, Path]:
    wheels = sorted(directory.glob("*.whl"))
    if len(wheels) != len(PLATFORM_PATTERNS):
        names = ", ".join(wheel.name for wheel in wheels) or "none"
        raise ArtifactValidationError(
            f"expected {len(PLATFORM_PATTERNS)} wheels, found {len(wheels)}: {names}"
        )

    prefix = f"persisting-{version}-py3-none-"
    found: dict[str, Path] = {}
    for wheel in wheels:
        if wheel.stat().st_size > max_bytes:
            raise ArtifactValidationError(
                f"{wheel.name}: {wheel.stat().st_size} bytes exceeds {max_bytes}"
            )
        if not wheel.name.startswith(prefix) or not wheel.name.endswith(".whl"):
            raise ArtifactValidationError(
                f"{wheel.name}: expected filename prefix {prefix!r} and a .whl suffix"
            )
        platform_tag = wheel.name[len(prefix) : -len(".whl")]
        matches = [
            name for name, pattern in PLATFORM_PATTERNS.items() if pattern.fullmatch(platform_tag)
        ]
        if len(matches) != 1:
            raise ArtifactValidationError(
                f"{wheel.name}: unsupported or ambiguous platform tag {platform_tag!r}"
            )
        platform_name = matches[0]
        if platform_name in found:
            raise ArtifactValidationError(
                f"duplicate {platform_name} wheels: {found[platform_name].name}, {wheel.name}"
            )
        metadata_version = _metadata_version(wheel)
        if metadata_version != version:
            raise ArtifactValidationError(
                f"{wheel.name}: METADATA version {metadata_version!r} != {version!r}"
            )
        found[platform_name] = wheel

    missing = set(PLATFORM_PATTERNS) - set(found)
    if missing:
        raise ArtifactValidationError(f"missing wheels for: {', '.join(sorted(missing))}")
    return found


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--max-bytes", type=int, default=DEFAULT_MAX_BYTES)
    args = parser.parse_args()

    try:
        found = validate_artifacts(args.directory, args.version, max_bytes=args.max_bytes)
    except ArtifactValidationError as error:
        print(f"release artifact validation failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error

    for platform_name, wheel in sorted(found.items()):
        print(f"{platform_name}={wheel.name} bytes={wheel.stat().st_size}")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Build and stage the native CLI component set for a Persisting wheel."""

from __future__ import annotations

import argparse
import json
import os
import platform
import shlex
import shutil
import stat
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping, Sequence

ROOT = Path(__file__).resolve().parents[2]
WHEEL_DATA = ROOT / "target" / "wheel-data"
EXPECTED_BINARIES = ("persisting", "pvisor", "ppilot")
SUPPORTED_TARGETS = {
    "x86_64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
}


@dataclass(frozen=True)
class BuildOptions:
    target: str | None = None
    profile: str = "release"
    target_dir: str | None = None
    locked: bool = True
    frozen: bool = False
    offline: bool = False
    jobs: str | None = None


def ensure_wheel_data_directory() -> Path:
    """Create the Maturin data layout without compiling the CLI binaries."""
    scripts = WHEEL_DATA / "scripts"
    scripts.mkdir(parents=True, exist_ok=True)
    return scripts


def _option(args: Sequence[str], *names: str) -> str | None:
    for index, arg in enumerate(args):
        for name in names:
            if arg == name:
                if index + 1 >= len(args):
                    raise RuntimeError(f"{name} requires a value")
                return args[index + 1]
            if arg.startswith(f"{name}="):
                return arg.split("=", 1)[1]
    return None


def _jobs(args: Sequence[str]) -> str | None:
    value = _option(args, "--jobs", "-j")
    if value is not None:
        return value
    for arg in args:
        if arg.startswith("-j") and len(arg) > 2:
            return arg[2:]
    return None


def _has_flag(args: Sequence[str], name: str) -> bool:
    return name in args


def _normalize_target(target: str | None) -> str | None:
    if target is None:
        machine = platform.machine().lower()
        if sys.platform == "linux" and machine in {"x86_64", "amd64"}:
            return None
        if sys.platform == "darwin" and machine in {"x86_64", "amd64", "arm64", "aarch64"}:
            return None
        raise RuntimeError(
            f"wheel CLI staging is not supported on host {sys.platform}/{platform.machine()}"
        )

    aliases = {
        "x86_64": "x86_64-apple-darwin" if sys.platform == "darwin" else "x86_64-unknown-linux-gnu",
        "aarch64": "aarch64-apple-darwin"
        if sys.platform == "darwin"
        else "aarch64-unknown-linux-gnu",
        "arm64": "aarch64-apple-darwin",
    }
    normalized = aliases.get(target, target)
    if normalized not in SUPPORTED_TARGETS:
        supported = ", ".join(sorted(SUPPORTED_TARGETS))
        raise RuntimeError(f"unsupported wheel target {normalized!r}; expected one of: {supported}")
    return normalized


def options_from_maturin(
    config_settings: Mapping[str, Any] | None,
    *,
    editable: bool,
) -> BuildOptions:
    """Resolve Cargo options from the same inputs consumed by Maturin's backend."""
    import maturin

    args = maturin.get_maturin_pep517_args(config_settings)
    config = maturin.get_config()
    if _has_flag(args, "--zig"):
        raise RuntimeError(
            "PEP 517 wheel builds with --zig are unsupported because the staged CLI binaries "
            "would not share Maturin's linker; build official Linux wheels in manylinux instead"
        )

    profile_key = "editable-profile" if editable else "profile"
    default_profile = config.get(profile_key) or config.get("profile") or "release"
    target = _option(args, "--target") or os.getenv("CARGO_BUILD_TARGET")
    target_dir = _option(args, "--target-dir")
    return BuildOptions(
        target=_normalize_target(target),
        profile=_option(args, "--profile") or str(default_profile),
        target_dir=target_dir,
        locked=_has_flag(args, "--locked") or bool(config.get("locked", True)),
        frozen=_has_flag(args, "--frozen") or bool(config.get("frozen", False)),
        offline=_has_flag(args, "--offline") or bool(config.get("offline", False)),
        jobs=_jobs(args),
    )


def _cargo_command(options: BuildOptions) -> list[str]:
    command = [
        "cargo",
        "build",
        "--profile",
        options.profile,
        "--message-format=json-render-diagnostics",
        "-p",
        "persisting-cli",
        "--bin",
        "persisting",
        "-p",
        "persisting-pvisor",
        "--bin",
        "pvisor",
        "-p",
        "persisting-ppilot",
        "--features",
        "cli",
        "--bin",
        "ppilot",
    ]
    if options.target is not None:
        command.extend(("--target", options.target))
    if options.target_dir is not None:
        command.extend(("--target-dir", options.target_dir))
    if options.frozen:
        command.append("--frozen")
    elif options.locked:
        command.append("--locked")
    if options.offline:
        command.append("--offline")
    if options.jobs is not None:
        command.extend(("--jobs", options.jobs))
    return command


def _build(options: BuildOptions) -> dict[str, Path]:
    command = _cargo_command(options)
    print(f"Building wheel CLI component set: {shlex.join(command)}", file=sys.stderr)
    process = subprocess.Popen(
        command,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        text=True,
    )
    assert process.stdout is not None
    artifacts: dict[str, Path] = {}
    for line in process.stdout:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            print(line, end="", file=sys.stderr)
            continue
        if message.get("reason") == "compiler-message":
            rendered = message.get("message", {}).get("rendered")
            if rendered:
                print(rendered, end="", file=sys.stderr)
        if message.get("reason") != "compiler-artifact":
            continue
        executable = message.get("executable")
        name = message.get("target", {}).get("name")
        kinds = message.get("target", {}).get("kind", [])
        if executable and name in EXPECTED_BINARIES and "bin" in kinds:
            artifacts[name] = Path(executable)

    return_code = process.wait()
    if return_code != 0:
        raise subprocess.CalledProcessError(return_code, command)
    missing = sorted(set(EXPECTED_BINARIES) - artifacts.keys())
    if missing:
        raise RuntimeError(f"Cargo did not report expected wheel binaries: {', '.join(missing)}")
    return artifacts


def stage_wheel_binaries(options: BuildOptions) -> Path:
    """Build all host CLIs and atomically replace the wheel scripts directory."""
    artifacts = _build(options)
    ensure_wheel_data_directory()
    staged = WHEEL_DATA / f".scripts-{os.getpid()}"
    backup = WHEEL_DATA / f".scripts-old-{os.getpid()}"
    shutil.rmtree(staged, ignore_errors=True)
    shutil.rmtree(backup, ignore_errors=True)
    staged.mkdir()

    try:
        for name in EXPECTED_BINARIES:
            source = artifacts[name]
            destination = staged / name
            shutil.copy2(source, destination)
            destination.chmod(
                destination.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH
            )
            print(f"Staged {name}: {source} -> {destination}", file=sys.stderr)

        scripts = WHEEL_DATA / "scripts"
        if scripts.exists():
            os.replace(scripts, backup)
        try:
            os.replace(staged, scripts)
        except BaseException:
            if backup.exists():
                os.replace(backup, scripts)
            raise
        shutil.rmtree(backup, ignore_errors=True)
        return scripts
    finally:
        shutil.rmtree(staged, ignore_errors=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target")
    parser.add_argument("--profile", default="release")
    parser.add_argument("--target-dir")
    parser.add_argument("--locked", action="store_true")
    parser.add_argument("--frozen", action="store_true")
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--jobs")
    args = parser.parse_args()
    options = BuildOptions(
        target=_normalize_target(args.target),
        profile=args.profile,
        target_dir=args.target_dir,
        locked=args.locked,
        frozen=args.frozen,
        offline=args.offline,
        jobs=args.jobs,
    )
    stage_wheel_binaries(options)


if __name__ == "__main__":
    main()

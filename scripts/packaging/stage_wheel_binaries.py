#!/usr/bin/env python3
"""Build and stage the native CLI component set for a Persisting wheel."""

from __future__ import annotations

import argparse
import hashlib
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
from typing import Any, Mapping

ROOT = Path(__file__).resolve().parents[2]
WHEEL_DATA = ROOT / "target" / "wheel-data"
WEB_ROOT = ROOT / "pchronicle-web"
WEB_PUBLIC = ROOT / "crates" / "persisting-pchronicle-cli" / "web-assets" / "public"
DX_PUBLIC = WEB_ROOT / "target" / "dx" / "pchronicle-web" / "release" / "web" / "public"
EXPECTED_BINARIES = ("pchronicle",)
SUPPORTED_TARGETS = {
    "x86_64-unknown-linux-gnu",
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
    """Create the wheel scripts layout without compiling the CLI binaries."""
    scripts = WHEEL_DATA / "scripts"
    scripts.mkdir(parents=True, exist_ok=True)
    return scripts


def _setting(config: Mapping[str, Any] | None, name: str) -> str | None:
    if not config:
        return None
    value = config.get(name)
    if value is None:
        value = config.get(f"--{name}")
    if isinstance(value, list):
        value = value[-1] if value else None
    return None if value is None else str(value)


def _bool_setting(config: Mapping[str, Any] | None, name: str, *, default: bool) -> bool:
    value = _setting(config, name)
    if value is None:
        return default
    normalized = value.strip().lower()
    if normalized in {"1", "true", "yes", "on"}:
        return True
    if normalized in {"0", "false", "no", "off"}:
        return False
    raise RuntimeError(f"{name} must be a boolean, got {value!r}")


def _normalize_target(target: str | None) -> str | None:
    if target is None:
        machine = platform.machine().lower()
        if sys.platform == "linux" and machine in {"x86_64", "amd64"}:
            return None
        if sys.platform == "darwin" and machine in {"arm64", "aarch64"}:
            return None
        raise RuntimeError(
            f"wheel CLI staging is not supported on host {sys.platform}/{platform.machine()}"
        )

    aliases = {
        "x86_64": "x86_64-unknown-linux-gnu",
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


def options_from_build_backend(
    config_settings: Mapping[str, Any] | None,
    *,
    editable: bool,
) -> BuildOptions:
    """Resolve Cargo options for the setuptools-backed PEP 517 build."""
    default_profile = "dev" if editable else "release"
    target = _setting(config_settings, "cargo-target") or os.getenv("CARGO_BUILD_TARGET")
    target_dir = _setting(config_settings, "cargo-target-dir") or os.getenv("CARGO_TARGET_DIR")
    return BuildOptions(
        target=_normalize_target(target),
        profile=_setting(config_settings, "cargo-profile") or default_profile,
        target_dir=target_dir,
        locked=_bool_setting(config_settings, "cargo-locked", default=True),
        frozen=_bool_setting(config_settings, "cargo-frozen", default=False),
        offline=_bool_setting(config_settings, "cargo-offline", default=False),
        jobs=_setting(config_settings, "cargo-jobs"),
    )


def _cargo_command(options: BuildOptions) -> list[str]:
    command = [
        "cargo",
        "build",
        "--profile",
        options.profile,
        "--message-format=json-render-diagnostics",
        "-p",
        "persisting-pchronicle-cli",
        "--bin",
        "pchronicle",
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


def _web_inputs_digest() -> str:
    """Hash the inputs that affect the generated Dioxus public directory."""
    digest = hashlib.sha256()
    inputs = [WEB_ROOT / "Cargo.toml", WEB_ROOT / "Dioxus.toml"]
    inputs.extend(sorted((WEB_ROOT / "src").rglob("*")))
    inputs.extend(sorted((WEB_ROOT / "assets").rglob("*")))
    for path in inputs:
        if not path.is_file():
            continue
        digest.update(str(path.relative_to(WEB_ROOT)).encode("utf-8"))
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def _web_assets_are_current(manifest: Path, digest: str) -> bool:
    if not manifest.is_file() or not (WEB_PUBLIC / "index.html").is_file():
        return False
    lines = manifest.read_text(encoding="utf-8").splitlines()
    return len(lines) >= 2 and lines[1] == digest


def _build_web_assets() -> None:
    """Build the target-independent Dioxus bundle when its inputs changed."""
    manifest = WEB_PUBLIC / "embedded.manifest"
    digest = _web_inputs_digest()
    if _web_assets_are_current(manifest, digest):
        print(f"Using current pChronicle Web assets: {WEB_PUBLIC}", file=sys.stderr)
        return
    if shutil.which("dx") is None and manifest.is_file():
        raise RuntimeError(
            "pChronicle Web assets are stale or incomplete and Dioxus CLI is unavailable"
        )
    command = ["dx", "bundle", "--release", "--debug-symbols", "false"]
    print(f"Building pChronicle Web assets: {shlex.join(command)}", file=sys.stderr)
    try:
        shutil.rmtree(WEB_PUBLIC.parent, ignore_errors=True)
        shutil.rmtree(DX_PUBLIC, ignore_errors=True)
        subprocess.run(command, cwd=WEB_ROOT, check=True)
    except FileNotFoundError as error:
        raise RuntimeError(
            "Dioxus CLI is required for wheel builds; install dioxus-cli 0.7.9"
        ) from error
    index = WEB_PUBLIC / "index.html"
    if not index.is_file():
        raise RuntimeError(f"Dioxus did not produce {index}")
    assets = WEB_PUBLIC / "assets"
    assets.mkdir(parents=True, exist_ok=True)
    for stylesheet in sorted((WEB_ROOT / "assets").glob("*.css")):
        shutil.copy2(stylesheet, assets / stylesheet.name)
    home_assets = WEB_ROOT / "assets" / "home"
    if home_assets.is_dir():
        destination = assets / "home"
        destination.mkdir(parents=True, exist_ok=True)
        for asset in sorted(home_assets.iterdir()):
            if asset.is_file():
                shutil.copy2(asset, destination / asset.name)
    manifest.write_text(
        f"__PCHRONICLE_EMBEDDED_WEB_ASSETS_V1__\n{digest}\n",
        encoding="utf-8",
    )


def stage_wheel_binaries(options: BuildOptions) -> Path:
    """Build pChronicle and atomically replace the wheel scripts directory."""
    _build_web_assets()
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
    parser.add_argument("--web-only", action="store_true")
    args = parser.parse_args()
    if args.web_only:
        _build_web_assets()
        return
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

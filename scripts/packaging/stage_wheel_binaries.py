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
import tarfile
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping

ROOT = Path(__file__).resolve().parents[2]
WHEEL_DATA = ROOT / "target" / "wheel-data"
WEB_ROOT = ROOT / "pchronicle-web"
WEB_PUBLIC = ROOT / "crates" / "persisting-pchronicle-cli" / "web-assets" / "public"
DX_PUBLIC = WEB_ROOT / "target" / "dx" / "pchronicle-web" / "release" / "web" / "public"
EXPECTED_BINARIES = ("pchronicle", "pvisor", "ppilot")
SUPPORTED_TARGETS = {
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
}
MACOS_ENTITLEMENTS = ROOT / "crates" / "persisting-pvisor" / "macos-hypervisor.entitlements"
LIBKRUNFW_VERSION = "5.5.0"
MACOS_DEPLOYMENT_TARGET = "11.0"
LIBKRUNFW_RELEASE = f"https://github.com/libkrun/libkrunfw/releases/download/v{LIBKRUNFW_VERSION}"
LIBKRUNFW_ARCHIVES = {
    "x86_64-unknown-linux-gnu": (
        "libkrunfw-x86_64.tgz",
        "c169206b01c89fbe134f1728bf4f988702bc7f73b4cf73e6fdece447d6fceca1",
        "lib64/libkrunfw.so.5.5.0",
    ),
    "aarch64-apple-darwin": (
        "libkrunfw-prebuilt-aarch64.tgz",
        "5bfae6efee63dbdf04a8fac2a69d772d9f900af2f54c4429b4acdfd6d86b9979",
        "libkrunfw/kernel.c",
    ),
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
    bundle_firmware: bool = True


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
        bundle_firmware=_bool_setting(config_settings, "bundle-firmware", default=not editable),
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
        "-p",
        "persisting-pvisor",
        "--bin",
        "pvisor",
        "-p",
        "persisting-ppilot",
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


def _is_macos(options: BuildOptions) -> bool:
    return options.target == "aarch64-apple-darwin" or (
        options.target is None and sys.platform == "darwin"
    )


def _firmware_source(options: BuildOptions) -> tuple[Path, str]:
    name = "libkrunfw.5.dylib" if _is_macos(options) else "libkrunfw.so.5"
    configured = os.getenv("PERSISTING_LIBKRUNFW_PATH")
    if configured:
        source = Path(configured).expanduser()
        if source.is_dir():
            source = source / name
        source = source.resolve()
        if not source.is_file():
            raise RuntimeError(f"libkrunfw payload does not exist: {source}")
        return source, name
    return _fetch_firmware(options, name), name


def _host_target() -> str:
    if sys.platform == "darwin" and platform.machine().lower() in {"arm64", "aarch64"}:
        return "aarch64-apple-darwin"
    if sys.platform == "linux" and platform.machine().lower() in {"x86_64", "amd64"}:
        return "x86_64-unknown-linux-gnu"
    raise RuntimeError(
        f"automatic libkrunfw preparation is unsupported on {sys.platform}/{platform.machine()}"
    )


def _fetch_firmware(options: BuildOptions, name: str) -> Path:
    target = options.target or _host_target()
    try:
        archive_name, expected_sha256, archive_member = LIBKRUNFW_ARCHIVES[target]
    except KeyError as error:
        raise RuntimeError(f"no downloadable libkrunfw payload for {target}") from error
    cache_key = f"{LIBKRUNFW_VERSION}-{target}"
    if _is_macos(options):
        cache_key += f"-macos{MACOS_DEPLOYMENT_TARGET}"
    build_root = ROOT / "target" / "libkrunfw" / cache_key
    destination = build_root / name
    if destination.is_file():
        return destination
    archive = build_root.parent / archive_name
    build_root.parent.mkdir(parents=True, exist_ok=True)
    if not archive.is_file() or hashlib.sha256(archive.read_bytes()).hexdigest() != expected_sha256:
        archive.unlink(missing_ok=True)
        print(f"Downloading wheel firmware: {LIBKRUNFW_RELEASE}/{archive_name}", file=sys.stderr)
        urllib.request.urlretrieve(f"{LIBKRUNFW_RELEASE}/{archive_name}", archive)
    actual_sha256 = hashlib.sha256(archive.read_bytes()).hexdigest()
    if actual_sha256 != expected_sha256:
        archive.unlink(missing_ok=True)
        raise RuntimeError(
            f"libkrunfw checksum mismatch: expected {expected_sha256}, got {actual_sha256}"
        )
    build_root.mkdir(parents=True, exist_ok=True)
    source_path = build_root / "kernel.c"
    with tarfile.open(archive, "r:gz") as source:
        try:
            member = source.getmember(archive_member)
        except KeyError as error:
            raise RuntimeError(f"libkrunfw archive is missing {archive_member}") from error
        if not member.isfile():
            raise RuntimeError(f"libkrunfw archive member is not a file: {archive_member}")
        payload = source.extractfile(member)
        if payload is None:
            raise RuntimeError(f"could not read libkrunfw archive member: {archive_member}")
        extracted = source_path if _is_macos(options) else destination
        with extracted.open("wb") as output:
            shutil.copyfileobj(payload, output)

    if _is_macos(options):
        subprocess.run(
            [
                "/usr/bin/cc",
                "-fPIC",
                "-DABI_VERSION=5",
                f"-mmacosx-version-min={MACOS_DEPLOYMENT_TARGET}",
                "-shared",
                "-Wl,-install_name,@rpath/libkrunfw.5.dylib",
                "-o",
                str(destination),
                str(source_path),
            ],
            check=True,
        )
        source_path.unlink(missing_ok=True)
    destination.chmod(0o755)
    return destination


def _sign_macos_pvisor(path: Path) -> None:
    subprocess.run(
        [
            "codesign",
            "--force",
            "--sign",
            "-",
            "--entitlements",
            str(MACOS_ENTITLEMENTS),
            str(path),
        ],
        check=True,
    )


def _build_web_assets() -> None:
    """Build the target-independent Dioxus bundle before compiling native CLIs."""
    manifest = WEB_PUBLIC / "embedded.manifest"
    if shutil.which("dx") is None and manifest.is_file():
        print(f"Using prebuilt pChronicle Web assets: {WEB_PUBLIC}", file=sys.stderr)
        return
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
    manifest.write_text("__PCHRONICLE_EMBEDDED_WEB_ASSETS_V1__\n", encoding="utf-8")


def stage_wheel_binaries(options: BuildOptions) -> Path:
    """Build all host CLIs and atomically replace the wheel scripts directory."""
    firmware = _firmware_source(options) if options.bundle_firmware else None
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

        if firmware is not None:
            firmware_source, firmware_name = firmware
            firmware_destination = staged / firmware_name
            shutil.copy2(firmware_source, firmware_destination)
            (staged / "libkrunfw.SOURCE").write_text(
                f"libkrunfw {LIBKRUNFW_VERSION}\n"
                f"source: {LIBKRUNFW_RELEASE}/libkrunfw-<architecture>.tgz\n"
                "licenses: GPL-2.0-only (Linux kernel), LGPL-2.1-only (library)\n",
                encoding="utf-8",
            )
            print(
                f"Staged libkrunfw: {firmware_source} -> {firmware_destination}",
                file=sys.stderr,
            )
        if _is_macos(options):
            _sign_macos_pvisor(staged / "pvisor")

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

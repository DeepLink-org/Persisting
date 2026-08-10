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
from typing import Any, Mapping, Sequence

ROOT = Path(__file__).resolve().parents[2]
WHEEL_DATA = ROOT / "target" / "wheel-data"
WEB_ROOT = ROOT / "pchronicle-web"
WEB_PUBLIC = ROOT / "crates" / "persisting-pchronicle-server" / "web-assets" / "public"
DX_PUBLIC = WEB_ROOT / "target" / "dx" / "pchronicle-web" / "release" / "web" / "public"
EXPECTED_BINARIES = ("persisting", "pvisor", "ppilot")
SUPPORTED_TARGETS = {
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
}
MACOS_ENTITLEMENTS = ROOT / "crates" / "persisting-pvisor" / "macos-hypervisor.entitlements"
LIBKRUNFW_VERSION = "5.5.0"
LIBKRUNFW_RELEASE = f"https://github.com/libkrun/libkrunfw/releases/download/v{LIBKRUNFW_VERSION}"
LIBKRUNFW_ARCHIVES = {
    "x86_64-unknown-linux-gnu": (
        "libkrunfw-x86_64.tgz",
        "c169206b01c89fbe134f1728bf4f988702bc7f73b4cf73e6fdece447d6fceca1",
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
        bundle_firmware=not editable,
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


def _is_macos(options: BuildOptions) -> bool:
    return options.target == "aarch64-apple-darwin" or (
        options.target is None and sys.platform == "darwin"
    )


def _firmware_source(options: BuildOptions) -> tuple[Path, str]:
    name = "libkrunfw.5.dylib" if _is_macos(options) else "libkrunfw.so.5"
    configured = os.getenv("PERSISTING_LIBKRUNFW_PATH")
    if not configured and os.getenv("PERSISTING_FETCH_LIBKRUNFW") == "1":
        return _fetch_linux_firmware(options), name
    if not configured:
        raise RuntimeError(
            "PERSISTING_LIBKRUNFW_PATH must point to libkrunfw when building a wheel; "
            "official Linux builds may set PERSISTING_FETCH_LIBKRUNFW=1"
        )
    source = Path(configured).expanduser()
    if source.is_dir():
        source = source / name
    source = source.resolve()
    if not source.is_file():
        raise RuntimeError(f"libkrunfw payload does not exist: {source}")
    return source, name


def _fetch_linux_firmware(options: BuildOptions) -> Path:
    target = options.target or "x86_64-unknown-linux-gnu"
    try:
        archive_name, expected_sha256 = LIBKRUNFW_ARCHIVES[target]
    except KeyError as error:
        raise RuntimeError(f"no downloadable libkrunfw payload for {target}") from error
    build_root = ROOT / "target" / "libkrunfw" / f"{LIBKRUNFW_VERSION}-{target}"
    cached = sorted(build_root.rglob(f"libkrunfw.so.{LIBKRUNFW_VERSION}"))
    if len(cached) == 1:
        return cached[0]
    archive = build_root.parent / archive_name
    build_root.parent.mkdir(parents=True, exist_ok=True)
    if not archive.is_file() or hashlib.sha256(archive.read_bytes()).hexdigest() != expected_sha256:
        archive.unlink(missing_ok=True)
        urllib.request.urlretrieve(f"{LIBKRUNFW_RELEASE}/{archive_name}", archive)
    actual_sha256 = hashlib.sha256(archive.read_bytes()).hexdigest()
    if actual_sha256 != expected_sha256:
        archive.unlink(missing_ok=True)
        raise RuntimeError(
            f"libkrunfw checksum mismatch: expected {expected_sha256}, got {actual_sha256}"
        )
    shutil.rmtree(build_root, ignore_errors=True)
    build_root.mkdir()
    with tarfile.open(archive, "r:gz") as source:
        source.extractall(build_root, filter="data")
    makefiles = sorted(build_root.rglob("Makefile"))
    if len(makefiles) != 1:
        raise RuntimeError(f"expected one libkrunfw Makefile, found {len(makefiles)}")
    subprocess.run(["make", "-j2"], cwd=makefiles[0].parent, check=True)
    matches = sorted(build_root.rglob(f"libkrunfw.so.{LIBKRUNFW_VERSION}"))
    if len(matches) != 1:
        raise RuntimeError(f"expected one built libkrunfw payload, found {len(matches)}")
    return matches[0]


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
    manifest.write_text(
        "__PCHRONICLE_EMBEDDED_WEB_ASSETS_V1__\n", encoding="utf-8"
    )


def stage_wheel_binaries(options: BuildOptions) -> Path:
    """Build all host CLIs and atomically replace the wheel scripts directory."""
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

        if options.bundle_firmware:
            firmware_source, firmware_name = _firmware_source(options)
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

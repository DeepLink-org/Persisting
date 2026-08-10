#!/usr/bin/env python3
"""Verify that a Persisting wheel installs its complete native CLI set."""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
import venv
import zipfile
from email.parser import BytesParser
from pathlib import Path

EXPECTED_BINARIES = ("persisting", "pvisor", "ppilot")
FIRMWARE_NAMES = ("libkrunfw.so.5", "libkrunfw.5.dylib")


def _wheel_contents(
    wheel: Path,
) -> tuple[str, dict[str, zipfile.ZipInfo], zipfile.ZipInfo]:
    with zipfile.ZipFile(wheel) as archive:
        metadata_names = [
            name for name in archive.namelist() if name.endswith(".dist-info/METADATA")
        ]
        if len(metadata_names) != 1:
            raise RuntimeError(f"expected one METADATA file, found {metadata_names}")
        metadata = BytesParser().parsebytes(archive.read(metadata_names[0]))
        version = metadata.get("Version")
        if not version:
            raise RuntimeError("wheel METADATA has no Version")

        scripts: dict[str, zipfile.ZipInfo] = {}
        for name in EXPECTED_BINARIES:
            matches = [
                info
                for info in archive.infolist()
                if ".data/scripts/" in info.filename and info.filename.endswith(f"/scripts/{name}")
            ]
            if len(matches) != 1:
                raise RuntimeError(f"expected one wheel script {name!r}, found {len(matches)}")
            mode = matches[0].external_attr >> 16
            if mode & 0o111 == 0:
                raise RuntimeError(f"wheel script {name!r} is not executable (mode {mode:o})")
            scripts[name] = matches[0]
        firmware = [
            info
            for info in archive.infolist()
            if ".data/scripts/" in info.filename
            and info.filename.rsplit("/", 1)[-1] in FIRMWARE_NAMES
        ]
        if len(firmware) != 1:
            raise RuntimeError(f"expected one libkrunfw payload, found {len(firmware)}")
        return version, scripts, firmware[0]


def _run(command: list[str], *, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        command,
        check=True,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=60,
    )
    return result.stdout


def _installed_script_dir(environment: Path) -> Path:
    return environment / ("Scripts" if os.name == "nt" else "bin")


def verify_native_payloads(
    wheel: Path,
    scripts: dict[str, zipfile.ZipInfo],
    firmware: zipfile.ZipInfo,
) -> None:
    wheel_name = wheel.name.lower()
    expected_arches: tuple[str, ...]
    if "arm64" in wheel_name or "aarch64" in wheel_name:
        expected_arches = ("arm64", "aarch64")
    elif "x86_64" in wheel_name:
        expected_arches = ("x86_64", "x86-64")
    else:
        raise RuntimeError(f"cannot determine native architecture from wheel name: {wheel.name}")

    with tempfile.TemporaryDirectory(prefix="persisting-wheel-native-") as temporary:
        root = Path(temporary)
        with zipfile.ZipFile(wheel) as archive:
            firmware_path = root / Path(firmware.filename).name
            firmware_path.write_bytes(archive.read(firmware))
            firmware_description = _run(["file", str(firmware_path)]).lower()
            if not any(arch in firmware_description for arch in expected_arches):
                raise RuntimeError(
                    f"libkrunfw architecture does not match {wheel.name}: "
                    f"{firmware_description.strip()}"
                )
            for name, info in scripts.items():
                executable = root / name
                executable.write_bytes(archive.read(info))
                executable.chmod(0o755)
                description = _run(["file", str(executable)]).lower()
                if not any(arch in description for arch in expected_arches):
                    raise RuntimeError(
                        f"{name} architecture does not match {wheel.name}: {description.strip()}"
                    )

                if sys.platform == "darwin" and "macosx" in wheel_name:
                    dependencies = _run(["otool", "-L", str(executable)])
                    for line in dependencies.splitlines()[1:]:
                        dependency = line.strip().split(" ", 1)[0]
                        if not dependency.startswith(("/usr/lib/", "/System/Library/")):
                            raise RuntimeError(f"{name} has non-system dependency {dependency!r}")
                    if name == "pvisor":
                        entitlements = _run(["codesign", "-d", "--entitlements", ":-", str(executable)])
                        if "com.apple.security.hypervisor" not in entitlements:
                            raise RuntimeError("pvisor is missing the Hypervisor.framework entitlement")
                elif sys.platform == "linux" and "linux" in wheel_name:
                    dependencies = _run(["ldd", str(executable)])
                    if "not found" in dependencies:
                        raise RuntimeError(f"{name} has unresolved dependencies:\n{dependencies}")
                    if name == "pvisor" and "libkrun" in dependencies:
                        raise RuntimeError("pvisor dynamically links libkrun")


def install_smoke(wheel: Path, version: str) -> None:
    with tempfile.TemporaryDirectory(prefix="persisting-wheel-") as temporary:
        environment = Path(temporary) / "venv"
        venv.EnvBuilder(with_pip=True).create(environment)
        scripts = _installed_script_dir(environment)
        python = scripts / ("python.exe" if os.name == "nt" else "python")
        _run([str(python), "-m", "pip", "install", "--no-deps", str(wheel)])

        env = os.environ.copy()
        env.pop("PERSISTING_PVISOR_BIN", None)
        env.pop("PERSISTING_PPILOT_BIN", None)
        env["PATH"] = os.pathsep.join((str(scripts), env.get("PATH", "")))
        for name in EXPECTED_BINARIES:
            executable = scripts / name
            if not executable.is_file() or not os.access(executable, os.X_OK):
                raise RuntimeError(
                    f"installed executable is missing or not executable: {executable}"
                )
            output = _run([str(executable), "--version"], env=env)
            if version not in output:
                raise RuntimeError(
                    f"{name} version output {output.strip()!r} does not contain wheel version {version!r}"
                )
            _run([str(executable), "--help"], env=env)

        _run([str(scripts / "persisting"), "execute", "--help"], env=env)
        _run([str(scripts / "persisting"), "query", "--help"], env=env)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("wheel", type=Path)
    parser.add_argument("--install-smoke", action="store_true")
    args = parser.parse_args()
    wheel = args.wheel.resolve()
    if not wheel.is_file():
        raise SystemExit(f"wheel does not exist: {wheel}")

    version, scripts, firmware = _wheel_contents(wheel)
    verify_native_payloads(wheel, scripts, firmware)
    print(f"wheel={wheel.name} version={version} scripts={','.join(sorted(scripts))} static=PASS")
    if args.install_smoke:
        install_smoke(wheel, version)
        print("install=PASS versions=PASS dispatch=PASS")


if __name__ == "__main__":
    main()

"""Setuptools configuration for platform wheels containing native CLI scripts."""

from __future__ import annotations

import os
from pathlib import Path

from setuptools import Command, setup
from setuptools.command.bdist_wheel import bdist_wheel

ROOT = Path(__file__).resolve().parent
WHEEL_SCRIPTS = ROOT / "target" / "wheel-data" / "scripts"


class PlatformWheel(bdist_wheel):
    """Mark the Python-universal wheel as platform-specific because it bundles native CLIs."""

    def finalize_options(self) -> None:
        super().finalize_options()
        self.root_is_pure = False

    def get_tag(self) -> tuple[str, str, str]:
        _python, _abi, platform = super().get_tag()
        return "py3", "none", platform


class BinaryScripts(Command):
    """Copy native executables without treating them as encoded text files."""

    user_options = [
        ("build-dir=", "d", "directory to copy scripts to"),
        ("force", "f", "forcibly copy all scripts"),
        ("executable=", "e", "unused interpreter path compatibility option"),
    ]
    boolean_options = ["force"]

    def initialize_options(self) -> None:
        self.build_dir = None
        self.scripts = None
        self.force = None
        self.executable = None

    def finalize_options(self) -> None:
        self.set_undefined_options(
            "build",
            ("build_scripts", "build_dir"),
            ("force", "force"),
            ("executable", "executable"),
        )
        self.scripts = self.distribution.scripts

    def get_source_files(self) -> list[str]:
        return list(self.scripts or [])

    def run(self) -> None:
        if not self.scripts:
            return
        self.mkpath(self.build_dir)
        for script in self.scripts:
            destination = str(Path(self.build_dir) / Path(script).name)
            self.copy_file(script, destination)
            if os.name == "posix":
                mode = Path(destination).stat().st_mode
                Path(destination).chmod(mode | 0o555)


def wheel_scripts() -> list[str]:
    if os.getenv("PERSISTING_SETUP_SKIP_NATIVE_SCRIPTS") == "1":
        return []
    if not WHEEL_SCRIPTS.is_dir():
        return []
    return [
        path.relative_to(ROOT).as_posix()
        for path in sorted(WHEEL_SCRIPTS.iterdir())
        if path.is_file()
    ]


setup(
    cmdclass={"bdist_wheel": PlatformWheel, "build_scripts": BinaryScripts},
    scripts=wheel_scripts(),
)

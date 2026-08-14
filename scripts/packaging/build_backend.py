"""PEP 517 backend that adds Persisting's native CLIs to setuptools wheels."""

from __future__ import annotations

import os
from typing import Any, Mapping

from setuptools import build_meta as setuptools_backend
from stage_wheel_binaries import (
    options_from_build_backend,
    stage_wheel_binaries,
)


def _stage(config_settings: Mapping[str, Any] | None, *, editable: bool) -> None:
    stage_wheel_binaries(options_from_build_backend(config_settings, editable=editable))


def _without_native_scripts(hook, *args):
    previous = os.environ.get("PERSISTING_SETUP_SKIP_NATIVE_SCRIPTS")
    os.environ["PERSISTING_SETUP_SKIP_NATIVE_SCRIPTS"] = "1"
    try:
        return hook(*args)
    finally:
        if previous is None:
            os.environ.pop("PERSISTING_SETUP_SKIP_NATIVE_SCRIPTS", None)
        else:
            os.environ["PERSISTING_SETUP_SKIP_NATIVE_SCRIPTS"] = previous


def build_wheel(
    wheel_directory: str,
    config_settings: Mapping[str, Any] | None = None,
    metadata_directory: str | None = None,
) -> str:
    _stage(config_settings, editable=False)
    return setuptools_backend.build_wheel(wheel_directory, config_settings, metadata_directory)


def build_editable(
    wheel_directory: str,
    config_settings: Mapping[str, Any] | None = None,
    metadata_directory: str | None = None,
) -> str:
    return _without_native_scripts(
        setuptools_backend.build_editable,
        wheel_directory,
        config_settings,
        metadata_directory,
    )


def build_sdist(
    sdist_directory: str,
    config_settings: Mapping[str, Any] | None = None,
) -> str:
    return _without_native_scripts(setuptools_backend.build_sdist, sdist_directory, config_settings)


def prepare_metadata_for_build_wheel(
    metadata_directory: str,
    config_settings: Mapping[str, Any] | None = None,
) -> str:
    return _without_native_scripts(
        setuptools_backend.prepare_metadata_for_build_wheel,
        metadata_directory,
        config_settings,
    )


prepare_metadata_for_build_editable = prepare_metadata_for_build_wheel
get_requires_for_build_wheel = setuptools_backend.get_requires_for_build_wheel
get_requires_for_build_editable = setuptools_backend.get_requires_for_build_editable
get_requires_for_build_sdist = setuptools_backend.get_requires_for_build_sdist

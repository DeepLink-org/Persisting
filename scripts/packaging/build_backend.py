"""PEP 517 backend that adds Persisting's native CLIs to Maturin wheels."""

from __future__ import annotations

from typing import Any, Mapping

import maturin
from stage_wheel_binaries import (
    ensure_wheel_data_directory,
    options_from_maturin,
    stage_wheel_binaries,
)


def _stage(config_settings: Mapping[str, Any] | None, *, editable: bool) -> None:
    stage_wheel_binaries(options_from_maturin(config_settings, editable=editable))


def build_wheel(
    wheel_directory: str,
    config_settings: Mapping[str, Any] | None = None,
    metadata_directory: str | None = None,
) -> str:
    _stage(config_settings, editable=False)
    return maturin.build_wheel(wheel_directory, config_settings, metadata_directory)


def build_editable(
    wheel_directory: str,
    config_settings: Mapping[str, Any] | None = None,
    metadata_directory: str | None = None,
) -> str:
    _stage(config_settings, editable=True)
    return maturin.build_editable(wheel_directory, config_settings, metadata_directory)


def build_sdist(
    sdist_directory: str,
    config_settings: Mapping[str, Any] | None = None,
) -> str:
    ensure_wheel_data_directory()
    return maturin.build_sdist(sdist_directory, config_settings)


def prepare_metadata_for_build_wheel(
    metadata_directory: str,
    config_settings: Mapping[str, Any] | None = None,
) -> str:
    ensure_wheel_data_directory()
    return maturin.prepare_metadata_for_build_wheel(metadata_directory, config_settings)


prepare_metadata_for_build_editable = prepare_metadata_for_build_wheel
get_requires_for_build_wheel = maturin.get_requires_for_build_wheel
get_requires_for_build_editable = maturin.get_requires_for_build_editable
get_requires_for_build_sdist = maturin.get_requires_for_build_sdist

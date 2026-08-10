from __future__ import annotations

import importlib.util
import zipfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]


def _load_script(name: str):
    path = ROOT / "scripts" / "ci" / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release_version = _load_script("check_release_version")
release_artifacts = _load_script("check_release_artifacts")


@pytest.mark.parametrize("workflow", ["nightly.yml", "release.yml"])
def test_manylinux_tag_is_configured_only_by_maturin_action(workflow: str) -> None:
    contents = (ROOT / ".github" / "workflows" / workflow).read_text(encoding="utf-8")

    assert "manylinux: 2014" in contents
    assert "--compatibility manylinux2014" not in contents
    assert "--manylinux 2014" not in contents


def _write_version_tree(root: Path, *, pyproject: str, cargo: str, package: str) -> None:
    (root / "persisting").mkdir()
    (root / "pyproject.toml").write_text(
        f'[project]\nname = "persisting"\nversion = "{pyproject}"\n', encoding="utf-8"
    )
    (root / "Cargo.toml").write_text(
        f'[workspace.package]\nversion = "{cargo}"\n', encoding="utf-8"
    )
    (root / "persisting" / "__init__.py").write_text(
        f'__version__ = "{package}"\n', encoding="utf-8"
    )


def _write_wheel(path: Path, version: str) -> None:
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr(
            f"persisting-{version}.dist-info/METADATA",
            f"Metadata-Version: 2.1\nName: persisting\nVersion: {version}\n",
        )


def test_release_version_accepts_matching_stable_tag(tmp_path: Path) -> None:
    _write_version_tree(tmp_path, pyproject="1.2.3", cargo="1.2.3", package="1.2.3")
    assert release_version.validate_versions("v1.2.3", tmp_path) == "1.2.3"


@pytest.mark.parametrize("tag", ["1.2.3", "v1.2", "v1.2.3rc1", "v01.2.3"])
def test_release_version_rejects_non_stable_tags(tmp_path: Path, tag: str) -> None:
    _write_version_tree(tmp_path, pyproject="1.2.3", cargo="1.2.3", package="1.2.3")
    with pytest.raises(release_version.ReleaseValidationError):
        release_version.validate_versions(tag, tmp_path)


def test_release_version_rejects_mismatched_sources(tmp_path: Path) -> None:
    _write_version_tree(tmp_path, pyproject="1.2.3", cargo="1.2.4", package="1.2.3")
    with pytest.raises(release_version.ReleaseValidationError, match="do not match"):
        release_version.validate_versions("v1.2.3", tmp_path)


def test_release_version_rejects_tag_version_mismatch(tmp_path: Path) -> None:
    _write_version_tree(tmp_path, pyproject="1.2.3", cargo="1.2.3", package="1.2.3")
    with pytest.raises(release_version.ReleaseValidationError, match="does not match"):
        release_version.validate_versions("v1.2.4", tmp_path)


def test_release_artifacts_accept_supported_matrix(tmp_path: Path) -> None:
    version = "1.2.3"
    names = [
        f"persisting-{version}-cp310-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl",
        f"persisting-{version}-cp310-abi3-macosx_11_0_arm64.whl",
    ]
    for name in names:
        _write_wheel(tmp_path / name, version)

    found = release_artifacts.validate_artifacts(tmp_path, version)
    assert set(found) == {"linux-x86_64", "macos-arm64"}


def test_release_artifacts_reject_missing_platform(tmp_path: Path) -> None:
    version = "1.2.3"
    _write_wheel(
        tmp_path / f"persisting-{version}-cp310-abi3-macosx_11_0_arm64.whl",
        version,
    )
    with pytest.raises(release_artifacts.ArtifactValidationError, match="expected 2 wheels"):
        release_artifacts.validate_artifacts(tmp_path, version)


def test_release_artifacts_reject_metadata_version_mismatch(tmp_path: Path) -> None:
    filename_version = "1.2.3"
    names = [
        f"persisting-{filename_version}-cp310-abi3-manylinux2014_x86_64.whl",
        f"persisting-{filename_version}-cp310-abi3-macosx_11_0_arm64.whl",
    ]
    for name in names:
        _write_wheel(tmp_path / name, "1.2.4")

    with pytest.raises(release_artifacts.ArtifactValidationError, match="METADATA version"):
        release_artifacts.validate_artifacts(tmp_path, filename_version)


def test_release_artifacts_reject_oversized_wheel(tmp_path: Path) -> None:
    version = "1.2.3"
    names = [
        f"persisting-{version}-cp310-abi3-manylinux2014_x86_64.whl",
        f"persisting-{version}-cp310-abi3-macosx_11_0_arm64.whl",
    ]
    for name in names:
        _write_wheel(tmp_path / name, version)

    with pytest.raises(release_artifacts.ArtifactValidationError, match="exceeds"):
        release_artifacts.validate_artifacts(tmp_path, version, max_bytes=1)

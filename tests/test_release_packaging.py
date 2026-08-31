from __future__ import annotations

import importlib.util
import sys
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


def _load_wheel_stage():
    name = "stage_wheel_binaries_test"
    path = ROOT / "scripts" / "packaging" / "stage_wheel_binaries.py"
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


wheel_stage = _load_wheel_stage()


def _load_wheel_verify():
    name = "verify_wheel_test"
    path = ROOT / "scripts" / "packaging" / "verify_wheel.py"
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


wheel_verify = _load_wheel_verify()


def test_python_wheel_uses_setuptools_and_platform_builds() -> None:
    contents = (ROOT / "pyproject.toml").read_text(encoding="utf-8")

    assert 'requires = ["setuptools>=77"]' in contents
    assert 'build-backend = "build_backend"' in contents
    assert 'build = "cp312-*"' in contents
    assert 'manylinux-x86_64-image = "manylinux_2_28"' in contents
    assert 'archs = ["arm64"]' in contents
    assert "PERSISTING_CARGO_ZIGBUILD" not in contents
    assert "cargo-zigbuild" not in contents


@pytest.mark.parametrize("workflow", ["nightly.yml", "release.yml"])
def test_platform_wheels_use_cibuildwheel(workflow: str) -> None:
    contents = (ROOT / ".github" / "workflows" / workflow).read_text(encoding="utf-8")

    assert "pypa/cibuildwheel@v4.1.0" in contents


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
        f"persisting-{version}-py3-none-manylinux_2_28_x86_64.whl",
        f"persisting-{version}-py3-none-macosx_11_0_arm64.whl",
    ]
    for name in names:
        _write_wheel(tmp_path / name, version)

    found = release_artifacts.validate_artifacts(tmp_path, version)
    assert set(found) == {"linux-x86_64", "macos-arm64"}


def test_release_artifacts_reject_missing_platform(tmp_path: Path) -> None:
    version = "1.2.3"
    _write_wheel(
        tmp_path / f"persisting-{version}-py3-none-macosx_11_0_arm64.whl",
        version,
    )
    with pytest.raises(release_artifacts.ArtifactValidationError, match="expected 2 wheels"):
        release_artifacts.validate_artifacts(tmp_path, version)


def test_release_artifacts_reject_metadata_version_mismatch(tmp_path: Path) -> None:
    filename_version = "1.2.3"
    names = [
        f"persisting-{filename_version}-py3-none-manylinux_2_28_x86_64.whl",
        f"persisting-{filename_version}-py3-none-macosx_11_0_arm64.whl",
    ]
    for name in names:
        _write_wheel(tmp_path / name, "1.2.4")

    with pytest.raises(release_artifacts.ArtifactValidationError, match="METADATA version"):
        release_artifacts.validate_artifacts(tmp_path, filename_version)


def test_release_artifacts_reject_oversized_wheel(tmp_path: Path) -> None:
    version = "1.2.3"
    names = [
        f"persisting-{version}-py3-none-manylinux_2_28_x86_64.whl",
        f"persisting-{version}-py3-none-macosx_11_0_arm64.whl",
    ]
    for name in names:
        _write_wheel(tmp_path / name, version)

    with pytest.raises(release_artifacts.ArtifactValidationError, match="exceeds"):
        release_artifacts.validate_artifacts(tmp_path, version, max_bytes=1)


@pytest.mark.parametrize(
    ("editable", "bundle_firmware"),
    [(True, False), (False, True)],
)
def test_build_backend_options_only_skip_firmware_for_editable_builds(
    editable: bool,
    bundle_firmware: bool,
) -> None:
    options = wheel_stage.options_from_build_backend(None, editable=editable)

    assert options.bundle_firmware is bundle_firmware


def test_build_backend_options_accept_explicit_cargo_settings() -> None:
    options = wheel_stage.options_from_build_backend(
        {
            "cargo-profile": "dev",
            "cargo-locked": "false",
            "cargo-jobs": "3",
            "bundle-firmware": "false",
        },
        editable=False,
    )

    assert options.profile == "dev"
    assert options.locked is False
    assert options.jobs == "3"
    assert options.bundle_firmware is False


def test_editable_staging_does_not_resolve_firmware(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    artifacts = {}
    for name in wheel_stage.EXPECTED_BINARIES:
        artifact = tmp_path / "artifacts" / name
        artifact.parent.mkdir(exist_ok=True)
        artifact.write_text(name, encoding="utf-8")
        artifacts[name] = artifact

    monkeypatch.setattr(wheel_stage, "WHEEL_DATA", tmp_path / "wheel-data")
    monkeypatch.setattr(wheel_stage, "_build_web_assets", lambda: None)
    monkeypatch.setattr(wheel_stage, "_build", lambda _options: artifacts)
    monkeypatch.setattr(wheel_stage, "_is_macos", lambda _options: False)

    def unexpected_firmware(_options):
        raise AssertionError("editable staging must not resolve wheel firmware")

    monkeypatch.setattr(wheel_stage, "_firmware_source", unexpected_firmware)

    scripts = wheel_stage.stage_wheel_binaries(wheel_stage.BuildOptions(bundle_firmware=False))

    assert {path.name for path in scripts.iterdir()} == set(wheel_stage.EXPECTED_BINARIES)


def test_release_staging_resolves_firmware_before_build(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    events: list[str] = []

    def missing_firmware(_options):
        events.append("firmware")
        raise RuntimeError("missing firmware")

    def unexpected_build(_options):
        events.append("build")
        raise AssertionError("Cargo must not run before firmware is ready")

    monkeypatch.setattr(wheel_stage, "_firmware_source", missing_firmware)
    monkeypatch.setattr(wheel_stage, "_build", unexpected_build)

    with pytest.raises(RuntimeError, match="missing firmware"):
        wheel_stage.stage_wheel_binaries(wheel_stage.BuildOptions())

    assert events == ["firmware"]


def test_firmware_source_prefers_explicit_path(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    firmware = tmp_path / "libkrunfw.5.dylib"
    firmware.write_bytes(b"firmware")
    monkeypatch.setenv("PERSISTING_LIBKRUNFW_PATH", str(tmp_path))

    source, name = wheel_stage._firmware_source(
        wheel_stage.BuildOptions(target="aarch64-apple-darwin")
    )

    assert source == firmware.resolve()
    assert name == firmware.name


def test_firmware_source_fetches_when_path_is_not_configured(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    firmware = tmp_path / "libkrunfw.5.dylib"
    firmware.write_bytes(b"firmware")
    monkeypatch.delenv("PERSISTING_LIBKRUNFW_PATH", raising=False)
    monkeypatch.setattr(wheel_stage, "_fetch_firmware", lambda _options, _name: firmware)

    source, name = wheel_stage._firmware_source(
        wheel_stage.BuildOptions(target="aarch64-apple-darwin")
    )

    assert source == firmware
    assert name == firmware.name


def test_cargo_command_uses_plain_build_by_default() -> None:
    command = wheel_stage._cargo_command(wheel_stage.BuildOptions())

    assert command[:2] == ["cargo", "build"]
    assert "--target" not in command


def test_manylinux_glibc_requirement_accepts_2_28() -> None:
    symbols = """
    0000000000000000  0 FUNC    GLOBAL DEFAULT  UND memcpy@GLIBC_2.2.5
    0000000000000000  0 FUNC    GLOBAL DEFAULT  UND copy_file_range@GLIBC_2.27
    0000000000000000  0 FUNC    GLOBAL DEFAULT  UND statx@GLIBC_2.28
    """
    assert wheel_verify.glibc_requirement(symbols) == (2, 28, 0)
    assert wheel_verify.glibc_requirement(symbols) <= wheel_verify.MANYLINUX_MAX_GLIBC


def test_manylinux_glibc_requirement_detects_newer_than_2_28() -> None:
    symbols = """
    0000000000000000  0 FUNC    GLOBAL DEFAULT  UND statx@GLIBC_2.28
    0000000000000000  0 FUNC    GLOBAL DEFAULT  UND fchmodat2@GLIBC_2.38
    """
    assert wheel_verify.glibc_requirement(symbols) == (2, 38, 0)
    assert wheel_verify.glibc_requirement(symbols) > wheel_verify.MANYLINUX_MAX_GLIBC

# Releasing `persisting`

Stable releases are built by GitHub Actions from version tags and published to
PyPI with Trusted Publishing. The project still ships as a Python wheel, but it
does not contain a PyO3 extension and does not use Maturin.

Each platform wheel is tagged `py3-none-<platform>` and contains:

- the Python `persisting` package;
- native `pvisor`, `ppilot`, and `pchronicle` scripts;
- the bundled pChronicle Web assets;
- the platform libkrun firmware payload required by pVisor.

The release set currently contains Linux x86_64 and Apple Silicon macOS wheels.
Source distributions are not part of the published artifact set.

## One-time setup

1. Create a GitHub environment named `pypi`. Do not require reviewers; restrict
   deployments to tags matching `v*`.
2. In the PyPI publishing settings, add a pending Trusted Publisher:
   - PyPI project: `persisting`
   - GitHub owner: `DeepLink-org`
   - Repository: `Persisting`
   - Workflow: `release.yml`
   - Environment: `pypi`

No PyPI API token is stored in GitHub. The pending publisher can create the
project during the first successful upload but does not reserve the name.

## Prepare a release

1. Update the same `X.Y.Z` version in `pyproject.toml`, the workspace package
   section of `Cargo.toml`, and `persisting/__init__.py`.
2. Refresh local workspace versions in the lockfile without upgrading
   dependencies:

   ```bash
   cargo metadata --format-version 1 --no-deps >/dev/null
   ```

3. Commit and merge the version change to `main`. The workflow refuses a tag
   whose commit is not reachable from `main`.
4. Optionally run **Publish PyPI** manually. A manual run builds and verifies
   all wheels but does not publish them.
5. Create and push the matching stable tag:

   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

## Build and verification path

The PEP 517 backend is setuptools with the repository-owned
`scripts/packaging/build_backend.py`. Before wheel assembly it builds the three
Rust CLIs, stages firmware, and ensures the Dioxus bundle exists. `setup.py`
marks the wheel platform-specific while keeping the Python and ABI tags
`py3-none`.

Linux CI may fetch the pinned libkrun firmware archive through the packaging
script. macOS CI installs libkrunfw explicitly and passes
`PERSISTING_LIBKRUNFW_PATH`. Local wheel builds must use one of the supported
firmware paths; a missing payload is a build error rather than an incomplete
wheel.

Linux wheels stay on the manylinux2014 / glibc 2.17 tag. cibuildwheel still
runs inside that image, but the three CLIs are linked with
`cargo zigbuild --target x86_64-unknown-linux-gnu.2.17` so rustc's libstd does
not pull glibc 2.27+ symbols. Local `python -m build` on Linux keeps using
plain `cargo build` unless `PERSISTING_CARGO_ZIGBUILD=1`.

Every wheel is checked for its component set and install-time CLI smoke tests.
The release-set check then requires exactly one supported wheel per platform,
matching versions, valid package metadata, and bounded artifact size before
publishing.

Re-running a partially completed tagged release skips files PyPI already
accepted and repairs missing GitHub Release assets.

## Related documents

- [Installation](../installation.md) describes the consumer-facing install
  path and supported platforms.
- [Engineering notes](engineering.md) separate contributor status from public
  product contracts.
- [Reproducible examples](examples.md) exercise the installed product workflow.

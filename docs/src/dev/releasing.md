# Releasing `persisting`

Stable releases are built by GitHub Actions from version tags and published to
PyPI with Trusted Publishing. The release contains CPython 3.10+ ABI3 wheels for
Linux x86_64 and Apple Silicon macOS. Source distributions and the
standalone `dldb` package are not part of this workflow.

## One-time setup

1. Create a GitHub environment named `pypi`. Do not require reviewers; restrict
   deployments to tags matching `v*`.
2. In the PyPI account publishing settings, add a pending Trusted Publisher:
   - PyPI project: `persisting`
   - GitHub owner: `DeepLink-org`
   - Repository: `Persisting`
   - Workflow: `release.yml`
   - Environment: `pypi`

No PyPI API token is stored in GitHub. The pending publisher creates the PyPI
project during the first successful upload, but does not reserve the name in
advance.

## Prepare a release

1. Update the same `X.Y.Z` version in `pyproject.toml`, the workspace package
   section of `Cargo.toml`, and `persisting/__init__.py`.
2. Refresh local workspace versions in the lockfile without upgrading
   dependencies:

   ```bash
   cargo metadata --format-version 1 --no-deps >/dev/null
   ```

3. Commit and merge the version change to `main`. The release workflow refuses
   tags whose commit is not reachable from `main`.
4. Optionally run the **Publish PyPI** workflow manually. Manual runs build and
   verify all wheels but never publish them.
5. Create and push the matching stable tag:

   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

The workflow validates the version contract and lockfile before compiling. It
then publishes the two verified wheels to PyPI and creates a GitHub Release
from the same artifacts. Re-running a partially completed release skips files
that PyPI already accepted and repairs missing GitHub Release assets.

#!/usr/bin/env bash
# Refresh crates/persisting-fs-overlay from upstream fuse-overlayfs.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="${repo_root}/crates/persisting-fs-overlay"
ref="${1:-}"
url="${FUSE_OVERLAYFS_URL:-https://github.com/containers/fuse-overlayfs.git}"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/fuse-overlayfs-sync.XXXXXX")"

cleanup() { rm -rf "${tmp}"; }
trap cleanup EXIT

echo "cloning ${url} ${ref:-HEAD} ..."
if [[ -n "${ref}" ]]; then
  git clone --depth 1 --branch "${ref}" "${url}" "${tmp}/src" \
    || { git clone "${url}" "${tmp}/src"; git -C "${tmp}/src" checkout "${ref}"; }
else
  git clone --depth 1 "${url}" "${tmp}/src"
fi

git -C "${tmp}/src" rev-parse HEAD > "${tmp}/UPSTREAM.REVISION"
git -C "${tmp}/src" remote get-url origin > "${tmp}/UPSTREAM.REMOTE"
git -C "${tmp}/src" log -1 --format='%ci %s' > "${tmp}/UPSTREAM.LOG"

rm -rf "${tmp}/src/.git" "${tmp}/src/.github"
# Workspace member must not redefine [profile.*]
rm -f "${tmp}/src/Cargo.lock"
if [[ -f "${tmp}/src/Cargo.toml" ]]; then
  # Rewrite package identity for the Persisting path; keep upstream sources.
  cat > "${tmp}/src/Cargo.toml" <<'EOF'
[package]
name = "persisting-fs-overlay"
version = "2.0.0"
edition = "2024"
rust-version = "1.85.0"
license = "GPL-3.0-or-later"
description = "Overlay filesystem in userspace (vendored fuse-overlayfs for pVisor)"
repository = "https://github.com/containers/fuse-overlayfs"
homepage = "https://github.com/containers/fuse-overlayfs"
readme = "README.md"
keywords = ["fuse", "overlayfs", "filesystem", "containers"]
categories = ["filesystem"]
exclude = [
    ".github/",
    "tests/",
    "clippy",
    "Containerfile.*",
    "AGENTS.md",
]

[[bin]]
name = "fuse-overlayfs"
path = "src/main.rs"

[dependencies]
fuser = { version = "0.17.0", features = ["abi-7-40"] }
rustix = { version = "1.1", features = ["process"] }
libc = "0.2.183"
signal-hook = "0.3.18"
parking_lot = "0.12.5"
log = "0.4.29"
env_logger = "0.11.9"
thiserror = "2.0.18"
rustc-hash = "2.1.1"

[dev-dependencies]
tempfile = "3.27.0"
EOF
fi

# Preserve Persisting-facing README / pins; replace the rest of the tree.
mkdir -p "${dest}"
rsync -a --delete \
  --exclude README.md \
  --exclude UPSTREAM.REVISION \
  --exclude UPSTREAM.REMOTE \
  --exclude UPSTREAM.LOG \
  --exclude target \
  "${tmp}/src/" "${dest}/"

cp "${tmp}/UPSTREAM.REVISION" "${dest}/UPSTREAM.REVISION"
cp "${tmp}/UPSTREAM.REMOTE" "${dest}/UPSTREAM.REMOTE"
cp "${tmp}/UPSTREAM.LOG" "${dest}/UPSTREAM.LOG"

# Ensure Persisting README remains if sync wiped it (rsync exclude keeps it).
if [[ ! -f "${dest}/README.md" ]]; then
  echo "warning: ${dest}/README.md missing after sync" >&2
fi

echo "vendored $(cat "${dest}/UPSTREAM.REVISION") → ${dest}"

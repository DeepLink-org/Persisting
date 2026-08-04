#!/usr/bin/env bash
# Install a guest pVisor runtime from a GitHub Release (default tag: nightly).
#
# Container/KVM executors inject a static Linux pVisor into the guest. This
# script downloads the matching binary and places it where pVisor's automatic
# artifact discovery looks for it:
#
#   $PERSISTING_PVISOR_RUNTIME_DIR/<platform>/pvisor          (when set)
#   ~/.persisting/runtimes/<version>/<platform>/pvisor        (default)
#
# Usage:
#   bash scripts/install-guest-runtimes.sh --platform linux-amd64
#   curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-guest-runtimes.sh | bash -s -- --platform linux-arm64
#
# Options:
#   --platform PLATFORM   linux-amd64 (default) or linux-arm64
#   --version VERSION     exact version, e.g. 0.2.0 (default: newest in release)
#
# Environment:
#   PERSISTING_GITHUB_REPO        owner/repo (default: DeepLink-org/Persisting)
#   PERSISTING_NIGHTLY_TAG        release tag (default: nightly)
#   PERSISTING_RELEASE_API        API URL override (default: GitHub releases API)
#   PERSISTING_PVISOR_RUNTIME_DIR install root override
#   PYTHON                        interpreter for the GitHub API call (default: python3)

set -euo pipefail

REPO="${PERSISTING_GITHUB_REPO:-DeepLink-org/Persisting}"
TAG="${PERSISTING_NIGHTLY_TAG:-nightly}"
PYTHON="${PYTHON:-python3}"
PLATFORM=""
VERSION=""

usage() {
  cat <<'EOF'
usage: install-guest-runtimes.sh [--platform PLATFORM] [--version VERSION]

  --platform PLATFORM   linux-amd64 (default) or linux-arm64
  --version VERSION     exact version (default: newest in the release)

environment:
  PERSISTING_GITHUB_REPO        owner/repo (default: DeepLink-org/Persisting)
  PERSISTING_NIGHTLY_TAG        release tag (default: nightly)
  PERSISTING_PVISOR_RUNTIME_DIR install root override
  PYTHON                        interpreter for the GitHub API call
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --platform) PLATFORM="${2:-}"; shift 2 ;;
    --version) VERSION="${2:-}"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) echo "error: unknown option '$1'" >&2; usage >&2; exit 2 ;;
  esac
done

case "$PLATFORM" in
  linux-amd64|linux-arm64) ;;
  "") PLATFORM="linux-amd64" ;;
  *) echo "error: unsupported guest platform '$PLATFORM' (linux-amd64 or linux-arm64)" >&2; exit 1 ;;
esac

if ! command -v "$PYTHON" >/dev/null 2>&1; then
  echo "error: Python not found ($PYTHON)" >&2
  exit 1
fi

api="${PERSISTING_RELEASE_API:-https://api.github.com/repos/${REPO}/releases/tags/${TAG}}"

echo "Fetching nightly release assets from ${REPO} (tag=${TAG})..." >&2

asset_info="$("$PYTHON" - "$api" "$PLATFORM" "$VERSION" <<'PY'
import json
import re
import sys
import urllib.error
import urllib.request

api, platform, wanted = sys.argv[1], sys.argv[2], sys.argv[3]
pattern = re.compile(
    r"^persisting-guest-runtime-(\d+\.\d+\.\d+)-" + re.escape(platform) + r"\.tar\.gz$"
)

req = urllib.request.Request(api, headers={"Accept": "application/vnd.github+json"})
try:
    with urllib.request.urlopen(req, timeout=60) as resp:
        data = json.load(resp)
except urllib.error.HTTPError as e:
    if e.code == 404:
        sys.exit(
            f"release '{api.rsplit('/', 1)[-1]}' not found — wait for the Nightly Build "
            "workflow on main, or set PERSISTING_NIGHTLY_TAG"
        )
    raise

best = None  # (version, asset_name, url)
for asset in data.get("assets", []):
    name = asset.get("name", "")
    match = pattern.match(name)
    if not match:
        continue
    candidate = (match.group(1), name, asset["browser_download_url"])
    if best is None or candidate[0] > best[0]:
        best = candidate

if best is None:
    sys.exit(
        f"no persisting guest runtime for {platform} in release — "
        f"the Nightly Build workflow may not have produced one yet"
    )
version, name, url = best
if wanted and wanted != version:
    sys.exit(f"requested version {wanted} is not available; newest is {version}")
print(version)
print(name)
print(url)
PY
)"

VERSION="$(printf '%s\n' "$asset_info" | sed -n '1p')"
ASSET_NAME="$(printf '%s\n' "$asset_info" | sed -n '2p')"
ASSET_URL="$(printf '%s\n' "$asset_info" | sed -n '3p')"

work="$(mktemp -d "${TMPDIR:-/tmp}/persisting-guest-runtime.XXXXXX")"
trap 'rm -rf "$work"' EXIT

echo "Downloading ${ASSET_NAME}..." >&2
curl -fsSL "$ASSET_URL" -o "$work/$ASSET_NAME"
curl -fsSL "$ASSET_URL.sha256" -o "$work/$ASSET_NAME.sha256"

verify_sha256() {
  local file="$1" sumfile="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$work" && sha256sum -c "$sumfile")
  else
    (cd "$work" && shasum -a 256 -c "$sumfile")
  fi
}
verify_sha256 "$work/$ASSET_NAME" "$ASSET_NAME.sha256"

tar -xzf "$work/$ASSET_NAME" -C "$work"

if [ -n "${PERSISTING_PVISOR_RUNTIME_DIR:-}" ]; then
  install_dir="$PERSISTING_PVISOR_RUNTIME_DIR/$PLATFORM"
else
  install_dir="$HOME/.persisting/runtimes/$VERSION/$PLATFORM"
fi

mkdir -p "$install_dir"
install -m 0755 "$work/runtimes/$PLATFORM/pvisor" "$install_dir/pvisor"
echo "Installed guest pVisor ($PLATFORM v$VERSION): $install_dir/pvisor"
echo "Container/KVM executors will discover it automatically."

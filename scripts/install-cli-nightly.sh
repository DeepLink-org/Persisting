#!/usr/bin/env bash
# Install the unified Persisting CLI component set from a GitHub Release
# (default tag: nightly): `persisting`, `pvisor`, `ppilot`, and the matching
# `libpersisting_engine` shared library.
#
# The component set lands in:
#
#   $PERSISTING_CLI_ROOT/cli/bin    (default: $HOME/.persisting/cli/bin)
#
# Add that directory to your PATH (the installer prints the export line).
#
# Usage:
#   bash scripts/install-cli-nightly.sh
#   curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-cli-nightly.sh | bash
#
# Environment:
#   PERSISTING_GITHUB_REPO   owner/repo (default: DeepLink-org/Persisting)
#   PERSISTING_NIGHTLY_TAG   release tag (default: nightly)
#   PERSISTING_RELEASE_API   API URL override (default: GitHub releases API)
#   PERSISTING_CLI_ROOT      install root (default: $HOME/.persisting)
#   PYTHON                   interpreter for the GitHub API call (default: python3)

set -euo pipefail

REPO="${PERSISTING_GITHUB_REPO:-DeepLink-org/Persisting}"
TAG="${PERSISTING_NIGHTLY_TAG:-nightly}"
PYTHON="${PYTHON:-python3}"
CLI_ROOT="${PERSISTING_CLI_ROOT:-$HOME/.persisting}"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target="x86_64-unknown-linux-gnu" ;;
  Darwin-arm64) target="aarch64-apple-darwin" ;;
  Darwin-x86_64) target="x86_64-apple-darwin" ;;
  *)
    echo "error: unsupported platform $(uname -s)-$(uname -m)" >&2
    echo "  available CLI archives: x86_64-unknown-linux-gnu, aarch64-apple-darwin, x86_64-apple-darwin" >&2
    exit 1
    ;;
esac

if ! command -v "$PYTHON" >/dev/null 2>&1; then
  echo "error: Python not found ($PYTHON)" >&2
  exit 1
fi

api="${PERSISTING_RELEASE_API:-https://api.github.com/repos/${REPO}/releases/tags/${TAG}}"

echo "Fetching nightly release assets from ${REPO} (tag=${TAG})..." >&2

asset_info="$("$PYTHON" - "$api" "$target" <<'PY'
import json
import re
import sys
import urllib.error
import urllib.request

api, target = sys.argv[1], sys.argv[2]
pattern = re.compile(r"^persisting-cli-(\d+\.\d+\.\d+)-" + re.escape(target) + r"\.tar\.gz$")

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
        f"no Persisting CLI archive for {target} in release — "
        f"the Nightly Build workflow may not have produced one yet"
    )
version, name, url = best
print(version)
print(name)
print(url)
PY
)"

VERSION="$(printf '%s\n' "$asset_info" | sed -n '1p')"
ASSET_NAME="$(printf '%s\n' "$asset_info" | sed -n '2p')"
ASSET_URL="$(printf '%s\n' "$asset_info" | sed -n '3p')"

work="$(mktemp -d "${TMPDIR:-/tmp}/persisting-cli.XXXXXX")"
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

install_root="$CLI_ROOT/cli"
rm -rf "$install_root"
mkdir -p "$install_root"
cp -R "$work/cli/." "$install_root/"

bin_dir="$install_root/bin"
echo "Installed Persisting CLI component set v$VERSION in $bin_dir"
echo "Add it to your PATH:"
printf '  export PATH="%s:$PATH"\n' "$bin_dir"
echo "Or run it directly: $bin_dir/persisting --help"

#!/usr/bin/env bash
# Install the latest persisting nightly wheel from GitHub Releases (tag: nightly).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
#   PERSISTING_GITHUB_REPO=DeepLink-org/Persisting PERSISTING_NIGHTLY_TAG=nightly ./scripts/install-nightly.sh
#
# Environment:
#   PYTHON                   Python interpreter (default: python3)
#   PERSISTING_GITHUB_REPO   owner/repo (default: DeepLink-org/Persisting)
#   PERSISTING_NIGHTLY_TAG   release tag (default: nightly)

set -euo pipefail

REPO="${PERSISTING_GITHUB_REPO:-DeepLink-org/Persisting}"
TAG="${PERSISTING_NIGHTLY_TAG:-nightly}"
PYTHON="${PYTHON:-python3}"

if ! command -v "$PYTHON" >/dev/null 2>&1; then
  echo "error: Python not found ($PYTHON)" >&2
  exit 1
fi

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) platform_re='manylinux.*x86_64' ;;
  Linux-aarch64) platform_re='manylinux.*aarch64' ;;
  Darwin-arm64) platform_re='macosx.*arm64' ;;
  *)
    echo "error: unsupported platform $(uname -s)-$(uname -m)" >&2
    exit 1
    ;;
esac

min_py="$("$PYTHON" -c 'import sys; print(sys.version_info.major * 10 + sys.version_info.minor)')"
if [ "$min_py" -lt 310 ]; then
  echo "error: persisting wheels require Python 3.10+; got $("$PYTHON" --version)" >&2
  exit 1
fi
api="https://api.github.com/repos/${REPO}/releases/tags/${TAG}"

echo "Fetching nightly release assets from ${REPO} (tag=${TAG})..." >&2

url="$("$PYTHON" - <<PY
import json
import re
import sys
import urllib.error
import urllib.request

api = "${api}"
platform_re = re.compile(r"${platform_re}")

# Wheels contain native CLIs but only pure Python modules, so one py3-none wheel
# is published per supported OS/architecture.
PY3_RE = re.compile(r"-py3-none-")

req = urllib.request.Request(api, headers={"Accept": "application/vnd.github+json"})
try:
    with urllib.request.urlopen(req, timeout=60) as resp:
        data = json.load(resp)
except urllib.error.HTTPError as e:
    if e.code == 404:
        sys.exit(
            "nightly release not found — wait for the Nightly Build workflow on main, "
            f"or open https://github.com/${REPO}/actions/workflows/nightly.yml"
        )
    raise

for asset in data.get("assets", []):
    name = asset.get("name", "")
    if not name.endswith(".whl") or not name.startswith("persisting-"):
        continue
    if not PY3_RE.search(name):
        continue
    if platform_re.search(name):
        print(asset["browser_download_url"])
        break
else:
    sys.exit(
        f"no platform wheel for ${platform_re.pattern} in nightly release — "
        "check https://github.com/" + "${REPO}" + "/releases/tag/nightly"
    )
PY
)"

echo "Installing ${url}" >&2
"$PYTHON" -m pip install --upgrade pip
"$PYTHON" -m pip install --force-reinstall "$url"
"$PYTHON" -c "import persisting; print('persisting', persisting.__version__)"

scripts_dir="$($PYTHON -c 'import sysconfig; print(sysconfig.get_path("scripts"))')"
for binary in pchronicle pvisor ppilot; do
  if [ ! -x "$scripts_dir/$binary" ]; then
    echo "error: wheel did not install executable $scripts_dir/$binary" >&2
    exit 1
  fi
  "$scripts_dir/$binary" --version
done
echo "CLI component set installed in $scripts_dir" >&2
case ":$PATH:" in
  *":$scripts_dir:"*) ;;
  *) echo "Add it to PATH: export PATH=\"$scripts_dir:\$PATH\"" >&2 ;;
esac

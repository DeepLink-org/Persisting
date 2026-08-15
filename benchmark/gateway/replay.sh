#!/usr/bin/env bash
set -euo pipefail

scenario_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$scenario_dir/replay.py" "$@"

#!/usr/bin/env bash
set -euo pipefail

scenario_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
exec env PERSISTING_GATEWAY_FUZZ_SUITE=formats \
  uv run --isolated --no-project \
  --with-requirements "$scenario_dir/requirements.txt" \
  python "$scenario_dir/regression.py"

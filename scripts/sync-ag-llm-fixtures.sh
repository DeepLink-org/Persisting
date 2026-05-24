#!/usr/bin/env bash
# Sync agentgateway LLM test fixtures into persisting-capture for regression tests.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${AG_SRC:-$ROOT/../agentgateway/crates/agentgateway/src/llm/tests}"
DST="$ROOT/crates/persisting-capture/tests/fixtures"
if [[ ! -d "$SRC" ]]; then
  echo "error: agentgateway tests not found at $SRC" >&2
  echo "set AG_SRC to agentgateway .../src/llm/tests" >&2
  exit 1
fi
rsync -a --delete "$SRC/" "$DST/"
echo "synced $SRC -> $DST"

#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
PVISOR="${PVISOR_BIN:-$ROOT/target/debug/pvisor}"
[[ -x "$PVISOR" ]] || (cd "$ROOT" && cargo build -q -p persisting-pvisor --bin pvisor)

WORK="$(mktemp -d "${TMPDIR:-/tmp}/pvisor-gateway.XXXXXX")"
PYTHONDONTWRITEBYTECODE=1 MOCK_LLM_PORT=19080 python3 "$DIR/mock_llm.py" >"$WORK/mock.log" 2>&1 &
MOCK_PID=$!
trap 'kill "$MOCK_PID" 2>/dev/null || true; wait "$MOCK_PID" 2>/dev/null || true; rm -rf "$WORK"' EXIT
sleep 0.3
kill -0 "$MOCK_PID" 2>/dev/null || { cat "$WORK/mock.log" >&2; exit 1; }

"$PVISOR" run --config "$DIR/run.toml" --workspace "$WORK/run" --stdio capture -- \
  env PYTHONDONTWRITEBYTECODE=1 python3 "$DIR/agent.py"

MD="$(find "$WORK/run" -type f -name '*.md' -print -quit)"
UPSTREAM_POSTS="$(grep -c 'POST /v1/chat/completions' "$WORK/mock.log")"
SINK_REQUESTS="$(jq -r '.network.intercepted.sink_requests' "$WORK/run/run-bundle.json")"
FAILURES="$(jq -r '.network.intercepted.failures' "$WORK/run/run-bundle.json")"
BLOCKS="$(grep -c '^<!-- persisting:block' "$MD")"

printf 'RESULT upstream_posts=%s sink_requests=%s agenticmd_blocks=%s failures=%s\n' \
  "$UPSTREAM_POSTS" "$SINK_REQUESTS" "$BLOCKS" "$FAILURES"
[[ "$UPSTREAM_POSTS" == "2" && "$SINK_REQUESTS" == "2" && "$BLOCKS" == "4" && "$FAILURES" == "0" ]]
echo 'CONCLUSION Gateway routed and captured two complete LLM request/response interactions'

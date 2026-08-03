#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
PVISOR="${PVISOR_BIN:-$ROOT/target/debug/pvisor}"
[[ -x "$PVISOR" ]] || (cd "$ROOT" && cargo build -q -p persisting-pvisor --bin pvisor)

PORT="${PORT:-19111}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/pvisor-network.XXXXXX")"
printf 'ok\n' > "$WORK/index.html"
python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$WORK" >"$WORK/server.log" 2>&1 &
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null || true; wait "$SERVER_PID" 2>/dev/null || true; rm -rf "$WORK"' EXIT
sleep 0.3
kill -0 "$SERVER_PID" 2>/dev/null || { cat "$WORK/server.log" >&2; exit 1; }

"$PVISOR" run --workspace "$WORK/allow-run" --overlaynet-allow "127.0.0.1:$PORT" --stdio capture -- \
  /bin/sh -c 'curl --fail --silent --proxy "$HTTP_PROXY" --noproxy "" "$1"' sh "http://127.0.0.1:$PORT/"
set +e
"$PVISOR" run --workspace "$WORK/deny-run" --overlaynet-deny "127.0.0.1:$PORT" --stdio capture -- \
  /bin/sh -c 'curl --fail --silent --proxy "$HTTP_PROXY" --noproxy "" "$1"' sh \
  "http://127.0.0.1:$PORT/" >"$WORK/deny.log" 2>&1
DENY_EXIT=$?
set -e

ALLOWED="$(jq -r '.network.intercepted.policy_allowed' "$WORK/allow-run/run-bundle.json")"
DENIED="$(jq -r '.network.intercepted.policy_denied' "$WORK/deny-run/run-bundle.json")"
NON_BYPASSABLE="$(jq -r '.safety.network_non_bypassable' "$WORK/deny-run/run-bundle.json")"
printf 'RESULT policy_allowed=%s policy_denied=%s deny_exit=%s network_non_bypassable=%s\n' \
  "$ALLOWED" "$DENIED" "$DENY_EXIT" "$NON_BYPASSABLE"
[[ "$ALLOWED" == "1" && "$DENIED" == "1" && "$DENY_EXIT" != "0" && "$NON_BYPASSABLE" == "false" ]]
echo 'CONCLUSION intercepted HTTP obeyed allow and deny policy; the Bundle preserved the cooperative boundary'

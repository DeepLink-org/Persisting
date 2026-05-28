#!/usr/bin/env bash
# Capture real-time append stress: concurrent proxy requests + trajectory durability check.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/integration/_common.sh
source "$REPO_ROOT/scripts/integration/_common.sh"

MOCK_PY="$REPO_ROOT/scripts/integration/mock_llm_api_server.py"
LOAD_PY="$REPO_ROOT/scripts/integration/capture_stress_load.py"

REQUESTS="${REQUESTS:-80}"
CONCURRENCY="${CONCURRENCY:-8}"
WARMUP="${WARMUP:-2}"
DRAIN_SEC="${DRAIN_SEC:-45}"
MIN_SUCCESS_RATE="${MIN_SUCCESS_RATE:-0.98}"
MIN_ROW_RATIO="${MIN_ROW_RATIO:-0.95}"
MAX_P99_MS="${MAX_P99_MS:-15000}"

die() { capture_die "$@"; }
pick_port() { capture_pick_port; }
wait_http() { capture_wait_http "$@"; }

capture_resolve_binaries "${SKIP_BUILD:-0}" 1
command -v python3 >/dev/null || die "need python3"
[[ -f "$MOCK_PY" && -f "$LOAD_PY" ]] || die "missing stress scripts"

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/persisting-capture-stress.XXXXXX")"
STORAGE="$WORKDIR/store"
mkdir -p "$STORAGE"
SUMMARY_JSON="$WORKDIR/load.json"
SERVE_LOG="$WORKDIR/serve.log"
STATS_TOML="$WORKDIR/stats.toml"

AGENT_ID="stress-agent"
SESSION_ID="stress-session-$$"
EXPECTED_ROWS=$((REQUESTS * 2))

MOCK_PORT="$(pick_port)"
PROXY_PORT="$(pick_port)"
ADMIN_PORT="$(pick_port)"
while [[ "$PROXY_PORT" == "$MOCK_PORT" || "$ADMIN_PORT" == "$MOCK_PORT" || "$ADMIN_PORT" == "$PROXY_PORT" ]]; do
  PROXY_PORT="$(pick_port)"
  ADMIN_PORT="$(pick_port)"
done

CONFIG="$WORKDIR/proxy.toml"
cat >"$CONFIG" <<EOF
listen = "127.0.0.1:${PROXY_PORT}"
admin_listen = "127.0.0.1:${ADMIN_PORT}"
agent_id = "${AGENT_ID}"
session_header = "x-persisting-session-id"

[[models]]
name = "*"
upstream = "http://127.0.0.1:${MOCK_PORT}/v1"
EOF

cleanup() {
  set +e
  [[ -n "${STORAGE:-}" ]] && "$CLI" capture stop -o "$STORAGE" >/dev/null 2>&1
  [[ -n "${MOCK_PID:-}" ]] && kill "$MOCK_PID" 2>/dev/null
  wait "${MOCK_PID:-}" 2>/dev/null || true
  kill "${SERVE_PID:-}" 2>/dev/null || true
}
trap cleanup EXIT

echo "==> capture stress"
echo "    CLI=$CLI"
echo "    requests=$REQUESTS concurrency=$CONCURRENCY expected_lance_rows=$EXPECTED_ROWS"

export MOCK_UPSTREAM_PORT="$MOCK_PORT"
python3 "$MOCK_PY" &
MOCK_PID=$!
sleep 0.2
kill -0 "$MOCK_PID" || die "mock upstream failed"

"$CLI" capture serve -o "$STORAGE" -c "$CONFIG" >>"$SERVE_LOG" 2>&1 &
SERVE_PID=$!
sleep 0.3
kill -0 "$SERVE_PID" || { tail -20 "$SERVE_LOG"; die "capture serve exited early"; }
wait_http "http://127.0.0.1:${ADMIN_PORT}/admin/status" || die "admin not ready"

echo "==> load"
python3 "$LOAD_PY" \
  --proxy "http://127.0.0.1:${PROXY_PORT}" \
  --session-id "$SESSION_ID" \
  -n "$REQUESTS" \
  -c "$CONCURRENCY" \
  --warmup "$WARMUP" \
  --json-out "$SUMMARY_JSON" || true

python3 <<PY
import json, sys
s = json.load(open("$SUMMARY_JSON"))
rate = s["success_rate"]
p99 = s["latency_ms"]["p99"]
print(f"load: ok={s['ok']} fail={s['fail']} rate={rate:.4f} rps={s['rps']} p99_ms={p99}")
if rate < float("$MIN_SUCCESS_RATE"):
    sys.exit("success rate below MIN_SUCCESS_RATE")
if p99 > float("$MAX_P99_MS"):
    sys.exit(f"p99 {p99}ms exceeds MAX_P99_MS=$MAX_P99_MS")
PY

echo "==> drain trajectory (up to ${DRAIN_SEC}s)"
best="$(capture_drain_lance_rows "$STORAGE" "$AGENT_ID" "$SESSION_ID" "$EXPECTED_ROWS" "$DRAIN_SEC" "$STATS_TOML" || true)"
APPEND_FAILS="$(grep -c 'trajectory append failed' "$SERVE_LOG" 2>/dev/null || true)"
APPEND_FAILS="${APPEND_FAILS:-0}"

echo "==> reliability: rows=$best/$EXPECTED_ROWS append_failures=$APPEND_FAILS"
[[ "${best:-0}" -ge 1 ]] || { tail -30 "$SERVE_LOG"; die "no Lance rows"; }

python3 <<PY
expected = int("$EXPECTED_ROWS")
best = int("${best:-0}")
if best / expected < float("$MIN_ROW_RATIO"):
    raise SystemExit(f"row ratio {best/expected:.4f} < MIN_ROW_RATIO")
if int("$APPEND_FAILS") > 0:
    raise SystemExit(f"{int('$APPEND_FAILS')} append failures in serve log")
PY

echo "==> capture stress OK"

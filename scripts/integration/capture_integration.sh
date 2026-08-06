#!/usr/bin/env bash
# Gateway/History CLI integration: arguments, daemon lifecycle, import, and pVisor execution.
#
# Usage:
#   PERSISTING_CLI=target/debug/persisting \
#     ./scripts/integration/capture_integration.sh
#
# Or: just capture-integration

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/integration/_common.sh
source "$REPO_ROOT/scripts/integration/_common.sh"

MOCK_PY="$REPO_ROOT/scripts/integration/mock_llm_api_server.py"
pass() { echo "  ok: $*"; }
section() { echo ""; echo "==> $*"; }
die() { capture_die "$@"; }
pick_port() { capture_pick_port; }
wait_http() { capture_wait_http "$@"; }

capture_resolve_binaries "${SKIP_BUILD:-0}"

command -v python3 >/dev/null || die "need python3"
command -v curl >/dev/null || die "need curl"
[[ -f "$MOCK_PY" ]] || die "missing $MOCK_PY"

run_cli() {
  "$CLI" "$@" 2>&1
}

expect_fail() {
  local desc="$1"
  shift
  if "$CLI" "$@" >/dev/null 2>&1; then
    die "expected failure: $desc"
  fi
  pass "$desc (exited non-zero)"
}

expect_grep() {
  local desc="$1" pattern="$2"
  shift 2
  local out
  out="$(run_cli "$@")" || die "$desc: command failed"
  grep -qE "$pattern" <<<"$out" || { echo "--- output ---"; echo "$out"; die "$desc: pattern not found: $pattern"; }
  pass "$desc"
}

daemon_json() {
  echo "$STORAGE/.capture/daemon.json"
}

cleanup() {
  set +e
  if [[ -n "${STORAGE:-}" ]]; then
    "$CLI" gateway stop -o "$STORAGE" >/dev/null 2>&1 || true
  fi
  if [[ -n "${MOCK_PID:-}" ]]; then
    kill "$MOCK_PID" 2>/dev/null || true
    wait "$MOCK_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

# --- temp workspace -------------------------------------------------------
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/persisting-gateway-it.XXXXXX")"
STORAGE="$WORKDIR/trajectory-store"
mkdir -p "$STORAGE"

MOCK_PORT="$(pick_port)"
PROXY_PORT="$(pick_port)"
ADMIN_PORT="$(pick_port)"

while [[ "$PROXY_PORT" == "$MOCK_PORT" || "$PROXY_PORT" == "$ADMIN_PORT" || "$ADMIN_PORT" == "$MOCK_PORT" ]]; do
  PROXY_PORT="$(pick_port)"
  ADMIN_PORT="$(pick_port)"
done

CONFIG="$WORKDIR/proxy.toml"
cat >"$CONFIG" <<EOF
listen = "127.0.0.1:${PROXY_PORT}"
admin_listen = "127.0.0.1:${ADMIN_PORT}"
agent_id = "capture-it-agent"
session_header = "x-persisting-session-id"

[[models]]
name = "*"
upstream = "http://127.0.0.1:${MOCK_PORT}/v1"
EOF

GATEWAY_JSONL="$WORKDIR/gateway.jsonl"
cat >"$GATEWAY_JSONL" <<'EOF'
{"source":"agentgateway","kind":"llm.request","timestamp":"2026-05-20T12:00:00Z","session_id":"gw-session-1","payload":{"model":"mock-model"}}
{"source":"agentgateway","kind":"llm.response","timestamp":"2026-05-20T12:00:01Z","session_id":"gw-session-1","payload":{"status":200,"body":{"choices":[{"message":{"role":"assistant","content":"hi"}}]}}}
EOF

echo "==> capture integration"
echo "    CLI=$CLI"
echo "    WORKDIR=$WORKDIR"
echo "    mock=$MOCK_PORT proxy=$PROXY_PORT admin=$ADMIN_PORT"

# --- 1. CLI help / argument surface ---------------------------------------
section "CLI help and required arguments"

if ! run_cli gateway start --help >/dev/null 2>&1; then
  die "binary missing 'gateway start' — rebuild: cargo build -p persisting-cli"
fi
expect_grep "gateway help lists lifecycle commands" "serve|start|stop|list|status" gateway --help
expect_grep "history import help" "provider|gateway|ide" history import --help

expect_fail "gateway start without config" gateway start -o "$STORAGE"
expect_fail "gateway serve without output dir" gateway serve -c "$CONFIG"

# --- 2. import (no daemon) ------------------------------------------------
section "history import (dry-run, gateway JSONL)"

expect_grep "import dry-run finds gateway records" "capture-it-agent|gw-session|imported|dry-run" \
  history import "$STORAGE" \
  --provider gateway \
  --gateway-input "$GATEWAY_JSONL" \
  --session-id gw-session-1 \
  --agent-id capture-it-agent \
  --dry-run

expect_fail "gateway provider without readable input" \
  history import "$STORAGE" --provider gateway --gateway-input /nonexistent/capture-it.jsonl

# --- 3. mock upstream + daemon start ----------------------------------------
section "mock upstream and gateway start"

export MOCK_UPSTREAM_PORT="$MOCK_PORT"
python3 "$MOCK_PY" &
MOCK_PID=$!
sleep 0.3
kill -0 "$MOCK_PID" 2>/dev/null || die "mock upstream failed to start"

run_cli gateway start -o "$STORAGE" -c "$CONFIG" >/dev/null || die "gateway start failed"
pass "gateway start"

[[ -f "$(daemon_json)" ]] || die "missing daemon.json"
pass "daemon.json written"

DJ="$(cat "$(daemon_json)")"
echo "$DJ" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d.get("listen")==f"127.0.0.1:{sys.argv[1]}", d' "$PROXY_PORT" \
  || die "daemon.json listen mismatch"
pass "daemon.json fields"

wait_http "http://127.0.0.1:${ADMIN_PORT}/admin/status" 80 || die "admin /admin/status not ready"
pass "admin API ready"

# --- 4. status / list -------------------------------------------------------
section "gateway status and list"

STATUS_JSON="$(curl -sf "http://127.0.0.1:${ADMIN_PORT}/admin/status")"
echo "$STATUS_JSON" | python3 -c 'import json,sys; j=json.load(sys.stdin); assert "listen" in j and "sessions" in j' \
  || die "invalid admin status JSON"
pass "GET /admin/status JSON"

run_cli gateway status -o "$STORAGE" >/dev/null || die "gateway status failed"
pass "gateway status CLI"

LIST_OUT="$(run_cli gateway list -o "$STORAGE")"
if grep -q 'No capture sessions found' <<<"$LIST_OUT"; then
  pass "gateway list empty before proxy traffic"
else
  grep -q 'agent_id' <<<"$LIST_OUT" || { echo "$LIST_OUT"; die "gateway list missing table header"; }
  pass "gateway list table header"
fi

# --- 5. double start --------------------------------------------------------
section "double start rejected"

DAEMON_PID="$(python3 -c "import json; print(json.load(open('$(daemon_json)'))['pid'])")"
kill -0 "$DAEMON_PID" 2>/dev/null || die "daemon pid $DAEMON_PID not running before double-start test"

START2_OUT="$(run_cli gateway start -o "$STORAGE" -c "$CONFIG" || true)"
if grep -qiE 'already running' <<<"$START2_OUT"; then
  pass "second start rejected (already running)"
else
  echo "$START2_OUT"
  die "expected 'already running' on second start"
fi

# --- 6. proxy request + persisted session -----------------------------------
section "proxy request and persisted session"

SESSION_ID="it-session-$(date +%s)"
curl -sf "http://127.0.0.1:${PROXY_PORT}/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -H "x-persisting-session-id: ${SESSION_ID}" \
  -d '{"model":"mock-model","messages":[{"role":"user","content":"ping"}]}' >/dev/null \
  || die "proxy chat/completions failed"

pass "proxy forwarded to mock upstream"

sleep 0.5
LIST2="$(run_cli gateway list -o "$STORAGE")"
RUN_BUCKET="$(capture_read_run_session "$STORAGE")"
grep -q "$RUN_BUCKET" <<<"$LIST2" || { echo "$LIST2"; die "gateway list missing Run bucket after request"; }
SESSION_MD="$STORAGE/capture-it-agent/$RUN_BUCKET/$SESSION_ID.md"
[[ -f "$SESSION_MD" ]] || die "missing captured session Markdown: $SESSION_MD"
grep -q 'ping' "$SESSION_MD" || die "captured session Markdown missing request content"
pass "gateway list shows Run bucket and capture persists the header session"

AUTO_STATUS="$(run_cli gateway status)"
grep -q 'via process' <<<"$AUTO_STATUS" || { echo "$AUTO_STATUS"; die "gateway status did not auto-discover the serve process"; }
pass "gateway status auto-discovers gateway serve"

# --- 7. execute (in-process Gateway, no forked daemon) -----------------------
section "execute (subprocess + injected Gateway env)"

RUN_STORAGE="$WORKDIR/run-store"
mkdir -p "$RUN_STORAGE"
RUN_PROXY_PORT="$(pick_port)"
RUN_ADMIN_PORT="$(pick_port)"
while [[ "$RUN_PROXY_PORT" == "$RUN_ADMIN_PORT" || "$RUN_PROXY_PORT" == "$MOCK_PORT" || "$RUN_ADMIN_PORT" == "$MOCK_PORT" ]]; do
  RUN_PROXY_PORT="$(pick_port)"
  RUN_ADMIN_PORT="$(pick_port)"
done
RUN_OUT="$(run_cli execute \
  --workspace "$RUN_STORAGE" \
  --agent capture-it-agent \
  --overlaynet-listen "127.0.0.1:${RUN_PROXY_PORT}" \
  --gateway-mode capture \
  --gateway-admin-listen "127.0.0.1:${RUN_ADMIN_PORT}" \
  --gateway-route "name=\"*\", upstream=\"http://127.0.0.1:${MOCK_PORT}/v1\"" \
  --chronicle-mode lance \
  --chronicle-dir "$RUN_STORAGE" \
  -- true)"
pass "execute completed with an in-process Gateway"

[[ -f "$RUN_STORAGE/.capture/run_session" ]] || die "missing run_session file"
RUN_SESSION="$(capture_read_run_session "$RUN_STORAGE")"
[[ "$RUN_SESSION" == run-* ]] || die "run_session content mismatch: $RUN_SESSION"
pass "run_session=$RUN_SESSION"

# --- 8. stop ----------------------------------------------------------------
section "gateway stop"

run_cli gateway stop -o "$STORAGE" >/dev/null || die "gateway stop failed"
pass "gateway stop"

sleep 0.3
[[ ! -f "$(daemon_json)" ]] && pass "daemon.json removed" || die "daemon.json still present after stop"

expect_fail "status after stop" gateway status -o "$STORAGE"

# --- 9. storage resolution via env ------------------------------------------
section "PERSISTING_CAPTURE_STORAGE resolution"

export PERSISTING_CAPTURE_STORAGE="$STORAGE"
run_cli gateway list >/dev/null || die "gateway list with PERSISTING_CAPTURE_STORAGE failed"
unset PERSISTING_CAPTURE_STORAGE
pass "list without positional STORAGE (env)"

if run_cli gateway start -o "$STORAGE" -c "$CONFIG" >/dev/null; then
  pass "restart after stop"
  run_cli gateway stop -o "$STORAGE" >/dev/null || die "stop after restart"
fi

section "done"
echo "==> capture integration OK"

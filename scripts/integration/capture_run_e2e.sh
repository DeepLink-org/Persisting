#!/usr/bin/env bash
# E2E: `persisting execute` + mock LLM API — Lance drain, materialize, and replay.
#
# Flow:
#   mock LLM API (upstream) ← in-process Gateway ← agent (pVisor-injected OPENAI_BASE_URL)
#
# Usage:
#   ./scripts/integration/capture_run_e2e.sh
#   TURNS=5 just capture-run-e2e
#
# Env:
#   TURNS           default 3
#   DRAIN_SEC       default 60
#   SKIP_BUILD      default 0

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/integration/_common.sh
source "$REPO_ROOT/scripts/integration/_common.sh"

MOCK_API="$REPO_ROOT/scripts/integration/mock_llm_api_server.py"
AGENT_PY="$REPO_ROOT/scripts/integration/capture_run_agent.py"

TURNS="${TURNS:-3}"
DRAIN_SEC="${DRAIN_SEC:-60}"
AGENT_ID="capture-run-e2e"

pass() { echo "  ok: $*"; }
section() { echo ""; echo "==> $*"; }
die() { capture_die "$@"; }

capture_resolve_binaries "${SKIP_BUILD:-0}"

command -v python3 >/dev/null || die "need python3"
[[ -f "$MOCK_API" && -f "$AGENT_PY" ]] || die "missing scripts"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/persisting-gateway-run-e2e.XXXXXX")"
WORKSPACE="$WORKDIR/workspace"
RUN_HOME="$WORKDIR/runs"
mkdir -p "$WORKSPACE"

MOCK_PORT="$(capture_pick_port)"
PROXY_PORT="$(capture_pick_port)"
ADMIN_PORT="$(capture_pick_port)"
while [[ "$PROXY_PORT" == "$MOCK_PORT" || "$ADMIN_PORT" == "$MOCK_PORT" || "$ADMIN_PORT" == "$PROXY_PORT" ]]; do
  PROXY_PORT="$(capture_pick_port)"
  ADMIN_PORT="$(capture_pick_port)"
done

MOCK_LOG="$WORKDIR/mock_requests.jsonl"
MANIFEST="$WORKDIR/agent_manifest.json"
REPLAY_TOML="$WORKDIR/replay.toml"
STATS_TOML="$WORKDIR/stats.toml"
: >"$MOCK_LOG"

cleanup() {
  set +e
  [[ -n "${MOCK_PID:-}" ]] && kill "$MOCK_PID" 2>/dev/null
  wait "${MOCK_PID:-}" 2>/dev/null || true
}
trap cleanup EXIT

echo "==> execute + Gateway capture E2E"
echo "    CLI=$CLI"
echo "    WORKDIR=$WORKDIR"
echo "    turns=$TURNS format=lance"
echo "    mock=127.0.0.1:${MOCK_PORT} proxy=127.0.0.1:${PROXY_PORT}"

section "start mock LLM API"
export MOCK_LLM_PORT="$MOCK_PORT"
export MOCK_REQUEST_LOG="$MOCK_LOG"
python3 "$MOCK_API" &
MOCK_PID=$!
sleep 0.25
kill -0 "$MOCK_PID" || die "mock API failed to start"
pass "mock LLM API listening"

section "execute (in-process Gateway + agent with OPENAI_BASE_URL)"
export CAPTURE_AGENT_TURNS="$TURNS"
export CAPTURE_AGENT_MANIFEST="$MANIFEST"

set +e
RUN_OUT="$(PERSISTING_RUN_HOME="$RUN_HOME" "$CLI" execute \
  --workspace "$WORKSPACE" \
  --agent "$AGENT_ID" \
  --overlaynet-listen "127.0.0.1:${PROXY_PORT}" \
  --gateway-mode capture \
  --gateway-admin-listen "127.0.0.1:${ADMIN_PORT}" \
  --gateway-session-header x-persisting-session-id \
  --gateway-route "name=\"*\", upstream=\"http://127.0.0.1:${MOCK_PORT}/v1\"" \
  --chronicle-mode lance \
  -- python3 "$AGENT_PY" 2>&1)"
RUN_CODE=$?
set -e
echo "$RUN_OUT"
[[ "$RUN_CODE" -eq 0 ]] || die "persisting execute agent exited $RUN_CODE"
echo "$RUN_OUT" | grep -q '\[capture-run-agent\] session=' || die "OPENAI_BASE_URL not injected"
pass "persisting execute completed"

RUN_STORAGE="$(find "$RUN_HOME" -mindepth 1 -maxdepth 1 -type d -name 'run-*' | head -n 1)"
[[ -n "$RUN_STORAGE" ]] || die "missing generated Run storage under $RUN_HOME"
STORAGE="$RUN_STORAGE/chronicle"
ROOT_SESSION="$(capture_read_run_session "$RUN_STORAGE")"
[[ "$ROOT_SESSION" == run-* ]] || die "unexpected run_session: $ROOT_SESSION"
echo "$RUN_OUT" | grep -q "session=${ROOT_SESSION}" || die "run session not in capture output"
pass "run_session=$ROOT_SESSION"

[[ -f "$MANIFEST" ]] || die "agent manifest missing"
python3 -c "import json,sys; m=json.load(open(sys.argv[1])); assert len(m)==int(sys.argv[2])" "$MANIFEST" "$TURNS"
pass "agent manifest has $TURNS turns"

MOCK_COUNT="$(wc -l <"$MOCK_LOG" | tr -d ' ')"
[[ "$MOCK_COUNT" -eq "$TURNS" ]] || die "mock API saw $MOCK_COUNT requests, expected $TURNS"
pass "mock API received $TURNS HTTP requests"

LANCE_PATH="$STORAGE/$AGENT_ID/$ROOT_SESSION/events.lance"
[[ -d "$LANCE_PATH" ]] || die "missing Lance log: $LANCE_PATH"
pass "events.lance present"

section "lance-only capture (no live markdown)"
MD_PATH="$STORAGE/$AGENT_ID/$ROOT_SESSION/${ROOT_SESSION}.md"
if [[ -f "$MD_PATH" ]]; then
  die "lance capture must not write .md during capture (found $MD_PATH); use \`history materialize\`"
fi
pass "no live markdown during lance capture"

section "wait for Lance drain (up to ${DRAIN_SEC}s)"
EXPECTED_ROWS=$((TURNS * 2 + 2))
best="$(capture_drain_event_rows "$STORAGE" "$AGENT_ID" "$ROOT_SESSION" "$EXPECTED_ROWS" "$DRAIN_SEC" "$STATS_TOML" || true)"
[[ "${best:-0}" -ge "$EXPECTED_ROWS" ]] || die "Lance rows ${best:-0} < expected $EXPECTED_ROWS"
pass "Lance row_count=$best (expected >= $EXPECTED_ROWS)"

section "history materialize (idempotent rebuild)"
MAT_OUT="$("$CLI" history materialize "$STORAGE" \
  --agent-id "$AGENT_ID" \
  --session-id "$ROOT_SESSION" 2>&1)"
echo "$MAT_OUT"
grep -q 'status = "ok"' <<<"$MAT_OUT" || die "history materialize failed"
pass "history materialize"

section "trajectory replay (lance) contains all turns"
"$CLI" history replay "$STORAGE" \
  --agent-id "$AGENT_ID" \
  --session-id "$ROOT_SESSION" \
  --storage-format lance >"$REPLAY_TOML"

python3 <<PY
import json, sys, re

turns = int("$TURNS")
replay = open("$REPLAY_TOML").read()
json.load(open("$MANIFEST"))

for i in range(1, turns + 1):
    user = f"turn-{i}"
    reply = f"mock-reply-to-{user}"
    if user not in replay:
        sys.exit(f"replay missing user message {user!r}")
    if reply not in replay:
        sys.exit(f"replay missing assistant reply {reply!r}")

llm_rows = len(re.findall(r'^kind = "llm\\.(request|response)"', replay, re.M))
if llm_rows < turns * 2:
    sys.exit(f"expected >= {turns * 2} llm events, got {llm_rows}")

print(f"verified {turns} user/assistant pairs in lance replay")
PY
pass "lance replay content matches agent manifest"

section "materialized markdown content"
MD_PATH="$STORAGE/$AGENT_ID/$ROOT_SESSION/${ROOT_SESSION}.md"
[[ -f "$MD_PATH" ]] || die "missing markdown: $MD_PATH"
python3 <<PY
import json, sys, re

turns = int("$TURNS")
md = open("$MD_PATH").read()
json.load(open("$MANIFEST"))

for i in range(1, turns + 1):
    user = f"turn-{i}"
    reply = f"mock-reply-to-{user}"
    if user not in md:
        sys.exit(f"markdown missing user message {user!r}")
    if reply not in md:
        sys.exit(f"markdown missing assistant reply {reply!r}")

print(f"verified {turns} user/assistant pairs in materialized markdown")
PY
pass "materialized markdown content matches agent manifest"

section "gateway list shows session"
LIST_OUT="$("$CLI" gateway list -o "$RUN_STORAGE" 2>&1)"
SESSION_PREFIX="${ROOT_SESSION:0:24}"
grep -q "$SESSION_PREFIX" <<<"$LIST_OUT" || { echo "$LIST_OUT"; die "gateway list missing run session"; }
pass "gateway list contains run_session"

section "done"
echo "==> execute + Gateway capture E2E OK"

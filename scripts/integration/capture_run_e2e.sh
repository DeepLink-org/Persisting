#!/usr/bin/env bash
# E2E: `capture run` + mock LLM API — every agent turn is recorded in trajectory.
#
# Flow:
#   mock LLM API (upstream) ← capture proxy ← agent (OPENAI_BASE_URL via capture run)
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
MOCK_API="$REPO_ROOT/scripts/integration/mock_llm_api_server.py"
AGENT_PY="$REPO_ROOT/scripts/integration/capture_run_agent.py"

TURNS="${TURNS:-3}"
DRAIN_SEC="${DRAIN_SEC:-60}"
BUILD_PROFILE="${PERSISTING_BUILD_PROFILE:-debug}"
AGENT_ID="capture-run-e2e"

die() { echo "error: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }
section() { echo ""; echo "==> $*"; }

cargo_target_dir() {
  (cd "$REPO_ROOT" && cargo metadata --format-version 1 --no-deps \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
}

engine_lib_name() {
  case "$(uname -s)" in
    Darwin) echo "libpersisting_engine.dylib" ;;
    MINGW*|MSYS*|CYGWIN*) echo "persisting_engine.dll" ;;
    *) echo "libpersisting_engine.so" ;;
  esac
}

pick_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}

TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo_target_dir)}"
ENGINE_NAME="$(engine_lib_name)"

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "==> cargo build -p persisting-cli -p persisting-engine"
  (cd "$REPO_ROOT" && cargo build -p persisting-cli -p persisting-engine) || die "cargo build failed"
fi

CLI="${PERSISTING_CLI:-$TARGET_DIR/$BUILD_PROFILE/persisting}"
[[ -x "$CLI" ]] || die "not executable: $CLI"

if [[ -n "${PERSISTING_ENGINE_LIB:-}" ]]; then
  export PERSISTING_ENGINE_LIB
elif [[ -f "$TARGET_DIR/$BUILD_PROFILE/$ENGINE_NAME" ]]; then
  export PERSISTING_ENGINE_LIB="$TARGET_DIR/$BUILD_PROFILE/$ENGINE_NAME"
else
  die "build persisting-engine or set PERSISTING_ENGINE_LIB"
fi

command -v python3 >/dev/null || die "need python3"
[[ -f "$MOCK_API" && -f "$AGENT_PY" ]] || die "missing scripts"

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/persisting-capture-run-e2e.XXXXXX")"
STORAGE="$WORKDIR/store"
mkdir -p "$STORAGE"

MOCK_PORT="$(pick_port)"
PROXY_PORT="$(pick_port)"
ADMIN_PORT="$(pick_port)"
while [[ "$PROXY_PORT" == "$MOCK_PORT" || "$ADMIN_PORT" == "$MOCK_PORT" || "$ADMIN_PORT" == "$PROXY_PORT" ]]; do
  PROXY_PORT="$(pick_port)"
  ADMIN_PORT="$(pick_port)"
done

SESSION_ID="capture-run-e2e-$$"
CONFIG="$WORKDIR/proxy.yaml"
MOCK_LOG="$WORKDIR/mock_requests.jsonl"
MANIFEST="$WORKDIR/agent_manifest.json"
REPLAY_TOML="$WORKDIR/replay.toml"
: >"$MOCK_LOG"

cat >"$CONFIG" <<EOF
listen: "127.0.0.1:${PROXY_PORT}"
admin_listen: "127.0.0.1:${ADMIN_PORT}"
agent_id: "${AGENT_ID}"
session_header: "x-persisting-session-id"
models:
  - name: "*"
    upstream: "http://127.0.0.1:${MOCK_PORT}/v1"
EOF

cleanup() {
  set +e
  [[ -n "${STORAGE:-}" ]] && "$CLI" capture stop -o "$STORAGE" >/dev/null 2>&1
  [[ -n "${MOCK_PID:-}" ]] && kill "$MOCK_PID" 2>/dev/null
  wait "${MOCK_PID:-}" 2>/dev/null || true
}
trap cleanup EXIT

echo "==> capture run E2E"
echo "    CLI=$CLI"
echo "    WORKDIR=$WORKDIR"
echo "    turns=$TURNS session=$SESSION_ID"
echo "    mock=127.0.0.1:${MOCK_PORT} proxy=127.0.0.1:${PROXY_PORT}"

section "start mock LLM API"
export MOCK_LLM_PORT="$MOCK_PORT"
export MOCK_REQUEST_LOG="$MOCK_LOG"
python3 "$MOCK_API" &
MOCK_PID=$!
sleep 0.25
kill -0 "$MOCK_PID" || die "mock API failed to start"
pass "mock LLM API listening"

section "capture run (in-process proxy + agent with OPENAI_BASE_URL)"
export CAPTURE_AGENT_TURNS="$TURNS"
export CAPTURE_AGENT_MANIFEST="$MANIFEST"

set +e
RUN_OUT="$("$CLI" capture run -o "$STORAGE" -c "$CONFIG" --session-id "$SESSION_ID" -- \
  python3 "$AGENT_PY" 2>&1)"
RUN_CODE=$?
set -e
echo "$RUN_OUT"
[[ "$RUN_CODE" -eq 0 ]] || die "capture run agent exited $RUN_CODE"
echo "$RUN_OUT" | grep -q '\[capture-run-agent\] session=' || die "OPENAI_BASE_URL not injected"
echo "$RUN_OUT" | grep -q "session=${SESSION_ID}" || die "session id not in run output"
pass "capture run completed"

[[ -f "$MANIFEST" ]] || die "agent manifest missing"
python3 -c "import json,sys; m=json.load(open(sys.argv[1])); assert len(m)==int(sys.argv[2])" "$MANIFEST" "$TURNS"
pass "agent manifest has $TURNS turns"

MOCK_COUNT="$(wc -l <"$MOCK_LOG" | tr -d ' ')"
[[ "$MOCK_COUNT" -eq "$TURNS" ]] || die "mock API saw $MOCK_COUNT requests, expected $TURNS"
pass "mock API received $TURNS HTTP requests"

section "wait for trajectory drain (up to ${DRAIN_SEC}s)"
EXPECTED_ROWS=$((TURNS * 2))
STATS_TOML="$WORKDIR/stats.toml"
best=0
for ((i = 0; i < DRAIN_SEC * 2; i++)); do
  if "$CLI" trajectory stats "$STORAGE" \
    --agent-id "$AGENT_ID" \
    --session-id "$SESSION_ID" \
    --storage-format lance >"$STATS_TOML" 2>/dev/null; then
    best="$(python3 -c "import re,sys; t=open(sys.argv[1]).read(); m=re.search(r'row_count\s*=\s*(\d+)', t); print(m.group(1) if m else '0')" "$STATS_TOML")"
    if [[ "$best" -ge "$EXPECTED_ROWS" ]]; then
      pass "Lance row_count=$best (expected >= $EXPECTED_ROWS)"
      break
    fi
  fi
  sleep 0.5
done
[[ "$best" -ge "$EXPECTED_ROWS" ]] || die "Lance rows $best < expected $EXPECTED_ROWS"

section "trajectory replay (markdown) contains all turns"
"$CLI" trajectory replay "$STORAGE" \
  --agent-id "$AGENT_ID" \
  --session-id "$SESSION_ID" \
  --storage-format markdown >"$REPLAY_TOML"

python3 <<PY
import json, re, sys

turns = int("$TURNS")
replay = open("$REPLAY_TOML").read()
manifest = json.load(open("$MANIFEST"))

for i in range(1, turns + 1):
    user = f"turn-{i}"
    reply = f"mock-reply-to-{user}"
    if user not in replay:
        sys.exit(f"replay missing user message {user!r}")
    if reply not in replay:
        sys.exit(f"replay missing assistant reply {reply!r}")

# Count dialogue blocks in markdown file if present
md = "$STORAGE/$AGENT_ID/$SESSION_ID/0001.md"
try:
    text = open(md).read()
    blocks = text.count("persisting:block")
    if blocks < turns * 2:
        sys.exit(f"markdown blocks {blocks} < {turns * 2}")
except FileNotFoundError:
    pass

print(f"verified {turns} user/assistant pairs in replay")
PY
pass "replay content matches agent manifest"

section "capture list shows session"
LIST_OUT="$("$CLI" capture list -o "$STORAGE" 2>&1)"
grep -q "$SESSION_ID" <<<"$LIST_OUT" || { echo "$LIST_OUT"; die "capture list missing session"; }
pass "capture list contains session_id"

section "done"
echo "==> capture run E2E OK"

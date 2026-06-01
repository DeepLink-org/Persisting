#!/usr/bin/env bash
# Trajectory CLI E2E: import → stats → judge → stats / judge-stats
#
# Usage:
#   PERSISTING_CLI=target/debug/persisting \
#   PERSISTING_ENGINE_LIB=target/debug/libpersisting_engine.dylib \
#     ./scripts/integration/traj_e2e.sh
#
# Or: just traj-e2e

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/integration/_common.sh
source "$REPO_ROOT/scripts/integration/_common.sh"

FIXTURE="$REPO_ROOT/scripts/integration/fixtures/traj_gateway_dialogue.jsonl"
AGENT_ID="traj-it-agent"
SESSION_ID="traj-it-session"
STATS_TOML=""
JUDGE_STATS_TOML=""

pass() { echo "  ok: $*"; }
section() { echo ""; echo "==> $*"; }
die() { capture_die "$@"; }

capture_resolve_binaries "${SKIP_BUILD:-0}" 1

[[ -f "$FIXTURE" ]] || die "missing fixture: $FIXTURE"

run_cli() {
  "$CLI" "$@" 2>&1
}

expect_grep() {
  local desc="$1" pattern="$2"
  shift 2
  local out
  out="$(run_cli "$@")" || die "$desc: command failed"
  grep -qE "$pattern" <<<"$out" || { echo "--- output ---"; echo "$out"; die "$desc: pattern not found: $pattern"; }
  pass "$desc"
}

parse_toml_field() {
  python3 -c "import re,sys; t=open(sys.argv[1]).read(); m=re.search(r'^${2}\s*=\s*(\S+)', t, re.M); print(m.group(1) if m else '')" "$1" "$2"
}

parse_plain_field() {
  python3 -c "
import sys
key = sys.argv[1]
for line in open(sys.argv[2]):
    if line.startswith(key + ':'):
        print(line.split(':', 1)[1].strip())
        break
" "$1" "$2"
}

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/persisting-traj-e2e.XXXXXX")"
STORAGE="$WORKDIR/store"
mkdir -p "$STORAGE"
STATS_TOML="$WORKDIR/stats.toml"
STATS_PLAIN="$WORKDIR/stats.plain"
JUDGE_STATS_TOML="$WORKDIR/judge_stats.toml"

echo "==> traj E2E integration"
echo "    CLI=$CLI"
echo "    WORKDIR=$WORKDIR"

# --- 1. import (gateway JSONL → Vortex) ------------------------------------
section "traj import (gateway JSONL)"

IMPORT_OUT="$(run_cli traj import "$STORAGE" \
  --provider gateway \
  --gateway-input "$FIXTURE" \
  --session-id "$SESSION_ID" \
  --agent-id "$AGENT_ID")"
grep -qE 'traj-it-agent|traj-it-session|import' <<<"$IMPORT_OUT" \
  || { echo "$IMPORT_OUT"; die "import output unexpected"; }
pass "traj import wrote session"

[[ -f "$STORAGE/$AGENT_ID/$SESSION_ID/events.vortex" ]] \
  || die "missing events.vortex after import"
pass "events.vortex exists"

# --- 2. stats (pre-judge) ---------------------------------------------------
section "traj stats (before judge)"

run_cli traj stats "$STORAGE" \
  --agent-id "$AGENT_ID" \
  --session-id "$SESSION_ID" \
  --storage-format vortex >"$STATS_TOML"
ROW_COUNT="$(parse_toml_field "$STATS_TOML" row_count)"
[[ -n "$ROW_COUNT" && "$ROW_COUNT" -gt 0 ]] || die "expected row_count > 0, got '$ROW_COUNT'"
pass "stats row_count=$ROW_COUNT"

run_cli traj stats "$STORAGE" --output plain >"$STATS_PLAIN"
if grep -q 'judgment_count:' "$STATS_PLAIN"; then
  PRE_JUDGE="$(parse_plain_field judgment_count "$STATS_PLAIN")"
  [[ "${PRE_JUDGE:-0}" == "0" ]] || die "expected judgment_count=0 before judge, got $PRE_JUDGE"
  pass "stats judgment_count=0 (pre-judge)"
else
  pass "stats has no judge sidecar yet (pre-judge)"
fi

# --- 3. judge (non-interactive fixed score) ---------------------------------
section "traj judge (--score, story scope)"

JUDGE_OUT="$(run_cli traj judge "$STORAGE" \
  --agent-id "$AGENT_ID" \
  --session-id "$SESSION_ID" \
  --scope story \
  --score 100 \
  --force)"
grep -qE 'judged_calls\s*=\s*[1-9]|judged_calls: [1-9]' <<<"$JUDGE_OUT" \
  || { echo "$JUDGE_OUT"; die "judge did not score any units"; }
pass "traj judge wrote sidecar"

[[ -f "$STORAGE/$AGENT_ID/$SESSION_ID/layers/judge_default.vortex" ]] \
  || die "missing judge_default.vortex"
pass "judge sidecar file exists"

# --- 4. stats + judge-stats (post-judge) ------------------------------------
section "traj stats (after judge)"

run_cli traj stats "$STORAGE" \
  --agent-id "$AGENT_ID" \
  --session-id "$SESSION_ID" \
  --storage-format vortex >"$STATS_TOML"
grep -qE 'judgment_count\s*=\s*[1-9]|judgment_count: [1-9]' "$STATS_TOML" \
  || die "stats missing judgment_count after judge"
pass "stats shows judgment_count after judge"

run_cli traj stats "$STORAGE" --output plain >"$STATS_PLAIN"
POST_JUDGE="$(parse_plain_field judgment_count "$STATS_PLAIN")"
[[ -n "$POST_JUDGE" && "$POST_JUDGE" -ge 1 ]] || die "plain stats judgment_count=$POST_JUDGE"
pass "plain stats judgment_count=$POST_JUDGE"

section "traj judge-stats"

run_cli traj judge-stats "$STORAGE" >"$JUDGE_STATS_TOML"
JUDGED_SESSIONS="$(parse_toml_field "$JUDGE_STATS_TOML" judged_session_count)"
JUDGMENT_COUNT="$(parse_toml_field "$JUDGE_STATS_TOML" judgment_count)"
[[ "$JUDGED_SESSIONS" == "1" ]] || die "judged_session_count=$JUDGED_SESSIONS (want 1)"
[[ -n "$JUDGMENT_COUNT" && "$JUDGMENT_COUNT" -ge 1 ]] || die "judgment_count=$JUDGMENT_COUNT"
pass "judge-stats judged_session_count=1 judgment_count=$JUDGMENT_COUNT"

section "done"
echo "==> traj E2E integration OK"

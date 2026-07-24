#!/usr/bin/env bash
# Search + Trajectory CLI 全链路集成：generate → search → trajectory。
#
#   ./scripts/integration/search_traj_e2e.sh
#   QUICK=1 ./scripts/integration/search_traj_e2e.sh
#   just smoke / just integration
#
# 环境变量：
#   PERSISTING_CLI / PERSISTING_ENGINE_LIB / SKIP_BUILD / PERSISTING_BUILD_PROFILE
#   CLI_SOURCE=target|path   (default target)
#   QUICK=1                  smoke 规模
#   REORDER=1                search index reorder
#   SKIP_REBUILD=1           跳过 search index rebuild
#   SEARCH_ROWS / TRAJ_ROWS / SEED / EMBED_DIM / NUM_PARTITIONS / NPROBES / REPLAY_LIMIT

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/integration/_common.sh
source "$REPO_ROOT/scripts/integration/_common.sh"

GEN_PY="$REPO_ROOT/scripts/generate_benchmark_data.py"
PROFILE="${PERSISTING_BUILD_PROFILE:-debug}"
CLI_SOURCE="${CLI_SOURCE:-target}"
QUICK="${QUICK:-0}"
REORDER="${REORDER:-0}"
SKIP_REBUILD="${SKIP_REBUILD:-0}"
SEED="${SEED:-42}"
EMBED_DIM="${EMBED_DIM:-32}"
REPLAY_LIMIT="${REPLAY_LIMIT:-50}"

if [[ "$QUICK" == "1" ]]; then
  SEARCH_ROWS="${SEARCH_ROWS:-70}"
  TRAJ_ROWS="${TRAJ_ROWS:-25}"
  NUM_PARTITIONS="${NUM_PARTITIONS:-2}"
  NPROBES="${NPROBES:-2}"
else
  SEARCH_ROWS="${SEARCH_ROWS:-100000}"
  TRAJ_ROWS="${TRAJ_ROWS:-100000}"
  NUM_PARTITIONS="${NUM_PARTITIONS:-128}"
  NPROBES="${NPROBES:-32}"
fi

die() { capture_die "$@"; }

command -v python3 >/dev/null || die "need python3"
[[ -f "$GEN_PY" ]] || die "missing $GEN_PY"

# Resolve CLI / engine (reuse capture helpers when building from target)
if [[ "$CLI_SOURCE" == "target" ]]; then
  if [[ "${SKIP_BUILD:-0}" != "1" && "$PROFILE" == "release" ]]; then
    echo "==> cargo build -p persisting-cli -p persisting-engine --release"
    (cd "$REPO_ROOT" && cargo build -p persisting-cli -p persisting-engine --release)
    export SKIP_BUILD=1
  fi
  capture_resolve_binaries "${SKIP_BUILD:-0}" 1
  export PERSISTING_CLI="$CLI"
elif [[ -n "${PERSISTING_CLI:-}" ]]; then
  CLI="$PERSISTING_CLI"
  [[ -x "$CLI" ]] || die "not executable: $CLI"
else
  command -v persisting >/dev/null || die "CLI_SOURCE=$CLI_SOURCE needs persisting on PATH or PERSISTING_CLI=..."
  CLI="$(command -v persisting)"
  export PERSISTING_CLI="$CLI"
fi

echo "==> CLI: $CLI"
if [[ -n "${PERSISTING_ENGINE_LIB:-}" ]]; then
  echo "==> PERSISTING_ENGINE_LIB=$PERSISTING_ENGINE_LIB"
else
  echo "==> PERSISTING_ENGINE_LIB unset (CLI default lookup)"
fi
echo "==> params: cli_source=$CLI_SOURCE profile=$PROFILE reorder=$REORDER quick=$QUICK rows=$SEARCH_ROWS/$TRAJ_ROWS seed=$SEED"

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/persisting-it.XXXXXX")"
trap 'rm -rf "$WORKDIR"' EXIT

DATASET="$WORKDIR/search_ds"
STORAGE="$WORKDIR/traj_root"
TRAJ_AGENT="bench_agent"
TRAJ_SESSION="cli_bench_run"
SEARCH_JSONL="$WORKDIR/docs.jsonl"
TRAJ_INPUT="$WORKDIR/traj_records.toml"

if [[ -t 1 ]]; then
  _b=$'\033[1m'; _c=$'\033[36m'; _m=$'\033[35m'; _y=$'\033[33m'; _n=$'\033[0m'
else
  _b=; _c=; _m=; _y=; _n=
fi
it_rule() { printf '%b%s%b\n' "$_b$_c" "────────────────────────────────────────────────────────" "$_n"; }
it_sh() {
  it_rule; printf '%b$ ' "$_b$_m"; printf '%q ' "$@"; printf '%b\n' "$_n"; it_rule; "$@"
}
it_run_cli_out() {
  it_rule; printf '%b$ ' "$_b$_m"; printf '%q' "$CLI"; printf ' %q' "$@"; printf '%b\n' "$_n"
  it_rule; out=$("$CLI" "$@")
  printf '%b[stdout 捕获]%b\n' "$_b$_y" "$_n"; printf '%s\n\n' "$out"
}
timer_start() { export TIMER_START="$(python3 -c "import time; print(time.perf_counter())")"; }
timer_end() {
  python3 -c "import os,time,sys; t0=float(os.environ['TIMER_START']); print(f\"[timer] {sys.argv[1]}: {time.perf_counter()-t0:.3f}s\")" "$1"
}
assert_ok() {
  grep -qE 'status:\s*"ok"|status\s*=\s*"ok"' <<<"$1" || { echo "--- bad response ---"; echo "$1"; exit 1; }
}

export RUNNER_T0="$(python3 -c "import time; print(time.perf_counter())")"

echo "==> generate JSONL (seed=$SEED)"
timer_start
it_sh python3 "$GEN_PY" --seed "$SEED" --search-rows "$SEARCH_ROWS" --traj-rows "$TRAJ_ROWS" \
  --search-out "$SEARCH_JSONL" --traj-out "$TRAJ_INPUT"
timer_end "generate_benchmark_data.py"

echo "==> search create ($SEARCH_ROWS rows)"
timer_start
it_run_cli_out search create "$DATASET" --input "$SEARCH_JSONL" --format jsonl --embedding-dim "$EMBED_DIM"
assert_ok "$out"; timer_end "search create"

echo "==> search index list (pre-build)"
timer_start
it_run_cli_out search index list "$DATASET"
assert_ok "$out"; timer_end "search index list (pre-build)"

echo "==> search index build"
timer_start
it_run_cli_out search index build "$DATASET" \
  --vector-column embedding --text-column text --metric cosine \
  --num-partitions "$NUM_PARTITIONS" --ivf-max-iters 12 \
  --pq-num-sub-vectors 8 --pq-num-bits 4 --pq-max-iters 12 \
  --pq-sample-rate 4 --pq-kmeans-redos 1
assert_ok "$out"; timer_end "search index build"

echo "==> search index list (post-build)"
timer_start
it_run_cli_out search index list "$DATASET"
assert_ok "$out"
grep -q 'persisting_ivf_pq' <<<"$out" || die "missing persisting_ivf_pq"
grep -q 'persisting_fts' <<<"$out" || die "missing persisting_fts"
timer_end "search index list (post-build)"

echo "==> search query x3"
timer_start
it_run_cli_out search query "$DATASET" "integration alpha gamma" --mode vector --k 5 --embedding-dim "$EMBED_DIM" --nprobes "$NPROBES"
assert_ok "$out"
it_run_cli_out search query "$DATASET" "integration" --mode fts --k 8 --embedding-dim "$EMBED_DIM"
assert_ok "$out"
it_run_cli_out search query "$DATASET" "keyword beta" --mode hybrid --k 5 --embedding-dim "$EMBED_DIM"
assert_ok "$out"
timer_end "search query (3 modes)"

if [[ "$REORDER" == "1" ]]; then
  echo "==> search index reorder"
  timer_start
  it_run_cli_out search index reorder "$DATASET" persisting_ivf_pq --in-place
  assert_ok "$out"; timer_end "search index reorder"
fi

if [[ "$SKIP_REBUILD" != "1" ]]; then
  echo "==> search index rebuild"
  timer_start
  it_run_cli_out search index rebuild "$DATASET" --no-retrain
  assert_ok "$out"; timer_end "search index rebuild"
else
  echo "==> skip search index rebuild"
fi

echo "==> trajectory add ($TRAJ_ROWS rows)"
timer_start
it_run_cli_out trajectory add "$STORAGE" --agent-id "$TRAJ_AGENT" --session-id "$TRAJ_SESSION" --format toml --input "$TRAJ_INPUT"
assert_ok "$out"; timer_end "trajectory add"

echo "==> trajectory stats"
timer_start
it_run_cli_out trajectory stats "$STORAGE" --agent-id "$TRAJ_AGENT" --session-id "$TRAJ_SESSION"
assert_ok "$out"
grep -Fq "$TRAJ_SESSION" <<<"$out" || die "stats missing session_id"
grep -Fq "$TRAJ_AGENT" <<<"$out" || die "stats missing agent_id"
timer_end "trajectory stats"

echo "==> trajectory replay"
timer_start
it_run_cli_out trajectory replay "$STORAGE" --agent-id "$TRAJ_AGENT" --session-id "$TRAJ_SESSION" --offset 2 --limit "$REPLAY_LIMIT"
assert_ok "$out"
grep -qE '\[\[records\]\]|^records\s*=' <<<"$out" || die "replay missing TOML records"
timer_end "trajectory replay"

it_sh python3 -c "import os,time; t0=float(os.environ['RUNNER_T0']); print(f'[timer] TOTAL wall: {time.perf_counter()-t0:.3f}s')"
echo "==> search/traj integration OK"

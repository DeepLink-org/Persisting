#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
PPILOT="${PPILOT_BIN:-$ROOT/target/debug/ppilot}"
[[ -x "$PPILOT" ]] || (cd "$ROOT" && cargo build -q -p persisting-ppilot --features cli --bin ppilot)

INPUT="$ROOT/crates/persisting-pchronicle/tests/fixtures/atif"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/ppilot-analysis.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
"$PPILOT" analysis "$INPUT" --sql-file "$DIR/analysis.sql" \
  --parallelism 3 --fmt json --output "$WORK/output"

ROWS="$(jq 'length' "$WORK/output/results.json")"
STEPS="$(jq 'map(.steps) | add' "$WORK/output/results.json")"
SHARDS="$(jq -r '.shard_count' "$WORK/output/analysis-report.json")"
SHARD_SIZES="$(jq -r '[.shards[].trajectory_ids | length] | sort | join(",")' "$WORK/output/analysis-report.json")"

printf 'RESULT result_rows=%s total_steps=%s shards=%s shard_sizes=%s\n' \
  "$ROWS" "$STEPS" "$SHARDS" "$SHARD_SIZES"
[[ "$ROWS" == "8" && "$STEPS" == "118" && "$SHARDS" == "3" && "$SHARD_SIZES" == "2,3,3" ]]
echo 'CONCLUSION pPilot balanced eight trajectories across three shards and merged every per-session SQL result'

#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
SCALE="${SCALE:-64}"
ITERATIONS="${ITERATIONS:-20}"
PROFILE="${PROFILE:-debug}"
[[ "$PROFILE" == "debug" || "$PROFILE" == "release" ]]

if [[ "$PROFILE" == "release" ]]; then
  OUTPUT="$(cd "$ROOT" && PCHRONICLE_BENCH_SCALE="$SCALE" PCHRONICLE_BENCH_ITERS="$ITERATIONS" \
    cargo run -q --release -p persisting-pchronicle --example compare_analysis_speed)"
else
  OUTPUT="$(cd "$ROOT" && PCHRONICLE_BENCH_SCALE="$SCALE" PCHRONICLE_BENCH_ITERS="$ITERATIONS" \
    cargo run -q -p persisting-pchronicle --example compare_analysis_speed)"
fi
printf '%s\n' "$OUTPUT"

MEASUREMENTS="$(printf '%s\n' "$OUTPUT" | grep -c '^RESULT benchmark=')"
printf 'RESULT build_profile=%s measurements=%s\n' "$PROFILE" "$MEASUREMENTS"
[[ "$MEASUREMENTS" == "2" ]]
echo 'CONCLUSION two result-equivalent queries produced quantitative Lance and ATIF throughput measurements'

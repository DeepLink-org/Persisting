#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
PPILOT="${PPILOT_BIN:-$ROOT/target/debug/ppilot}"
[[ -x "$PPILOT" ]] || (cd "$ROOT" && cargo build -q -p persisting-ppilot --features cli --bin ppilot)

INPUT="$ROOT/crates/persisting-pchronicle/tests/fixtures/atif"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/ppilot-process.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
"$PPILOT" process "$INPUT" --script "$DIR/metrics.py" \
  --mappers 4 --output "$WORK/output"

TRAJECTORIES="$(jq -r '.trajectories' "$WORK/output/results.json")"
STEPS="$(jq -r '.steps' "$WORK/output/results.json")"
MAPPERS="$(jq -r '.mappers' "$WORK/output/results.json")"
PARTIALS="$(jq -r '.partials | length' "$WORK/output/process-report.json")"

printf 'RESULT trajectories=%s steps=%s mappers=%s partials=%s\n' \
  "$TRAJECTORIES" "$STEPS" "$MAPPERS" "$PARTIALS"
[[ "$TRAJECTORIES" == "8" && "$STEPS" == "118" && "$MAPPERS" == "4" && "$PARTIALS" == "4" ]]
echo 'CONCLUSION pPilot mapped four disjoint ATIF shards and reduced them to one globally checked result'

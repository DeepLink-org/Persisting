#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
PPILOT="${PPILOT_BIN:-$ROOT/target/debug/ppilot}"
[[ -x "$PPILOT" ]] || (cd "$ROOT" && cargo build -q -p persisting-ppilot --features cli --bin ppilot)

WORK="$(mktemp -d "${TMPDIR:-/tmp}/ppilot-produce.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
"$PPILOT" produce "$DIR/production.py" --output "$WORK/runs" \
  --parallelism 2 --batch-id example-batch --no-capture >"$WORK/report.json"

COMPLETED="$(jq -r '.completed' "$WORK/report.json")"
FAILED="$(jq -r '.failed' "$WORK/report.json")"
BUNDLES="$(find "$WORK/runs" -name run-bundle.json -type f | wc -l | tr -d ' ')"
LINEAGED="$(find "$WORK/runs" -name run-bundle.json -type f -exec \
  jq -r 'select(.orchestration["ppilot.batch_id"] == "example-batch") | 1' {} + | wc -l | tr -d ' ')"

printf 'RESULT completed=%s failed=%s bundles=%s lineaged_bundles=%s\n' \
  "$COMPLETED" "$FAILED" "$BUNDLES" "$LINEAGED"
[[ "$COMPLETED" == "3" && "$FAILED" == "0" && "$BUNDLES" == "3" && "$LINEAGED" == "3" ]]
echo 'CONCLUSION pPilot produced three independent pVisor Runs with reviewable batch lineage'

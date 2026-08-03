#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
PPILOT="${PPILOT_BIN:-$ROOT/target/debug/ppilot}"
[[ -x "$PPILOT" ]] || (cd "$ROOT" && cargo build -q -p persisting-ppilot --features cli --bin ppilot)

WORK="$(mktemp -d "${TMPDIR:-/tmp}/ppilot-run.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
"$PPILOT" run "$DIR/plan.py" --workers 2 --per-worker 2 \
  --sink "$WORK/sink" --results quiet 2>"$WORK/ppilot.log"

COMPLETED="$(wc -l < "$WORK/sink/ready.ndjson" | tr -d ' ')"
FAILED=0
[[ ! -f "$WORK/sink/failures.ndjson" ]] || FAILED="$(wc -l < "$WORK/sink/failures.ndjson" | tr -d ' ')"
SQUARE_SUM="$(jq -s 'map(.value.square) | add' "$WORK/sink/ready.ndjson")"
WORKER_SLOTS="$(jq -rs 'map(.worker) | unique | length' "$WORK/sink/ready.ndjson")"

printf 'RESULT completed=%s failed=%s square_sum=%s worker_slots=%s\n' \
  "$COMPLETED" "$FAILED" "$SQUARE_SUM" "$WORKER_SLOTS"
[[ "$COMPLETED" == "6" && "$FAILED" == "0" && "$SQUARE_SUM" == "55" && "$WORKER_SLOTS" -ge 2 ]]
echo 'CONCLUSION pPilot executed six planned tasks across multiple slots and recorded every terminal result'

#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
PPILOT="${PPILOT_BIN:-$ROOT/target/debug/ppilot}"
[[ -x "$PPILOT" ]] || (cd "$ROOT" && cargo build -q -p persisting-ppilot --features cli --bin ppilot)

FIXTURES="$ROOT/crates/persisting-pchronicle/tests/fixtures/atif"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/pchronicle-analysis.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
python3 "$DIR/../common/generate_atif.py" "$FIXTURES" "$WORK/trajectories.ndjson" 4
(cd "$ROOT" && cargo run -q -p persisting-pchronicle --example import_atif_jsonl -- \
  "$WORK/trajectories.ndjson" "$WORK/lance")

SQL='SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source'
ATIF_RESULT="$($PPILOT query "$WORK/trajectories.ndjson" --source atif --sql "$SQL")"
LANCE_RESULT="$($PPILOT query "$WORK/lance" --source lance --sql "$SQL")"
IDENTICAL=0
[[ "$ATIF_RESULT" == "$LANCE_RESULT" ]] && IDENTICAL=1
ROWS="$(printf '%s\n' "$LANCE_RESULT" | jq -s 'length')"
TOTAL_STEPS="$(printf '%s\n' "$LANCE_RESULT" | jq -s 'map(.steps) | add')"

printf 'RESULT identical=%s result_rows=%s total_steps=%s\n' "$IDENTICAL" "$ROWS" "$TOTAL_STEPS"
[[ "$IDENTICAL" == "1" && "$ROWS" == "3" ]]
echo 'CONCLUSION one read-only SQL statement returned identical analysis for Lance and ATIF trajectories'

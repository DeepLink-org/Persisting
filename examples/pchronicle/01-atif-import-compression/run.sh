#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
FIXTURES="$ROOT/crates/persisting-pchronicle/tests/fixtures/atif"
REPLICAS="${REPLICAS:-64}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/pchronicle-compression.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

python3 "$DIR/../common/generate_atif.py" "$FIXTURES" "$WORK/trajectories.ndjson" "$REPLICAS"
(cd "$ROOT" && cargo run -q -p persisting-pchronicle --example import_atif_jsonl -- \
  "$WORK/trajectories.ndjson" "$WORK/lance")

ATIF_BYTES="$(wc -c < "$WORK/trajectories.ndjson" | tr -d ' ')"
TRAJECTORIES="$(wc -l < "$WORK/trajectories.ndjson" | tr -d ' ')"
LANCE_BYTES="$(find "$WORK/lance" -type f -exec cat {} + | wc -c | tr -d ' ')"
RATIO="$(awk -v raw="$ATIF_BYTES" -v stored="$LANCE_BYTES" 'BEGIN {printf "%.3f", stored/raw}')"
SAVING="$(awk -v raw="$ATIF_BYTES" -v stored="$LANCE_BYTES" 'BEGIN {printf "%.1f", 100*(1-stored/raw)}')"

printf 'RESULT trajectories=%s atif_bytes=%s lance_bytes=%s stored_over_raw=%s saving_percent=%s\n' \
  "$TRAJECTORIES" "$ATIF_BYTES" "$LANCE_BYTES" "$RATIO" "$SAVING"
[[ "$LANCE_BYTES" -lt "$ATIF_BYTES" ]]
echo 'CONCLUSION this deterministic ATIF corpus occupied fewer physical bytes after pChronicle Lance import'

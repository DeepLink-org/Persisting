#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$example_dir/../../.." && pwd)"
source "$example_dir/../common.sh"
pchronicle="${PCHRONICLE_BIN:-$repo_root/target/release/pchronicle}"
input="$repo_root/examples/data/atif/support-ticket.json"

pchronicle_example_init "$example_dir"
run_dir="$PCHRONICLE_EXAMPLE_RUN_DIR"

imported="$(pchronicle_capture 01-import "$pchronicle" import \
  --from "$input" --output "$run_dir/atif" --format atif)"
pchronicle_capture 02-export "$pchronicle" export --from "$run_dir/atif" \
  --output "$run_dir/restored.json" --format atif --strict >/dev/null

jq --sort-keys . "$input" >"$run_dir/input.normalized.json"
jq --sort-keys . "$run_dir/restored.json" >"$run_dir/restored.normalized.json"
cmp "$run_dir/input.normalized.json" "$run_dir/restored.normalized.json"

input_bytes="$(jq -r '.input_bytes' <<<"$imported")"
pchronicle_report_start "Strict format roundtrip"
pchronicle_report_item "Input" "ATIF: 1 trajectory, $(pchronicle_human_bytes "$input_bytes")"
pchronicle_report_item "Export" "strict ATIF mode"
pchronicle_report_item "Conclusion" "canonical input and restored JSON are byte-identical"
pchronicle_report_finish \
  "strict ATIF roundtrip preserved the canonical JSON exactly"

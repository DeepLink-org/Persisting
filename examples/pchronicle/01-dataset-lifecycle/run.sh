#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$example_dir/../../.." && pwd)"
source "$example_dir/../common.sh"
pchronicle="${PCHRONICLE_BIN:-$repo_root/target/release/pchronicle}"
input="$repo_root/examples/data/atif/support-ticket.json"

pchronicle_example_init "$example_dir"
run_dir="$PCHRONICLE_EXAMPLE_RUN_DIR"
settings="$run_dir/settings.toml"
warehouse="$run_dir/warehouse"

pchronicle_capture 01-default "$pchronicle" --config "$settings" \
  default set "$warehouse" >/dev/null
imported="$(pchronicle_capture 02-import "$pchronicle" --config "$settings" \
  import --from "$input" --to "$warehouse/imported" --input-format atif)"
dataset_uri="$(jq -er '.dataset_uri' <<<"$imported")"

sources="$(pchronicle_capture 03-ls "$pchronicle" --config "$settings" \
  ls "$dataset_uri" --physical --format json)"
status="$(pchronicle_capture 04-status "$pchronicle" --config "$settings" \
  status "$dataset_uri" --format json)"
query_result="$(pchronicle_capture 05-query "$pchronicle" --config "$settings" query "$dataset_uri" \
  --sql 'SELECT session_id, COUNT(*) AS steps FROM dataset.steps GROUP BY session_id' \
  --format jsonl)"
found="$(pchronicle_capture 06-find "$pchronicle" --config "$settings" find "$dataset_uri" \
  --session-id support-001 --step-id 1 --format json)"

restored="$run_dir/restored.atif.json"
pchronicle_capture 07-export "$pchronicle" --config "$settings" export \
  --from "$dataset_uri" --to "$restored" --output-format atif --strict >/dev/null

jq -e '.sources | length == 1 and .[0].status == "ready"' \
  <<<"$sources" >/dev/null
jq -e '.status == "ready"
  and .counts.runs == 1
  and .counts.trajectories == 1
  and .counts.steps == 3
  and .counts.tool_calls == 1' <<<"$status" >/dev/null
jq -e '.session_id == "support-001" and .steps == 3' \
  <<<"$query_result" >/dev/null
jq -e '.truncated == false
  and (.matches | length) == 1
  and .matches[0].step_id == 1' <<<"$found" >/dev/null

jq --sort-keys . "$input" >"$run_dir/input.normalized.json"
jq --sort-keys . "$restored" >"$run_dir/restored.normalized.json"
cmp "$run_dir/input.normalized.json" "$run_dir/restored.normalized.json"

input_bytes="$(jq -r '.input_bytes' <<<"$imported")"
step_source="$(jq -r '.matches[0].step_source' <<<"$found")"
step_kind="$(jq -r '.matches[0].effective_kind' <<<"$found")"

pchronicle_report_start "Dataset lifecycle"
pchronicle_report_item "Dataset" \
  "support-ticket (ATIF): 1 trajectory, $(pchronicle_human_bytes "$input_bytes") input"
pchronicle_report_item "Status" "ready: 1 run, 3 steps, 1 tool call"
pchronicle_report_item "SQL" "support-001 -> 3 steps"
pchronicle_report_item "Lookup" "support-001 / step 1 / $step_source / $step_kind"
pchronicle_report_item "Roundtrip" "strict ATIF export matches the source"
pchronicle_report_finish \
  "import, inspect, query, find, and strict export completed successfully"

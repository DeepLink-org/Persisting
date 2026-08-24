#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$example_dir/../../.." && pwd)"
source "$example_dir/../common.sh"
pchronicle="${PCHRONICLE_BIN:-$repo_root/target/release/pchronicle}"
data="$repo_root/examples/data"

pchronicle_example_init "$example_dir"

mounts=(
  --mount "atif=$data/atif"
  --mount "actf=$data/actf"
  --mount "openai=$data/openai-messages"
)

counts="$(pchronicle_capture 01-counts "$pchronicle" query "${mounts[@]}" \
  --sql 'SELECT
     (SELECT COUNT(*) FROM atif.runs) AS atif_runs,
     (SELECT COUNT(*) FROM actf.runs) AS actf_runs,
     (SELECT COUNT(*) FROM openai.runs) AS openai_runs' \
  --format jsonl)"

sessions="$(pchronicle_capture 02-sessions "$pchronicle" query "${mounts[@]}" \
  --sql "SELECT 'atif' AS dataset, session_id FROM atif.runs
   UNION ALL
   SELECT 'actf' AS dataset, session_id FROM actf.runs
   UNION ALL
   SELECT 'openai' AS dataset, session_id FROM openai.runs
   ORDER BY dataset, session_id" \
  --format jsonl)"

jq -e '.atif_runs == 1 and .actf_runs == 1 and .openai_runs == 2' \
  <<<"$counts" >/dev/null
jq -s -e '. == [
  {"dataset":"actf","session_id":"example-code-repair"},
  {"dataset":"atif","session_id":"support-001"},
  {"dataset":"openai","session_id":"training-001"},
  {"dataset":"openai","session_id":"training-002"}
]' <<<"$sessions" >/dev/null

session_summary="$(jq -sr \
  'map("\(.dataset):\(.session_id)") | join(", ")' <<<"$sessions")"

pchronicle_report_start "Cross-Dataset SQL"
pchronicle_report_item "Mounts" "ATIF=1 run, ACTF=1 run, OpenAI=2 runs"
pchronicle_report_item "Sessions" "$session_summary"
pchronicle_report_item "Conclusion" "two SQL statements query three normalized exchange formats"
pchronicle_report_finish "cross-Dataset SQL addressed three independent named mounts"

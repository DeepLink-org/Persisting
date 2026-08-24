#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$example_dir/../../.." && pwd)"
source "$example_dir/../common.sh"
pchronicle="${PCHRONICLE_BIN:-$repo_root/target/release/pchronicle}"
data="$repo_root/examples/data"

pchronicle_example_init "$example_dir"

openai="$(pchronicle_capture 01-openai "$pchronicle" query "$data/openai-messages" \
  --sql "SELECT session_id, COUNT(*) AS steps FROM dataset.steps GROUP BY session_id ORDER BY session_id" \
  --format jsonl)"

actf="$(pchronicle_capture 02-actf "$pchronicle" query "$data/actf" \
  --sql "SELECT session_id, agent_id FROM dataset.runs ORDER BY session_id" \
  --format jsonl)"

jq -s -e '. == [
  {"session_id":"training-001","steps":2},
  {"session_id":"training-002","steps":2}
]' <<<"$openai" >/dev/null
jq -s -e '. == [
  {"session_id":"example-code-repair","agent_id":"actf-agent"}
]' <<<"$actf" >/dev/null

pchronicle_report_start "Direct exchange-format queries"
pchronicle_report_item "OpenAI" "2 sessions, 4 steps (training-001 and training-002)"
pchronicle_report_item "ACTF" "example-code-repair -> actf-agent"
pchronicle_report_item "Conclusion" "both formats are queryable without a prior import"
pchronicle_report_finish \
  "OpenAI and ACTF sources returned the expected normalized rows"

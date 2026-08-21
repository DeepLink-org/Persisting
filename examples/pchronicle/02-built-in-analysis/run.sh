#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$example_dir/../../.." && pwd)"
source "$example_dir/../common.sh"
pchronicle="${PCHRONICLE_BIN:-$repo_root/target/release/pchronicle}"
data="$repo_root/examples/data"

pchronicle_example_init "$example_dir"

overview="$(pchronicle_capture 01-overview "$pchronicle" \
  analysis overview "$data" --format jsonl)"
agents="$(pchronicle_capture 02-agents "$pchronicle" \
  analysis agents "$data" --format jsonl)"
models="$(pchronicle_capture 03-models "$pchronicle" \
  analysis models "$data" --format jsonl)"
tools="$(pchronicle_capture 04-tools "$pchronicle" \
  analysis tools "$data" --format jsonl)"
found="$(pchronicle_capture 05-find "$pchronicle" \
  find "$data" --session-id support-001 \
  --step-id 1 --format json)"

jq -e '.sources == 3
  and .ready_sources == 3
  and .trajectories == 4
  and .steps == 9
  and .tool_calls == 2
  and .agents == 3
  and .models == 1' <<<"$overview" >/dev/null
jq -s -e '. == [
  {"agent_id":"example-model","agent_name":"example-model","agent_version":"","trajectories":2,"sources":1,"steps":4,"user_steps":2,"agent_steps":2,"tool_calls":0},
  {"agent_id":"actf-agent","agent_name":"ACTF Agent","agent_version":"","trajectories":1,"sources":1,"steps":2,"user_steps":0,"agent_steps":2,"tool_calls":1},
  {"agent_id":"support-agent","agent_name":"support-agent","agent_version":"1.0.0","trajectories":1,"sources":1,"steps":3,"user_steps":1,"agent_steps":2,"tool_calls":1}
]' <<<"$agents" >/dev/null
jq -s -e '. == [
  {"model":"example-model","declared_trajectories":3,"observed_steps":4}
]' <<<"$models" >/dev/null
jq -s -e '. == [
  {"function_name":"Bash","calls":1,"trajectories":1,"sources":1,"duration_samples":1,"total_duration_ms":25},
  {"function_name":"deployment_status","calls":1,"trajectories":1,"sources":1,"duration_samples":0,"total_duration_ms":0}
]' <<<"$tools" >/dev/null
jq -e '.truncated == false
  and (.matches | length) == 1
  and .matches[0].source_path == "atif/support-ticket.json"
  and .matches[0].step_id == 1' <<<"$found" >/dev/null

agent_summary="$(jq -sr \
  'map("\(.agent_id)=\(.trajectories)") | join(", ")' <<<"$agents")"
tool_summary="$(jq -sr \
  'map("\(.function_name)=\(.calls)") | join(", ")' <<<"$tools")"

pchronicle_report_start "Built-in analysis"
pchronicle_report_item "Corpus" "3 sources, 4 trajectories, 9 steps"
pchronicle_report_item "Agents" "3 agents; trajectories: $agent_summary"
pchronicle_report_item "Models" "example-model: 3 declared trajectories, 4 observed steps"
pchronicle_report_item "Tools" "2 calls: $tool_summary"
pchronicle_report_item "Lookup" "atif/support-ticket.json / support-001 / step 1"
pchronicle_report_finish \
  "built-in analyses and source-local lookup returned the expected facts"

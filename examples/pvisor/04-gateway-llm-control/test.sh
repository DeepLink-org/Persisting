#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
work_root="${WORK_ROOT:-$example_dir/.work}"
work_dir="$work_root/gateway-llm-control"
export WORK_ROOT="$work_root"

bash "$example_dir/run.sh"

run_dir="$(find "$work_dir/runs" -mindepth 1 -maxdepth 1 -type d -name 'run-*' -print -quit)"
test -n "$run_dir"
upstream_posts="$(grep -c 'POST /v1/chat/completions' "$work_dir/mock.log")"
agentic_blocks="$(grep -E -h -c '<!-- persisting:block:(user|agent) ' \
  "$run_dir"/gateway-example/*/*.md | awk '{ total += $1 } END { print total + 0 }')"
test "$upstream_posts" = 2
test "$agentic_blocks" = 4
jq -e '
  .run.state == "completed" and
  .network.intercepted.requests_seen == 2 and
  .network.intercepted.sink_requests == 2 and
  .network.intercepted.failures == 0
' "$run_dir/run-bundle.json" >/dev/null

echo 'RESULT example=gateway-llm-control upstream_posts=2 sink_requests=2 agentic_blocks=4 failures=0'

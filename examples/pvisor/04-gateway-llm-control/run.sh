#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$example_dir/../common.sh"
pvisor_example_init "$example_dir" gateway-llm-control
command -v jq >/dev/null

# Start a local OpenAI-compatible endpoint for the example agent.
pvisor_example_reset
ports="$(pvisor_free_ports 3)"
read -r mock_port proxy_port admin_port <<<"$ports"
sed \
  -e "s|^\[run\]|[run]\ncommand = [\"${PYTHON_BIN:-python3}\", \"$example_dir/agent.py\"]|" \
  -e "s/127.0.0.1:19080/127.0.0.1:$mock_port/" \
  -e "s/127.0.0.1:19081/127.0.0.1:$proxy_port/" \
  -e "s/127.0.0.1:19082/127.0.0.1:$admin_port/" \
  run.toml >"$work_dir/run.toml"

export PERSISTING_RUN_HOME="$work_dir/runs"
PYTHONDONTWRITEBYTECODE=1 MOCK_LLM_PORT="$mock_port" \
  python3 mock_llm.py >"$work_dir/mock.log" 2>&1 &
mock_pid=$!
trap 'kill "$mock_pid" 2>/dev/null || true; wait "$mock_pid" 2>/dev/null || true' EXIT
pvisor_wait_tcp "$mock_port"

# Run the agent through pVisor's configured Gateway.
"$pvisor_bin" run --spec "$work_dir/run.toml" --stdio capture
run_dir="$(find "$PERSISTING_RUN_HOME" -mindepth 1 -maxdepth 1 -type d -name 'run-*' -print -quit)"
test -n "$run_dir"

# Print the upstream requests, Gateway counters, and captured conversation.
echo 'Mock LLM requests:'
cat "$work_dir/mock.log"

echo 'Gateway counters:'
jq '.network.intercepted' "$run_dir/run-bundle.json"

echo 'Generated AgenticMD:'
cat "$run_dir"/gateway-example/*/*.md

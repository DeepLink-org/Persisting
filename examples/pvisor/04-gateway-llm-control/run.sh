#!/usr/bin/env bash
set -euo pipefail

# Use the pVisor binary built from this checkout.
export PATH="../../../target/release:$PATH"

# Start a local OpenAI-compatible endpoint for the example agent.
rm -rf .work
mkdir .work
export PERSISTING_RUN_HOME="$PWD/.work/runs"
PYTHONDONTWRITEBYTECODE=1 MOCK_LLM_PORT=19080 python3 mock_llm.py >.work/mock.log 2>&1 &
mock_pid=$!
trap 'kill "$mock_pid" 2>/dev/null || true; wait "$mock_pid" 2>/dev/null || true' EXIT
sleep 0.3

# Run the agent through pVisor's configured Gateway.
pvisor run --config run.toml --stdio capture -- \
  env PYTHONDONTWRITEBYTECODE=1 python3 agent.py
run_dir="$(find "$PERSISTING_RUN_HOME" -mindepth 1 -maxdepth 1 -type d -name 'run-*' | head -n 1)"

# Print the upstream requests, Gateway counters, and captured conversation.
echo 'Mock LLM requests:'
cat .work/mock.log

echo 'Gateway counters:'
jq '.network.intercepted' "$run_dir/run-bundle.json"

echo 'Generated AgenticMD:'
cat "$run_dir"/gateway-example/*/*.md

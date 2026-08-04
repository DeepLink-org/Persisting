#!/usr/bin/env bash
set -euo pipefail

# Use the pPilot binary built from this checkout.
export PATH="../../../target/debug:$PATH"

# Remove results from the previous run.
rm -rf .work
mkdir .work

# Execute the plan with two workers and persist every result in the sink.
ppilot run plan.py --workers 2 --per-worker 2 \
  --sink .work/sink --results ndjson

# Show the durable NDJSON written by pPilot.
echo 'Durable results:'
cat .work/sink/ready.ndjson

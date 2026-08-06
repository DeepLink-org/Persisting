#!/usr/bin/env bash
set -euo pipefail

# Use the pPilot binary built from this checkout.
export PATH="../../../target/release:$PATH"

# Remove Run Bundles from the previous run.
rm -rf .work
mkdir .work

# Produce three trajectories with at most two running in parallel.
ppilot produce production.py --output .work/runs \
  --parallelism 2 --batch-id example-batch --no-capture

# Print the run and orchestration sections from every generated bundle.
echo 'Generated Run Bundles:'
jq '{run: .run, orchestration}' .work/runs/*/run-bundle.json

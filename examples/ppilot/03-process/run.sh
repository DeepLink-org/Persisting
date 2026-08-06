#!/usr/bin/env bash
set -euo pipefail

# Use the pPilot binary built from this checkout.
export PATH="../../../target/release:$PATH"

# Remove mapper output from the previous run.
rm -rf .work
mkdir .work

# Map the ATIF fixtures in four workers, then reduce their metrics.
ppilot process ../../../crates/persisting-pchronicle/tests/fixtures/atif \
  --script metrics.py --mappers 4 --output .work/output

# Print both the reduced value and the execution report.
echo 'Reduced result:'
jq . .work/output/results.json

echo 'Process report:'
jq . .work/output/process-report.json

#!/usr/bin/env bash
set -euo pipefail

# Use the pPilot binary built from this checkout.
export PATH="../../../target/release:$PATH"

# Remove query output from the previous run.
rm -rf .work
mkdir .work

# Run the SQL analysis over three parallel shards.
ppilot analysis ../../../crates/persisting-pchronicle/tests/fixtures/atif \
  --sql-file analysis.sql --parallelism 3 --fmt json --output .work/output

# Print the merged rows and the per-shard execution report.
echo 'Query results:'
jq . .work/output/results.json

echo 'Analysis report:'
jq . .work/output/analysis-report.json

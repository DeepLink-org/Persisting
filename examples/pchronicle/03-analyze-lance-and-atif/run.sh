#!/usr/bin/env bash
set -euo pipefail

# Use the pPilot binary built from this checkout.
export PATH="../../../target/debug:$PATH"

# Remove generated data and query results from the previous run.
rm -rf .work
mkdir .work

# Build equivalent ATIF and Lance inputs from the same fixtures.
python3 ../common/generate_atif.py ../../../crates/persisting-pchronicle/tests/fixtures/atif \
  .work/trajectories.ndjson 4
cargo run -q --manifest-path ../../../Cargo.toml \
  -p persisting-pchronicle --example import_atif_jsonl -- \
  .work/trajectories.ndjson .work/lance

# Run the same SQL against both storage formats.
ppilot query .work/trajectories.ndjson --source atif \
  --sql 'SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source' \
  > .work/atif.jsonl
ppilot query .work/lance --source lance \
  --sql 'SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source' \
  > .work/lance.jsonl

# Print both result sets and show that they are identical.
echo 'ATIF query result:'
cat .work/atif.jsonl

echo 'Lance query result:'
cat .work/lance.jsonl

echo 'Diff:'
diff -u .work/atif.jsonl .work/lance.jsonl

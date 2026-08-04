#!/usr/bin/env bash
set -euo pipefail

# Remove generated ATIF and Lance data from the previous run.
rm -rf .work
mkdir .work

# Expand the small fixture set, then import it into pChronicle's Lance layout.
python3 ../common/generate_atif.py ../../../crates/persisting-pchronicle/tests/fixtures/atif \
  .work/trajectories.ndjson 64
cargo run -q --manifest-path ../../../Cargo.toml \
  -p persisting-pchronicle --example import_atif_jsonl -- \
  .work/trajectories.ndjson .work/lance

# Compare the source file size with the imported Lance directory size.
echo 'ATIF input:'
wc -l -c .work/trajectories.ndjson

echo 'Lance output:'
du -sh .work/lance

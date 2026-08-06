#!/usr/bin/env bash
set -euo pipefail

export PATH="../../../target/release:$PATH"

if ! command -v persisting >/dev/null 2>&1; then
    cargo build --release -q --manifest-path ../../../Cargo.toml \
        -p persisting-cli --bin persisting
fi

rm -rf .work
mkdir .work

example_scale="${PCHRONICLE_QUERY_MODE_SCALE:-64}"
query_iterations="${PCHRONICLE_QUERY_MODE_ITERS:-5}"
batch_ids="${PCHRONICLE_QUERY_MODE_BATCH_IDS:-64}"
live_batches="${PCHRONICLE_QUERY_MODE_LIVE_BATCHES:-20}"
live_batch_size="${PCHRONICLE_QUERY_MODE_LIVE_BATCH_SIZE:-16}"
follow_poll_ms="${PCHRONICLE_QUERY_MODE_FOLLOW_POLL_MS:-10}"

python3 ../common/generate_atif.py ../../../crates/persisting-pchronicle/tests/fixtures/atif \
  .work/trajectories.ndjson "$example_scale"

python3 benchmark_query_modes.py \
  .work/trajectories.ndjson .work/storyline .work \
  "$query_iterations" "$batch_ids" "$live_batches" "$live_batch_size" "$follow_poll_ms" \
  | tee .work/output.txt

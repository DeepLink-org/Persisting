#!/usr/bin/env bash
set -euo pipefail

export PATH="../../../target/release:$PATH"

# Remove benchmark output from the previous run.
rm -rf .work
mkdir .work

# Build a reproducible corpus with message text sampled from repository source.
bench_scale="${PCHRONICLE_BENCH_SCALE:-64}"
bench_iters="${PCHRONICLE_BENCH_ITERS:-20}"
python3 ../common/generate_atif.py ../../../crates/persisting-pchronicle/tests/fixtures/atif \
  .work/trajectories.ndjson "$bench_scale"

# Measure the installed/built pPilot product CLI end to end.
python3 ../common/benchmark_ppilot.py \
  .work/trajectories.ndjson .work/lance "$bench_iters" \
  | tee .work/output.txt

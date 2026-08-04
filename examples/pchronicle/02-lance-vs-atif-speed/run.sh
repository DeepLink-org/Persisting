#!/usr/bin/env bash
set -euo pipefail

# Remove benchmark output from the previous run.
rm -rf .work
mkdir .work

# Run a small, repeatable comparison and keep a copy of its console output.
PCHRONICLE_BENCH_SCALE=64 PCHRONICLE_BENCH_ITERS=20 \
  cargo run -q --manifest-path ../../../Cargo.toml \
    -p persisting-pchronicle --example compare_analysis_speed \
  | tee .work/output.txt

#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/../.." && pwd)"
pchronicle="${PCHRONICLE_BIN:-$repo_root/target/release/pchronicle}"
benchmark="${PCHRONICLE_BENCH_BIN:-$repo_root/target/release/examples/pchronicle_storage_query_benchmark}"
lifecycle_scenario="$script_dir/01-dataset-lifecycle/run.sh"
performance_scenario="$script_dir/04-storage-query-performance/run.sh"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

compact="$(PCHRONICLE_BIN="$pchronicle" PCHRONICLE_EXAMPLE_VERBOSE=0 \
  bash "$lifecycle_scenario" 2>&1)"
grep -Fq "Dataset lifecycle" <<<"$compact" ||
  fail "compact output is missing its report heading"
grep -Fq "PASS:" <<<"$compact" || fail "compact output is missing PASS"
if grep -Eq '^(settings=|dataset_uri=|snapshot_id=)' <<<"$compact"; then
  fail "compact output leaked raw CLI diagnostics"
fi
if grep -Fq "Imported Dataset" <<<"$compact"; then
  fail "compact output still contains the legacy JSON dump"
fi

verbose="$(PCHRONICLE_BIN="$pchronicle" PCHRONICLE_EXAMPLE_VERBOSE=1 \
  bash "$lifecycle_scenario" 2>&1)"
grep -Fq "Raw command output" <<<"$verbose" ||
  fail "verbose output is missing the raw-log section"
grep -Eq 'snapshot_id=|dataset_uri=' <<<"$verbose" ||
  fail "verbose output did not expose captured CLI diagnostics"

performance_compact="$(
  PCHRONICLE_BENCH_BIN="$benchmark" \
  PCHRONICLE_EXAMPLE_BENCH_SCALE=2 \
  PCHRONICLE_EXAMPLE_BENCH_ITERS=1 \
  PCHRONICLE_EXAMPLE_VERBOSE=0 \
  bash "$performance_scenario" 2>&1
)"
grep -Fq "Storage and query performance" <<<"$performance_compact" ||
  fail "performance output is missing its report heading"
grep -Fq "Compression" <<<"$performance_compact" ||
  fail "performance output is missing its compression conclusion"
grep -Eq 'Lance is [0-9.]+x (faster|slower)|comparable elapsed time' \
  <<<"$performance_compact" ||
  fail "performance output is missing a directional comparison"
if grep -Fq "RESULT benchmark=" <<<"$performance_compact"; then
  fail "compact performance output leaked raw benchmark records"
fi

performance_verbose="$(
  PCHRONICLE_BENCH_BIN="$benchmark" \
  PCHRONICLE_EXAMPLE_BENCH_SCALE=2 \
  PCHRONICLE_EXAMPLE_BENCH_ITERS=1 \
  PCHRONICLE_EXAMPLE_VERBOSE=1 \
  bash "$performance_scenario" 2>&1
)"
grep -Fq "Raw command output" <<<"$performance_verbose" ||
  fail "verbose performance output is missing the raw-log section"
grep -Fq "RESULT benchmark=summary" <<<"$performance_verbose" ||
  fail "verbose performance output did not expose benchmark records"

echo "PASS: lifecycle and performance reports stay compact while verbose mode exposes raw logs"

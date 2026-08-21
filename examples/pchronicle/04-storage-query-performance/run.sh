#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$example_dir/../../.." && pwd)"
source "$example_dir/../common.sh"

benchmark="${PCHRONICLE_BENCH_BIN:-$repo_root/target/release/examples/pchronicle_storage_query_benchmark}"
scale="${PCHRONICLE_EXAMPLE_BENCH_SCALE:-64}"
iterations="${PCHRONICLE_EXAMPLE_BENCH_ITERS:-10}"

case "$scale:$iterations" in
  *[!0-9:]*|:*|*:) echo "benchmark scale and iterations must be positive integers" >&2; exit 2 ;;
esac
if ! awk -v scale="$scale" -v iterations="$iterations" \
  'BEGIN { exit !(scale > 0 && iterations > 0) }'; then
  echo "benchmark scale and iterations must be greater than zero" >&2
  exit 2
fi
if [[ ! -x "$benchmark" ]]; then
  echo "missing pChronicle benchmark executable: $benchmark" >&2
  exit 1
fi

result_field() {
  local line="$1"
  local name="$2"
  local token

  for token in $line; do
    case "$token" in
      "$name="*) printf '%s\n' "${token#*=}"; return 0 ;;
    esac
  done
  echo "missing benchmark field '$name'" >&2
  return 1
}

pchronicle_example_init "$example_dir"
export PCHRONICLE_BENCH_SCALE="$scale"
export PCHRONICLE_BENCH_ITERS="$iterations"
benchmark_output="$(pchronicle_capture 01-benchmark "$benchmark")"

dataset_result="$(grep '^RESULT benchmark=dataset ' <<<"$benchmark_output")"
lifecycle_result="$(grep '^RESULT benchmark=lifecycle ' <<<"$benchmark_output")"
summary_result="$(grep '^RESULT benchmark=summary ' <<<"$benchmark_output")"

documents="$(result_field "$dataset_result" documents)"
steps="$(result_field "$dataset_result" rows)"
json_bytes="$(result_field "$dataset_result" json_bytes)"
lance_bytes="$(result_field "$dataset_result" lance_bytes)"
open_ratio="$(result_field "$summary_result" open_speedup)"
selective_ratio="$(result_field "$summary_result" selective_disk_speedup)"
group_ratio="$(result_field "$summary_result" group_disk_speedup)"
cold_query_ms="$(result_field "$lifecycle_result" cold_query_ms)"
point_lookup_ms="$(result_field "$lifecycle_result" get_storyline_full_ms)"
replace_ms="$(result_field "$lifecycle_result" replace_storyline_ms)"

if ! awk \
  -v documents="$documents" -v steps="$steps" \
  -v json_bytes="$json_bytes" -v lance_bytes="$lance_bytes" \
  -v open_ratio="$open_ratio" -v selective_ratio="$selective_ratio" \
  -v group_ratio="$group_ratio" \
  'BEGIN {
    exit !(documents > 0 && steps > 0 && json_bytes > 0 && lance_bytes > 0 &&
      open_ratio > 0 && selective_ratio > 0 && group_ratio > 0)
  }'; then
  echo "benchmark returned an invalid non-positive metric" >&2
  exit 1
fi
if grep -Eq 'saving -[0-9]|(^|[^[:digit:]])0\.[[:digit:]]+x faster' \
  <<<"$benchmark_output"; then
  echo "benchmark described a regression as an improvement" >&2
  exit 1
fi

size_ratio="$(awk -v json_bytes="$json_bytes" -v lance_bytes="$lance_bytes" \
  'BEGIN { printf "%.2f", json_bytes / lance_bytes }')"

pchronicle_report_start "Storage and query performance"
pchronicle_report_item "Corpus" \
  "$documents documents, $steps steps, scale=${scale}x, iterations=$iterations"
pchronicle_report_item "Storage" \
  "JSON $(pchronicle_human_bytes "$json_bytes") -> Lance $(pchronicle_human_bytes "$lance_bytes")"
pchronicle_report_item "Compression" \
  "JSON/Lance=${size_ratio}x; $(pchronicle_storage_comparison "$json_bytes" "$lance_bytes")"
pchronicle_report_item "Open" \
  "$(pchronicle_relative_performance "Lance" "ATIF datasource" "$open_ratio")"
pchronicle_report_item "Selective" \
  "$(pchronicle_relative_performance "Lance" "JSON scan" "$selective_ratio")"
pchronicle_report_item "GROUP BY" \
  "$(pchronicle_relative_performance "Lance" "JSON scan" "$group_ratio")"
pchronicle_report_item "Lifecycle" \
  "cold query ${cold_query_ms} ms, point lookup ${point_lookup_ms} ms, replace ${replace_ms} ms"
pchronicle_report_item "Scope" "local elapsed times; ratios vary by machine and cache state"
pchronicle_report_finish \
  "storage, query, and lifecycle comparisons completed with equivalent results"

#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/../.." && pwd)"
profile=release

if [[ "${1:-}" == "--profile" ]]; then
  profile="${2:-}"
  shift 2
fi
case "$profile" in
  debug)
    default_binary="$repo_root/target/debug/pchronicle"
    default_benchmark="$repo_root/target/debug/examples/pchronicle_storage_query_benchmark"
    ;;
  release)
    default_binary="$repo_root/target/release/pchronicle"
    default_benchmark="$repo_root/target/release/examples/pchronicle_storage_query_benchmark"
    ;;
  *)
    echo "unsupported pChronicle profile: $profile (expected release or debug)" >&2
    exit 2
    ;;
esac
if [[ "$#" -eq 0 ]]; then
  set -- \
    01-dataset-lifecycle \
    02-built-in-analysis \
    03-cross-dataset-sql \
    04-storage-query-performance \
    05-format-roundtrip \
    06-query-openai-actf-directly
fi

export PCHRONICLE_BIN="${PCHRONICLE_BIN:-$default_binary}"
export PCHRONICLE_BENCH_BIN="${PCHRONICLE_BENCH_BIN:-$default_benchmark}"
test -x "$PCHRONICLE_BIN"

for scenario in "$@"; do
  case "$scenario" in
    01-dataset-lifecycle|02-built-in-analysis|03-cross-dataset-sql|04-storage-query-performance|05-format-roundtrip|06-query-openai-actf-directly) ;;
    *)
      echo "unknown pChronicle example: $scenario" >&2
      exit 2
      ;;
  esac
  example="$script_dir/$scenario/run.sh"
  if [[ ! -f "$example" ]]; then
    echo "missing pChronicle example: examples/pchronicle/$scenario/run.sh" >&2
    exit 1
  fi
  if [[ "$scenario" == "04-storage-query-performance" && ! -x "$PCHRONICLE_BENCH_BIN" ]]; then
    echo "missing pChronicle benchmark executable: $PCHRONICLE_BENCH_BIN" >&2
    exit 1
  fi
  echo "==> examples/pchronicle/$scenario/run.sh"
  bash "$example"
done

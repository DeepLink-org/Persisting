#!/usr/bin/env bash
set -euo pipefail

scenario_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
suite_duration="${PERSISTING_FUZZ_DURATION_SECONDS:-15}"

for suite in formats forwarding storage network-policy; do
  PERSISTING_FUZZ_DURATION_SECONDS="$suite_duration" \
    bash "$scenario_dir/$suite/run.sh"
done

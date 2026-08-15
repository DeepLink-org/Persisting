#!/usr/bin/env bash
set -euo pipefail

regression_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

found=0
for scenario in "$regression_dir"/*/run.sh; do
  [[ -f "$scenario" ]] || continue
  if [[ -f "$(dirname -- "$scenario")/.long-running" ]]; then
    printf '==> %s (skipped: long-running)\n' "${scenario#"$regression_dir"/}"
    continue
  fi
  found=1
  printf '==> %s\n' "${scenario#"$regression_dir"/}"
  bash "$scenario"
done

if [[ "$found" -eq 0 ]]; then
  echo "no regression scenarios found under $regression_dir" >&2
  exit 1
fi

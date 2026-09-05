#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$example_dir/../common.sh"
pvisor_example_init "$example_dir" changeset-management
command -v jq >/dev/null

# Create the host directory shared by the apply and drop examples.
pvisor_example_reset
mkdir -p "$work_dir/base"
printf 'original\n' >"$work_dir/base/existing.txt"
base="$work_dir/base"

# Review and apply the first Run, making its staged files visible on the host.
(
  cd "$base"
  "$pvisor_bin" run --overlayfs-commit manual --stdio capture -- \
    /bin/sh -c 'printf "accepted\n" > existing.txt; printf "accepted\n" > accepted.txt'
)
"$pvisor_bin" review --json "$base" >"$work_dir/apply-review.json"
jq '{run, filesystem}' "$work_dir/apply-review.json"
"$pvisor_bin" apply "$base" >/dev/null

echo 'Base directory after apply:'
cat "$base/existing.txt"
cat "$base/accepted.txt"

# Review and drop the second Run, leaving the host directory unchanged.
(
  cd "$base"
  "$pvisor_bin" run --overlayfs-commit manual --stdio capture -- \
    /bin/sh -c 'printf "rejected\n" > rejected.txt'
)
"$pvisor_bin" review --json "$base" >"$work_dir/drop-review.json"
jq '{run, filesystem}' "$work_dir/drop-review.json"
"$pvisor_bin" drop "$base" >/dev/null

echo 'Base directory after drop:'
cat "$base/existing.txt"
cat "$base/accepted.txt"

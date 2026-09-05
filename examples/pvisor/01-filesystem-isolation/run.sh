#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$example_dir/../common.sh"
pvisor_example_init "$example_dir" filesystem-isolation
command -v jq >/dev/null

# Create a clean host directory for the isolated command.
pvisor_example_reset
mkdir -p "$work_dir/base"
printf 'original\n' >"$work_dir/base/existing.txt"
base="$work_dir/base"

# The project workspace is reusable; pVisor creates an independent stage for this Run.
(
  cd "$base"
  "$pvisor_bin" run --overlayfs-commit manual --stdio capture -- \
    /bin/sh -c 'printf "changed\n" > existing.txt; printf "new\n" > new.txt'
)
run_dir="$(find "$PERSISTING_RUN_HOME" -mindepth 1 -maxdepth 1 -type d -name 'run-*' -print -quit)"
test -n "$run_dir"

# Print the unchanged host file and the two staged files.
echo 'Base directory:'
cat "$base/existing.txt"

echo 'Staged upper directory:'
cat "$run_dir/upper/existing.txt"
cat "$run_dir/upper/new.txt"

echo 'Run Bundle:'
jq '{filesystem, safety}' "$run_dir/run-bundle.json"

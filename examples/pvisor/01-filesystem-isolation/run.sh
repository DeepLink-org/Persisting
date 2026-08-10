#!/usr/bin/env bash
set -euo pipefail

# Use the pVisor binary built from this checkout, even after the example enters
# its isolated base directory below.
example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$example_dir/../../.." && pwd)"
export PATH="$repo_root/target/release:$PATH"

# Create a clean host directory for the isolated command.
rm -rf .work
mkdir -p .work/base
printf 'original\n' > .work/base/existing.txt
export PERSISTING_RUN_HOME="$PWD/.work/runs"
base="$PWD/.work/base"

# The project workspace is reusable; pVisor creates an independent stage for this Run.
(
  cd "$base"
  pvisor run --overlayfs-base "$base" \
    --overlayfs-commit manual --stdio capture -- \
    /bin/sh -c 'printf "changed\n" > existing.txt; printf "new\n" > new.txt'
)
run_dir="$(find "$PERSISTING_RUN_HOME" -mindepth 1 -maxdepth 1 -type d -name 'run-*' | head -n 1)"

# Print the unchanged host file and the two staged files.
echo 'Base directory:'
cat .work/base/existing.txt

echo 'Staged upper directory:'
cat "$run_dir/upper/existing.txt"
cat "$run_dir/upper/new.txt"

echo 'Run Bundle:'
jq '{filesystem, safety}' "$run_dir/run-bundle.json"

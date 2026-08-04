#!/usr/bin/env bash
set -euo pipefail

# Use the pVisor binary built from this checkout.
export PATH="../../../target/debug:$PATH"

# Create a clean host directory for the isolated command.
rm -rf .work
mkdir -p .work/lower
printf 'original\n' > .work/lower/existing.txt

# Stage filesystem changes in the Run workspace instead of changing the host directory.
pvisor run --workspace .work/run --overlayfs-mode overlay \
  --overlayfs-target .work/lower --overlayfs-commit manual --stdio capture -- \
  /bin/sh -c 'printf "changed\n" > existing.txt; printf "new\n" > new.txt'

# Print the unchanged host file and the two staged files.
echo 'Lower directory:'
cat .work/lower/existing.txt

echo 'Staged upper directory:'
cat .work/run/upper/existing.txt
cat .work/run/upper/new.txt

echo 'Run Bundle:'
jq '{filesystem, safety}' .work/run/run-bundle.json

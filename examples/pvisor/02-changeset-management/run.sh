#!/usr/bin/env bash
set -euo pipefail

# Use the pVisor binary built from this checkout.
export PATH="../../../target/release:$PATH"

# Create the host directory shared by the apply and drop examples.
rm -rf .work
mkdir -p .work/base
printf 'original\n' > .work/base/existing.txt
export PERSISTING_RUN_HOME="$PWD/.work/runs"

# Review and apply the first Run, making its staged files visible on the host.
pvisor run --workspace .work/base --overlayfs-base .work/base \
  --overlayfs-commit manual --stdio capture -- \
  /bin/sh -c 'printf "accepted\n" > existing.txt; printf "accepted\n" > accepted.txt'
pvisor review --json .work/base | jq '{run, filesystem}'
pvisor apply .work/base >/dev/null

echo 'Base directory after apply:'
cat .work/base/existing.txt
cat .work/base/accepted.txt

# Review and drop the second Run, leaving the host directory unchanged.
pvisor run --workspace .work/base --overlayfs-base .work/base \
  --overlayfs-commit manual --stdio capture -- \
  /bin/sh -c 'printf "rejected\n" > rejected.txt'
pvisor review --json .work/base | jq '{run, filesystem}'
pvisor drop .work/base >/dev/null

echo 'Base directory after drop:'
cat .work/base/existing.txt
cat .work/base/accepted.txt

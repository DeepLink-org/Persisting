#!/usr/bin/env bash
set -euo pipefail

# Use the pVisor binary built from this checkout.
export PATH="../../../target/release:$PATH"

# Create the host directory shared by the apply and drop examples.
rm -rf .work
mkdir -p .work/lower
printf 'original\n' > .work/lower/existing.txt

# Review and apply the first Run, making its staged files visible on the host.
pvisor run --workspace .work/apply-run --overlayfs-mode overlay \
  --overlayfs-target .work/lower --overlayfs-commit manual --stdio capture -- \
  /bin/sh -c 'printf "accepted\n" > existing.txt; printf "accepted\n" > accepted.txt'
pvisor review --json .work/apply-run | jq '{run, filesystem}'
pvisor apply .work/apply-run >/dev/null

echo 'Lower directory after apply:'
cat .work/lower/existing.txt
cat .work/lower/accepted.txt

# Review and drop the second Run, leaving the host directory unchanged.
pvisor run --workspace .work/drop-run --overlayfs-mode overlay \
  --overlayfs-target .work/lower --overlayfs-commit manual --stdio capture -- \
  /bin/sh -c 'printf "rejected\n" > rejected.txt'
pvisor review --json .work/drop-run | jq '{run, filesystem}'
pvisor drop .work/drop-run >/dev/null

echo 'Lower directory after drop:'
cat .work/lower/existing.txt
cat .work/lower/accepted.txt

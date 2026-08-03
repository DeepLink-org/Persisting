#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
PVISOR="${PVISOR_BIN:-$ROOT/target/debug/pvisor}"
[[ -x "$PVISOR" ]] || (cd "$ROOT" && cargo build -q -p persisting-pvisor --bin pvisor)

WORK="$(mktemp -d "${TMPDIR:-/tmp}/pvisor-filesystem.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/lower"
printf 'original\n' > "$WORK/lower/existing.txt"

"$PVISOR" run --workspace "$WORK/run" --overlayfs-mode overlay \
  --overlayfs-target "$WORK/lower" --overlayfs-commit manual --stdio capture -- \
  /bin/sh -c 'printf "changed\n" > existing.txt; printf "new\n" > new.txt'

LOWER_VALUE="$(tr -d '\n' < "$WORK/lower/existing.txt")"
STAGED_FILES="$(find "$WORK/run/upper" -type f | wc -l | tr -d ' ')"
BUNDLE_CHANGES="$(jq -r '.filesystem.changed_files' "$WORK/run/run-bundle.json")"
NON_BYPASSABLE="$(jq -r '.safety.filesystem_non_bypassable' "$WORK/run/run-bundle.json")"

printf 'RESULT lower_value=%s staged_files=%s bundle_changes=%s filesystem_non_bypassable=%s\n' \
  "$LOWER_VALUE" "$STAGED_FILES" "$BUNDLE_CHANGES" "$NON_BYPASSABLE"
[[ "$LOWER_VALUE" == "original" && "$STAGED_FILES" == "2" && "$BUNDLE_CHANGES" == "2" ]]
[[ "$NON_BYPASSABLE" == "false" ]]
echo 'CONCLUSION pVisor staged two workspace changes while leaving the lower directory unchanged'

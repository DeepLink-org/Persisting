#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
PVISOR="${PVISOR_BIN:-$ROOT/target/debug/pvisor}"
[[ -x "$PVISOR" ]] || (cd "$ROOT" && cargo build -q -p persisting-pvisor --bin pvisor)

WORK="$(mktemp -d "${TMPDIR:-/tmp}/pvisor-changeset.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/lower"
printf 'original\n' > "$WORK/lower/existing.txt"

"$PVISOR" run --workspace "$WORK/apply-run" --overlayfs-mode overlay \
  --overlayfs-target "$WORK/lower" --overlayfs-commit manual --stdio capture -- \
  /bin/sh -c 'printf "accepted\n" > existing.txt; printf "accepted\n" > accepted.txt'
APPLY_REVIEW="$($PVISOR review --json "$WORK/apply-run" | jq -r '.filesystem.changed_files')"
"$PVISOR" apply "$WORK/apply-run" >/dev/null

"$PVISOR" run --workspace "$WORK/drop-run" --overlayfs-mode overlay \
  --overlayfs-target "$WORK/lower" --overlayfs-commit manual --stdio capture -- \
  /bin/sh -c 'printf "rejected\n" > rejected.txt'
DROP_REVIEW="$($PVISOR review --json "$WORK/drop-run" | jq -r '.filesystem.changed_files')"
"$PVISOR" drop "$WORK/drop-run" >/dev/null

APPLIED=0
[[ "$(tr -d '\n' < "$WORK/lower/existing.txt")" == "accepted" ]] && APPLIED=$((APPLIED + 1))
[[ -f "$WORK/lower/accepted.txt" ]] && APPLIED=$((APPLIED + 1))
DROPPED=0
[[ ! -e "$WORK/lower/rejected.txt" ]] && DROPPED=1

printf 'RESULT reviewed_changes=%s applied_files=%s dropped_files=%s\n' \
  "$((APPLY_REVIEW + DROP_REVIEW))" "$APPLIED" "$DROPPED"
[[ "$APPLY_REVIEW" == "2" && "$DROP_REVIEW" == "1" && "$APPLIED" == "2" && "$DROPPED" == "1" ]]
echo 'CONCLUSION review observed three changes; apply accepted two and drop rejected one'

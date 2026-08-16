#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
work_root="${WORK_ROOT:-$example_dir/.work}"
work_dir="$work_root/changeset-management"
export WORK_ROOT="$work_root"

bash "$example_dir/run.sh"

base="$work_dir/base"
jq -e '.run.state == "completed" and .filesystem.changed_files == 2' \
  "$work_dir/apply-review.json" >/dev/null
jq -e '.run.state == "completed" and .filesystem.changed_files == 1' \
  "$work_dir/drop-review.json" >/dev/null
test "$(cat "$base/existing.txt")" = accepted
test "$(cat "$base/accepted.txt")" = accepted
test ! -e "$base/rejected.txt"

echo 'RESULT example=changeset-management reviewed=3 applied=2 dropped=1'

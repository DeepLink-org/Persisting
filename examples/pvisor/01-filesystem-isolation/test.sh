#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
work_root="${WORK_ROOT:-$example_dir/.work}"
work_dir="$work_root/filesystem-isolation"
export WORK_ROOT="$work_root"

bash "$example_dir/run.sh"

base="$work_dir/base"
run_dir="$(find "$work_dir/runs" -mindepth 1 -maxdepth 1 -type d -name 'run-*' -print -quit)"
test -n "$run_dir"
test "$(cat "$base/existing.txt")" = original
test ! -e "$base/new.txt"
test "$(cat "$run_dir/upper/existing.txt")" = changed
test "$(cat "$run_dir/upper/new.txt")" = new
jq -e '
  .run.state == "completed" and
  .filesystem.state == "staged" and
  .filesystem.changed_files == 2 and
  .safety.filesystem_changes_staged == true and
  .safety.filesystem_non_bypassable == true
' "$run_dir/run-bundle.json" >/dev/null

echo 'RESULT example=filesystem-isolation base_unchanged=true staged_changes=2'

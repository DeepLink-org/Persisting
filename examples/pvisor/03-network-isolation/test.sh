#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
work_root="${WORK_ROOT:-$example_dir/.work}"
work_dir="$work_root/network-isolation"
export WORK_ROOT="$work_root"

command -v jq >/dev/null
bash "$example_dir/run.sh"

test "$(cat "$work_dir/allowed.status")" = 0
test "$(tr -d '\r\n' <"$work_dir/allowed.stdout")" = allowed
test "$(cat "$work_dir/denied.status")" != 0
grep -q '(no-network)' "$work_dir/denied.stdout" "$work_dir/denied.stderr"
test "$(cat "$work_dir/direct.status")" = 0
test "$(tr -d '\r\n' <"$work_dir/direct.stdout")" = allowed

bundle_count=0
completed_count=0
failed_count=0
while IFS= read -r bundle; do
  bundle_count=$((bundle_count + 1))
  case "$(jq -r '.run.state' "$bundle")" in
    completed) completed_count=$((completed_count + 1)) ;;
    failed) failed_count=$((failed_count + 1)) ;;
    *) exit 1 ;;
  esac
done < <(find "$work_dir/runs" -name run-bundle.json -type f -print)
test "$bundle_count" = 3
test "$completed_count" = 2
test "$failed_count" = 1

echo 'RESULT example=network-isolation allowed=true denied=true direct_bypass=true'

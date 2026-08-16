#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/../.." && pwd)"
profile=release

if [[ "${1:-}" == "--profile" ]]; then
  profile="${2:-}"
  shift 2
fi
case "$profile" in
  debug) default_binary="$repo_root/target/debug/pvisor" ;;
  release) default_binary="$repo_root/target/release/pvisor" ;;
  *)
    echo "unsupported pVisor profile: $profile (expected release or debug)" >&2
    exit 2
    ;;
esac
if [[ "$#" -eq 0 ]]; then
  set -- \
    01-filesystem-isolation \
    02-changeset-management \
    03-network-isolation \
    04-gateway-llm-control
fi

export PVISOR_BIN="${PVISOR_BIN:-$default_binary}"
export WORK_ROOT="${WORK_ROOT:-$repo_root/target/pvisor-examples}"
test -x "$PVISOR_BIN"

for scenario in "$@"; do
  case "$scenario" in
    01-filesystem-isolation|02-changeset-management|03-network-isolation|04-gateway-llm-control) ;;
    *)
      echo "unknown pVisor example: $scenario" >&2
      exit 2
      ;;
  esac
  echo "==> examples/pvisor/$scenario/run.sh"
  bash "$script_dir/$scenario/run.sh"
done

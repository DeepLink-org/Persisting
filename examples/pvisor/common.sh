#!/usr/bin/env bash

# Shared example infrastructure. Scenario commands and result presentation stay
# in each run.sh; this file only owns workspace and local-service plumbing.
pvisor_example_init() {
  example_dir="$1"
  scenario="$2"
  repo_root="$(cd -- "$example_dir/../../.." && pwd)"
  pvisor_bin="${PVISOR_BIN:-$repo_root/target/release/pvisor}"
  work_root="${WORK_ROOT:-$example_dir/.work}"
  work_dir="$work_root/$scenario"

  cd "$example_dir"
  test -x "$pvisor_bin"
  if [[ -z "$work_root" || "$work_root" == "/" || -z "$work_dir" || "$work_dir" == "/" ]]; then
    echo "refusing unsafe work root: $work_root" >&2
    return 2
  fi
}

pvisor_example_reset() {
  rm -rf -- "$work_dir"
  mkdir -p "$work_dir"
  export PERSISTING_RUN_HOME="$work_dir/runs"
}

pvisor_free_ports() {
  python3 - "$1" <<'PY'
import socket
import sys

sockets = []
for _ in range(int(sys.argv[1])):
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    sockets.append(sock)
print(*(sock.getsockname()[1] for sock in sockets))
for sock in sockets:
    sock.close()
PY
}

pvisor_wait_http() {
  local url="$1"
  for ((attempt = 0; attempt < 50; attempt++)); do
    if curl --fail --silent "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
  echo "HTTP service did not become ready: $url" >&2
  return 1
}

pvisor_wait_tcp() {
  local port="$1"
  for ((attempt = 0; attempt < 50; attempt++)); do
    if python3 - "$port" >/dev/null 2>&1 <<'PY'
import socket
import sys

with socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=0.2):
    pass
PY
    then
      return 0
    fi
    sleep 0.1
  done
  echo "TCP service did not become ready: 127.0.0.1:$port" >&2
  return 1
}

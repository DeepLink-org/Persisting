#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$example_dir/../common.sh"
pvisor_example_init "$example_dir" network-isolation
command -v curl >/dev/null

server_port="$(pvisor_free_ports 1)"
server_url="http://127.0.0.1:$server_port"

pvisor_example_reset
mkdir -p "$work_dir/server"
printf 'allowed\n' >"$work_dir/server/index.html"

python3 -m http.server "$server_port" --bind 127.0.0.1 \
  --directory "$work_dir/server" >"$work_dir/server.log" 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true' EXIT
pvisor_wait_http "$server_url"

echo '1. Allowlist permits the declared destination'
set +e
"$pvisor_bin" run --overlaynet-allow "127.0.0.1:$server_port" -- \
  bash -ceu '
    exec curl --fail --silent --show-error \
      --proxy "$HTTP_PROXY" --noproxy "" "$1"
  ' bash "$server_url" \
  >"$work_dir/allowed.stdout" 2>"$work_dir/allowed.stderr"
allowed_status=$?
set -e
printf '%s\n' "$allowed_status" >"$work_dir/allowed.status"
printf 'exit status: %s\n' "$allowed_status"
cat "$work_dir/allowed.stdout"
cat "$work_dir/allowed.stderr"

echo
echo '2. Deny-all rejects traffic that uses the injected proxy'
set +e
"$pvisor_bin" run --overlaynet-deny-all -- \
  bash -ceu '
    exec curl --fail-with-body --silent --show-error \
      --proxy "$HTTP_PROXY" --noproxy "" "$1"
  ' bash "$server_url" \
  >"$work_dir/denied.stdout" 2>"$work_dir/denied.stderr"
denied_status=$?
set -e
printf '%s\n' "$denied_status" >"$work_dir/denied.status"
printf 'exit status: %s\n' "$denied_status"
cat "$work_dir/denied.stdout"
cat "$work_dir/denied.stderr"

echo
echo '3. A direct socket can bypass the cooperative proxy'
set +e
"$pvisor_bin" run --overlaynet-deny-all -- \
  bash -ceu '
    exec curl --fail --silent --show-error --noproxy "*" "$1"
  ' bash "$server_url" \
  >"$work_dir/direct.stdout" 2>"$work_dir/direct.stderr"
direct_status=$?
set -e
printf '%s\n' "$direct_status" >"$work_dir/direct.status"
printf 'exit status: %s\n' "$direct_status"
cat "$work_dir/direct.stdout"
cat "$work_dir/direct.stderr"

echo
echo 'Conclusion: OverlayNet controls cooperative proxy traffic; it is not a network sandbox.'

#!/usr/bin/env bash
set -euo pipefail

export PATH="../../../target/release:$PATH"

work_dir="${WORK_ROOT:-.work}/network-isolation"
server_port=19111
server_url="http://127.0.0.1:$server_port"

rm -rf -- "$work_dir"
mkdir -p "$work_dir/server"
printf 'allowed\n' >"$work_dir/server/index.html"

python3 -m http.server "$server_port" --bind 127.0.0.1 \
  --directory "$work_dir/server" >"$work_dir/server.log" 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true' EXIT
sleep 0.3

echo '1. Allowlist permits the declared destination'
pvisor run --overlaynet-allow "127.0.0.1:$server_port" -- \
  bash -ceu '
    body=$(curl --fail --silent --show-error \
      --proxy "$HTTP_PROXY" --noproxy "" "$1")
    test "$body" = allowed
  ' bash "$server_url"
echo 'PASS: the proxied request was allowed.'

echo
echo '2. Deny-all rejects traffic that uses the injected proxy'
pvisor run --overlaynet-deny-all -- \
  bash -ceu '
    if denied_output=$(curl --fail-with-body --silent --show-error \
      --proxy "$HTTP_PROXY" --noproxy "" "$1" 2>&1); then
      exit 1
    fi
    case "$denied_output" in
      *"(no-network)"*) ;;
      *) exit 1 ;;
    esac
  ' bash "$server_url"
echo 'PASS: the proxied request was rejected with no-network.'

echo
echo '3. A direct socket can bypass the cooperative proxy'
pvisor run --overlaynet-deny-all -- \
  bash -ceu '
    body=$(curl --fail --silent --show-error --noproxy "*" "$1")
    test "$body" = allowed
  ' bash "$server_url"
echo 'PASS: direct access bypassed the proxy, as documented.'

echo
echo 'Conclusion: OverlayNet controls cooperative proxy traffic; it is not a network sandbox.'
echo 'RESULT example=network-isolation allowed=true denied=true direct_bypass=true'

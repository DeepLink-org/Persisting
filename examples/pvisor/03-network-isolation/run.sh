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
pvisor run --workspace "$work_dir/allow" \
  --overlaynet-allow "127.0.0.1:$server_port" --stdio inherit -- \
  bash -ceu '
    body=$(curl --fail --silent --show-error \
      --proxy "$HTTP_PROXY" --noproxy "" "$1")
    printf "response: %s\n" "$body"
    test "$body" = allowed
  ' bash "$server_url"
echo 'PASS: the proxied request was allowed.'

echo
echo '2. Deny-all rejects traffic that uses the injected proxy'
set +e
denied_output=$(pvisor run --workspace "$work_dir/deny" \
  --overlaynet-deny-all --stdio inherit -- \
  bash -c 'curl --fail-with-body --silent --show-error \
    --proxy "$HTTP_PROXY" --noproxy "" "$1"' bash "$server_url" 2>&1)
denied_status=$?
set -e
printf '%s\n' "$denied_output"
if [ "$denied_status" -eq 0 ] || [[ "$denied_output" != *'(no-network)'* ]]; then
  echo 'FAIL: deny-all did not reject the proxied request.' >&2
  exit 1
fi
echo 'PASS: the proxied request was rejected with no-network.'

echo
echo '3. A direct socket can bypass the cooperative proxy'
pvisor run --workspace "$work_dir/direct" \
  --overlaynet-deny-all --stdio inherit -- \
  bash -ceu '
    body=$(curl --fail --silent --show-error --noproxy "*" "$1")
    printf "response: %s\n" "$body"
    test "$body" = allowed
  ' bash "$server_url"
echo 'PASS: direct access bypassed the proxy, as documented.'

echo
echo 'Conclusion: OverlayNet controls cooperative proxy traffic; it is not a network sandbox.'
echo 'RESULT example=network-isolation allowed=true denied=true direct_bypass=true'

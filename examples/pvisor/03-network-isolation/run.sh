#!/usr/bin/env bash
set -euo pipefail

# Use the pVisor binary built from this checkout.
export PATH="../../../target/debug:$PATH"

# Start a local HTTP server so the network policy has a deterministic target.
rm -rf .work
mkdir .work
printf 'ok\n' > .work/index.html
python3 -m http.server 19111 --bind 127.0.0.1 --directory .work >.work/server.log 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true' EXIT
sleep 0.3

# Allow the first request and print the recorded network counters.
pvisor run --workspace .work/allow-run --overlaynet-allow 127.0.0.1:19111 --stdio capture -- \
  /bin/sh -c 'curl --fail --silent --proxy "$HTTP_PROXY" --noproxy "" "$1"' sh http://127.0.0.1:19111/

echo 'Allowed request:'
jq '.network.intercepted' .work/allow-run/run-bundle.json

# Deny the same request. Failure is expected, so keep going and print its Run Bundle.
pvisor run --workspace .work/deny-run --overlaynet-deny 127.0.0.1:19111 --stdio capture -- \
  /bin/sh -c 'curl --fail --silent --proxy "$HTTP_PROXY" --noproxy "" "$1"' sh \
  http://127.0.0.1:19111/ >.work/deny.log 2>&1 || true

echo 'Denied request:'
cat .work/deny.log
jq '{network: .network.intercepted, safety}' .work/deny-run/run-bundle.json

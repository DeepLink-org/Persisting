#!/usr/bin/env bash
set -euo pipefail

archive="${1:?usage: release_archive.sh <archive.tar.gz>}"
tmp="$(mktemp -d)"
cleanup() {
  local pid
  for pid in "${proxy_pid:-}" "${mock_pid:-}"; do
    if [[ -n "${pid}" ]]; then
      kill "${pid}" 2>/dev/null || true
      wait "${pid}" 2>/dev/null || true
    fi
  done
  rm -rf "${tmp}"
}
trap cleanup EXIT

checksum="${archive}.sha256"
checksum_dir="$(dirname "${checksum}")"
checksum_file="$(basename "${checksum}")"
archive_file="$(basename "${archive}")"
test -f "${checksum}"
(
  cd "${checksum_dir}"
  sha256sum -c "${checksum_file}"
)
recorded_name="$(awk 'NR == 1 { sub(/^\*/, "", $2); print $2 }' "${checksum}")"
[[ "${recorded_name}" == "${archive_file}" ]] \
  || { echo "FAIL: checksum must record archive basename" >&2; exit 1; }

tar -xzf "${archive}" -C "${tmp}"
test -x "${tmp}/dlcapt-deploy/bin/dlcapt"
test -f "${tmp}/dlcapt-deploy/config/proxy.lance-s3.deploy.toml"
test -f "${tmp}/dlcapt-deploy/config/proxy.example.toml"
test ! -f "${tmp}/dlcapt-deploy/config/proxy.lance-s3-online.toml"
if compgen -G "${tmp}/dlcapt-deploy/config/*online*" >/dev/null \
  || compgen -G "${tmp}/dlcapt-deploy/config/*beta*" >/dev/null; then
  echo "FAIL: online/beta config present in archive" >&2
  exit 1
fi
archive_members="$(tar -tzf "${archive}")"
for tool in cargo rustc protoc; do
  if grep -Eq "(^|/)${tool}/?$" <<<"${archive_members}"; then
    echo "FAIL: build tool ${tool} present in archive" >&2
    exit 1
  fi
done

python3_bin="$(command -v python3 || true)"
curl_bin="$(command -v curl || true)"
[[ -n "${python3_bin}" ]] || { echo "FAIL: python3 is required by this test harness" >&2; exit 127; }
[[ -n "${curl_bin}" ]] || { echo "FAIL: curl is required by this test harness" >&2; exit 127; }

# Restricted PATH: no cargo/rustc/protoc
restricted_path="/usr/bin:/bin:/usr/sbin:/sbin"
export PATH="${restricted_path}"
command -v cargo >/dev/null 2>&1 && { echo "FAIL: cargo visible"; exit 1; }
command -v rustc >/dev/null 2>&1 && { echo "FAIL: rustc visible"; exit 1; }
command -v protoc >/dev/null 2>&1 && { echo "FAIL: protoc visible"; exit 1; }

# Minimal mock upstream; bind port 0 once and report its actual port.
mock_port_file="${tmp}/mock-port"
"${python3_bin}" - "${mock_port_file}" <<'PY' &
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import pathlib
import sys

class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n=int(self.headers.get("Content-Length","0"))
        _=self.rfile.read(n)
        body=json.dumps({
            "id":"chatcmpl-release",
            "choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
        }).encode()
        self.send_response(200)
        self.send_header("Content-Type","application/json")
        self.send_header("Content-Length",str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass

server = HTTPServer(("127.0.0.1", 0), H)
pathlib.Path(sys.argv[1]).write_text(f"{server.server_port}\n")
server.serve_forever()
PY
mock_pid=$!

for i in $(seq 1 60); do
  if [[ -s "${mock_port_file}" ]]; then
    mock_port="$(<"${mock_port_file}")"
    break
  fi
  sleep 0.05
  if [[ "${i}" -eq 60 ]]; then
    echo "FAIL: mock upstream did not report its port" >&2
    exit 1
  fi
done

deploy="${tmp}/dlcapt-deploy"
# Override deploy config to json_file + mock upstream so release test does not need S3.
cat > "${deploy}/config/proxy.release-test.toml" <<EOF
listen = "127.0.0.1:0"
admin_listen = "127.0.0.1:0"
store_dir = "var/store"
agent_id = "openclaw"
session_header = "x-persisting-session-id"
default_session_id = "default"
preserve_raw = false
base_session_path = "/v1/sessions"

[storage]
authoritative = "json_file"
also = ["md"]

[export.defaults]
env_name = "release-test"
job_id = "dlcapt"

[export.session_metadata]
source = "dlcapt-proxy"

[[models]]
name = "kimi-k2.5"
display_name = "Kimi K2.5"
provider = "openai"
upstream_base_url = "http://127.0.0.1:${mock_port}/v1"
api_key = ""

[[models]]
name = "*"
display_name = "Fallback"
provider = "openai"
upstream_base_url = "http://127.0.0.1:${mock_port}/v1"
api_key = ""
EOF

(
  cd "${deploy}"
  exec ./bin/dlcapt config/proxy.release-test.toml
) &
proxy_pid=$!

# The proxy owns two dynamically bound listeners. Discover their actual ports
# from its live Linux sockets, so no port is ever bound, released, then rebound.
proxy_ports="$("${python3_bin}" - "${proxy_pid}" <<'PY'
import glob
import os
import sys
import time

pid = sys.argv[1]
deadline = time.monotonic() + 15
while time.monotonic() < deadline:
    inodes = set()
    for fd in glob.glob(f"/proc/{pid}/fd/*"):
        try:
            target = os.readlink(fd)
        except OSError:
            continue
        if target.startswith("socket:[") and target.endswith("]"):
            inodes.add(target[8:-1])

    ports = []
    try:
        lines = open("/proc/net/tcp", encoding="utf-8").read().splitlines()[1:]
    except OSError:
        lines = []
    for line in lines:
        fields = line.split()
        if len(fields) < 10 or fields[3] != "0A" or fields[9] not in inodes:
            continue
        host, port = fields[1].split(":")
        if host == "0100007F":
            ports.append(str(int(port, 16)))
    if len(ports) >= 2:
        print("\n".join(ports))
        sys.exit(0)
    time.sleep(0.05)

print("FAIL: proxy did not open two dynamic IPv4 listeners", file=sys.stderr)
sys.exit(1)
PY
)"

public_port=""
admin_port=""
for i in $(seq 1 60); do
  while IFS= read -r port; do
    if "${curl_bin}" -sf "http://127.0.0.1:${port}/v1/models" >/dev/null; then
      public_port="${port}"
    fi
    if "${curl_bin}" -sf "http://127.0.0.1:${port}/admin/sessions" >/dev/null; then
      admin_port="${port}"
    fi
  done <<<"${proxy_ports}"
  if [[ -n "${public_port}" && -n "${admin_port}" ]]; then
    break
  fi
  sleep 0.25
  if [[ "${i}" -eq 60 ]]; then
    echo "FAIL: proxy did not become ready on dynamic ports" >&2
    exit 1
  fi
done

"${curl_bin}" -sf "http://127.0.0.1:${public_port}/healthz" >/dev/null
"${curl_bin}" -sf "http://127.0.0.1:${public_port}/readyz" >/dev/null
"${curl_bin}" -sf "http://127.0.0.1:${public_port}/v1/models" >/dev/null
"${curl_bin}" -sf "http://127.0.0.1:${admin_port}/admin/sessions" >/dev/null

"${curl_bin}" -sf -X POST "http://127.0.0.1:${public_port}/v1/sessions/release-session/chat/completions" \
  -H 'Content-Type: application/json' \
  -d '{"model":"kimi-k2.5","messages":[{"role":"user","content":"hi"}],"stream":false}' >/dev/null

test -f "${deploy}/var/store/release-session/trajectory.md"
echo "PASS release_archive.sh"

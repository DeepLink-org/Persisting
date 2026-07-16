#!/usr/bin/env bash
set -euo pipefail

archive="${1:?usage: release_archive.sh <archive.tar.gz>}"
tmp="$(mktemp -d)"
cleanup() {
  if [[ -n "${proxy_pid:-}" ]]; then kill "${proxy_pid}" 2>/dev/null || true; fi
  if [[ -n "${mock_pid:-}" ]]; then kill "${mock_pid}" 2>/dev/null || true; fi
  rm -rf "${tmp}"
}
trap cleanup EXIT

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
for tool in cargo rustc protoc; do
  if [[ -e "${tmp}/dlcapt-deploy/bin/${tool}" ]]; then
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

# Minimal mock upstream; use the pre-resolved harness interpreter after PATH restriction.
mock_port="$("${python3_bin}" - <<'PY'
import socket
s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()
PY
)"

"${python3_bin}" - <<PY &
from http.server import BaseHTTPRequestHandler, HTTPServer
import json

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

HTTPServer(("127.0.0.1", ${mock_port}), H).serve_forever()
PY
mock_pid=$!

deploy="${tmp}/dlcapt-deploy"
# Override deploy config to json_file + mock upstream so release test does not need S3.
cat > "${deploy}/config/proxy.release-test.toml" <<EOF
listen = "127.0.0.1:19081"
admin_listen = "127.0.0.1:19082"
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
  ./bin/dlcapt config/proxy.release-test.toml
) &
proxy_pid=$!

for i in $(seq 1 60); do
  if "${curl_bin}" -sf "http://127.0.0.1:19082/admin/sessions" >/dev/null; then
    break
  fi
  sleep 0.25
  if [[ "${i}" -eq 60 ]]; then
    echo "FAIL: proxy did not become ready" >&2
    exit 1
  fi
done

"${curl_bin}" -sf "http://127.0.0.1:19081/healthz" >/dev/null
"${curl_bin}" -sf "http://127.0.0.1:19081/readyz" >/dev/null
"${curl_bin}" -sf "http://127.0.0.1:19081/v1/models" >/dev/null
"${curl_bin}" -sf "http://127.0.0.1:19082/admin/sessions" >/dev/null

"${curl_bin}" -sf -X POST "http://127.0.0.1:19081/v1/sessions/release-session/chat/completions" \
  -H 'Content-Type: application/json' \
  -d '{"model":"kimi-k2.5","messages":[{"role":"user","content":"hi"}],"stream":false}' >/dev/null

test -f "${deploy}/var/store/release-session/trajectory.md"
echo "PASS release_archive.sh"

#!/usr/bin/env bash
set -euo pipefail

exec 9>&2

trace_exec() {
  BASH_XTRACEFD=9 bash -x -c 'exec "$@"' bash "$@"
}

# Use the pVisor binary built from this checkout.
export PATH="../../../target/release:$PATH"

# Keep generated output separate from the example sources. Override WORK_ROOT
# when running in CI or when preserving a previous .work/ walkthrough.
work_root=${WORK_ROOT:-.work}
work_dir="$work_root/network-policy"
server_port=19111
closed_port=19112
server_url="http://127.0.0.1:$server_port"
current_case=setup

on_error() {
  status=$?
  printf '\nCASE %s RESULT: FAIL (exit code %s)\n' "$current_case" "$status" >&2
  printf '[FAIL] OverlayNet 示例在 run.sh:%s 中止。\n' \
    "${BASH_LINENO[0]:-unknown}" >&2
  printf '[FAIL] 未出现 OVERALL: PASS 表示至少一个自动断言未通过。\n' >&2
  exit "$status"
}

trap on_error ERR

rm -rf -- "$work_dir"
mkdir -p "$work_dir/server"
printf 'allowed\n' >"$work_dir/server/index.html"
dd if=/dev/zero of="$work_dir/server/payload.bin" bs=4096 count=1 2>/dev/null

# A deterministic loopback server lets every scenario run without Internet
# access. Port 19112 deliberately has no listener.
python3 -m http.server "$server_port" --bind 127.0.0.1 \
  --directory "$work_dir/server" >"$work_dir/server.log" 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true' EXIT
sleep 0.3

show_bundle() {
  jq -r '"Bundle 摘要：mode=\(.network.policy.mode), strength=\(.network.interception.strength), allowed=\(.network.intercepted.policy_allowed), denied=\(.network.intercepted.policy_denied), failures=\(.network.intercepted.failures)"' \
    "$1/run-bundle.json"
  printf '完整记录：%s/run-bundle.json\n' "$1"
}

echo '判定规则：每个场景都包含自动断言；任一失败将立即以非零状态退出。'

current_case=1
echo
echo '=== CASE 1. Allowlist：CIDR + 端口 ==='
echo '目标端口应成功；同一 IP 的其他端口应在建立连接前被拒绝。'
trace_exec pvisor run --workspace "$work_dir/01-allowlist" \
  --overlaynet-allow "127.0.0.0/8:$server_port" --stdio inherit -- \
  bash ./curl-checks.sh allowlist "$server_url/" "http://127.0.0.1:$closed_port/"
show_bundle "$work_dir/01-allowlist"
echo 'CASE 1 RESULT: PASS — Allowlist 同时正确执行了允许和端口拒绝。'

current_case=2
echo
echo '=== CASE 2. Public 默认允许 + 显式 deny ==='
echo '这里隐式使用 public mode；显式 deny 应始终优先。'
trace_exec pvisor run --workspace "$work_dir/02-explicit-deny" \
  --overlaynet-deny "127.0.0.1:$server_port" --stdio inherit -- \
  bash ./curl-checks.sh explicit-deny "$server_url/"
show_bundle "$work_dir/02-explicit-deny"
echo 'CASE 2 RESULT: PASS — Public 模式下 explicit deny 优先生效。'

current_case=3
echo
echo '=== CASE 3. 结构化 TOML：私网 hostname + transport + 叠加限速 ==='
echo 'localhost 被显式允许解析到 loopback；CIDR 规则应限制 4096 字节的下载速度。'
trace_exec pvisor run --config advanced-policy.toml --workspace "$work_dir/03-structured-policy" \
  --stdio inherit -- \
  bash ./curl-checks.sh structured-policy "http://localhost:$server_port/payload.bin"
show_bundle "$work_dir/03-structured-policy"
echo 'CASE 3 RESULT: PASS — 结构化 hostname/transport/限速策略全部生效。'

current_case=4
echo
echo '=== CASE 4. Deny-all 与 cooperative 边界 ==='
echo '代理请求应被拒绝；显式绕过代理的 direct socket 仍可访问 server。'
trace_exec pvisor run --workspace "$work_dir/04-deny-all" --overlaynet-deny-all \
  --stdio inherit -- \
  bash ./curl-checks.sh deny-all "$server_url/"
show_bundle "$work_dir/04-deny-all"
echo 'CASE 4 RESULT: PASS — Deny-all 和 cooperative 绕过边界均符合预期。'

current_case=summary
echo
echo '=== 自动判定结果 ==='
echo 'OVERALL: PASS (4/4 cases, exit code 0)'
echo "产物目录：$work_dir"

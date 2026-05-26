#!/usr/bin/env bash
# Capture 分步示例
# ─────────────────────────────────────────────────────────────────────────────
#   ./run.sh              一键跑通
#   ./run.sh mock         仅 Mock LLM（分步教程 · 终端 A）
#   ./run.sh check        校验 0001.md
#
# 流程（run）：
#   1. cargo build persisting CLI
#   2. 后台启动 mock_llm.py  :19080
#   3. persisting capture run → 执行 agent.py（两轮对话）
#   4. 打印 store/.../0001.md → replay → check
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$DIR/../.." && pwd)"

STORAGE="$DIR/store"
PROXY="$DIR/proxy.toml"
AGENT="$DIR/agent.py"
SESSION="${SESSION:-walkthrough-001}"
AGENT_ID="demo-agent"
MOCK_PORT=19080
MD="$STORAGE/$AGENT_ID/$SESSION/0001.md"

case "${1:-run}" in
  mock)  exec python3 "$DIR/mock_llm.py" ;;
  check) shift; MD="${1:-$MD}"; exec python3 "$DIR/check.py" "$MD" ;;
  run)   ;;
  -h|--help)
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  *)     echo "用法: ./run.sh [run|mock|check]" >&2; exit 1 ;;
esac

die() { echo "error: $*" >&2; exit 1; }

if [[ -n "${PERSISTING_CLI:-}" ]]; then
  CLI="$PERSISTING_CLI"
elif [[ -x "$REPO/target/debug/persisting" ]]; then
  CLI="$REPO/target/debug/persisting"
else
  CLI="$(command -v persisting 2>/dev/null)" || die "先构建: cargo build -p persisting-cli -p persisting-engine"
fi

if [[ -z "${PERSISTING_ENGINE_LIB:-}" ]]; then
  for lib in libpersisting_engine.dylib libpersisting_engine.so; do
    [[ -f "$REPO/target/debug/$lib" ]] && export PERSISTING_ENGINE_LIB="$REPO/target/debug/$lib" && break
  done
fi

cleanup() {
  if [[ -n "${MOCK_PID:-}" ]]; then
    kill "$MOCK_PID" 2>/dev/null || true
    wait "$MOCK_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "==> [1/5] cargo build"
  (cd "$REPO" && CARGO_TARGET_DIR=target cargo build -p persisting-cli -p persisting-engine)
fi

echo "==> [2/5] Mock LLM :$MOCK_PORT"
mkdir -p "$STORAGE"
MOCK_LLM_PORT=$MOCK_PORT python3 "$DIR/mock_llm.py" &
MOCK_PID=$!
sleep 0.3
kill -0 "$MOCK_PID" || die "Mock LLM 启动失败"

echo "==> [3/5] capture run  session=$SESSION"
"$CLI" capture run -o "$STORAGE" -c "$PROXY" -f md -- python3 "$AGENT"

echo "==> [4/5] 等待 0001.md（4 块）"
for _ in $(seq 1 30); do
  if [[ -f "$MD" ]]; then
    n=$(grep -cE '^<!-- persisting:block:(user|assistant)' "$MD" 2>/dev/null || echo 0)
    [[ "$n" -ge 4 ]] && break
  fi
  sleep 0.2
done
[[ -f "$MD" ]] || die "未找到 $MD"
n=$(grep -cE '^<!-- persisting:block:(user|assistant)' "$MD" 2>/dev/null || echo 0)
[[ "$n" -ge 4 ]] || die "0001.md 块数不足（$n/4）"

echo "==> [5/5] 打印 / replay / check"
echo "────────────────────────────────────────"
cat "$MD"
echo "────────────────────────────────────────"
"$CLI" trajectory replay "$STORAGE" \
  --agent-id "$AGENT_ID" --session-id "$SESSION" --storage-format markdown
python3 "$DIR/check.py" "$MD"
echo "==> 完成  →  $MD"

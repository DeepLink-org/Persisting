#!/usr/bin/env bash
# Capture 分步示例
# ─────────────────────────────────────────────────────────────────────────────
#   ./run.sh              一键跑通
#   ./run.sh mock         仅 Mock LLM（分步教程 · 终端 A）
#   ./run.sh check        校验最近生成的 AgenticMD
#
# 流程（run）：
#   1. cargo build pvisor + persisting CLI
#   2. 后台启动 mock_llm.py  :19080
#   3. pvisor → 执行 agent.py（两轮对话）
#   4. 打印本次 Run 的 AgenticMD → replay → check
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$DIR/../.." && pwd)"

STORAGE="$DIR/store"
RUN_CONFIG="$DIR/run.toml"
AGENT="$DIR/agent.py"
AGENT_ID="demo-agent"
MOCK_PORT=19080
MD=""

case "${1:-run}" in
  mock)  exec python3 "$DIR/mock_llm.py" ;;
  check)
    shift
    MD="${1:-$(find "$STORAGE/$AGENT_ID" -type f -name '*.md' -print -quit 2>/dev/null)}"
    if [[ -z "$MD" ]]; then
      echo "error: 未找到 AgenticMD；先运行 ./run.sh" >&2
      exit 1
    fi
    exec python3 "$DIR/check.py" "$MD"
    ;;
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
elif [[ -x "$REPO/target/release/persisting" ]]; then
  CLI="$REPO/target/release/persisting"
else
  CLI="$(command -v persisting 2>/dev/null || true)"
  [[ -n "$CLI" ]] || CLI="$REPO/target/release/persisting"
fi

if [[ -n "${PVISOR_CLI:-}" ]]; then
  PVISOR="$PVISOR_CLI"
elif [[ -x "$REPO/target/release/pvisor" ]]; then
  PVISOR="$REPO/target/release/pvisor"
else
  PVISOR="$(command -v pvisor 2>/dev/null || true)"
  [[ -n "$PVISOR" ]] || PVISOR="$REPO/target/release/pvisor"
fi

if [[ -z "${PERSISTING_ENGINE_LIB:-}" ]]; then
  for lib in libpersisting_engine.dylib libpersisting_engine.so; do
    [[ -f "$REPO/target/release/$lib" ]] && export PERSISTING_ENGINE_LIB="$REPO/target/release/$lib" && break
  done
fi

cleanup() {
  if [[ -n "${MOCK_PID:-}" ]]; then
    kill "$MOCK_PID" 2>/dev/null || true
    wait "$MOCK_PID" 2>/dev/null || true
  fi
  [[ -n "${RUN_MARKER:-}" ]] && rm -f "$RUN_MARKER"
}
trap cleanup EXIT

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "==> [1/5] cargo build"
  (cd "$REPO" && CARGO_TARGET_DIR=target cargo build --release -p persisting-cli -p persisting-engine -p persisting-pvisor)
  CLI="$REPO/target/release/persisting"
  PVISOR="$REPO/target/release/pvisor"
fi

echo "==> [2/5] Mock LLM :$MOCK_PORT"
mkdir -p "$STORAGE"
MOCK_LLM_PORT=$MOCK_PORT python3 "$DIR/mock_llm.py" &
MOCK_PID=$!
sleep 0.3
kill -0 "$MOCK_PID" || die "Mock LLM 启动失败"

echo "==> [3/5] pvisor run"
RUN_MARKER="$(mktemp)"
WORKSPACE="$STORAGE/workspace-$(date +%Y%m%d-%H%M%S)"
"$PVISOR" run --config "$RUN_CONFIG" --workspace "$WORKSPACE" -- python3 "$AGENT"

MD="$(find "$WORKSPACE/$AGENT_ID" -type f -name '*.md' -newer "$RUN_MARKER" -print -quit 2>/dev/null)"

echo "==> [4/5] 等待 AgenticMD（4 块）"
for _ in $(seq 1 30); do
  if [[ -n "$MD" && -f "$MD" ]]; then
    n=$(grep -cE '^<!-- persisting:block:(user|assistant)' "$MD" 2>/dev/null || echo 0)
    [[ "$n" -ge 4 ]] && break
  fi
  sleep 0.2
done
[[ -n "$MD" && -f "$MD" ]] || die "未找到本次 Run 生成的 AgenticMD"
n=$(grep -cE '^<!-- persisting:block:(user|assistant)' "$MD" 2>/dev/null || echo 0)
[[ "$n" -ge 4 ]] || die "AgenticMD 块数不足（$n/4）"

echo "==> [5/5] 打印 / replay / check"
echo "────────────────────────────────────────"
cat "$MD"
echo "────────────────────────────────────────"
"$CLI" trajectory replay "$(dirname "$MD")" --storage-format markdown
python3 "$DIR/check.py" "$MD"
echo "==> 完成  →  $MD"

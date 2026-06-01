#!/usr/bin/env bash
# 统一测试 / 回归入口：交互选单或直接指定套件名。
#
#   ./scripts/test_suite.sh              # 交互菜单（TTY）
#   ./scripts/test_suite.sh list         # 列出套件
#   ./scripts/test_suite.sh gate         # 直接运行
#   ./scripts/test_suite.sh all-regression

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

PROFILE="${PERSISTING_BUILD_PROFILE:-debug}"
SKIP_REBUILD="${SKIP_REBUILD:-0}"

die() { echo "error: $*" >&2; exit 1; }

run_just() {
  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ just $*"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  just "$@"
}

# 套件顺序（bash 3.2 兼容，不用关联数组）
SUITE_NAMES=(
  dev gate ci
  rust rust-engine capture-rust search-integration
  search-traj-smoke search-traj-full traj-e2e
  capture-integration capture-stress capture-run-e2e capture-all
  capture-check test-py all-regression
)

SUITE_DESCS=(
  "日常：fmt + clippy + 引擎单测 + search/traj 冒烟"
  "提交前：fmt + clippy + 全 workspace Rust 测试"
  "CI 近似：gate + build + capture 集成 + Claude 回归"
  "全 workspace cargo test"
  "persisting-engine 库单测"
  "capture crate + fixtures + Claude 回归"
  "engine search_integration 测试"
  "search + traj CLI 冒烟（quick=1）"
  "search + traj CLI 全量基准"
  "traj import → stats → judge → stats"
  "proxy / import / daemon 集成"
  "capture 写入压测"
  "traj capture -f vortex 全链路"
  "全部 capture shell 集成（含 traj-e2e）"
  "capture Rust 测试 + capture-all"
  "Python pytest"
  "Rust capture 测试 + 全部 shell 回归（跳过 fmt/clippy）"
)

suite_desc() {
  local want="$1" i
  for i in "${!SUITE_NAMES[@]}"; do
    if [[ "${SUITE_NAMES[$i]}" == "$want" ]]; then
      echo "${SUITE_DESCS[$i]}"
      return 0
    fi
  done
  return 1
}

run_suite() {
  local name="$1"
  if [[ "$SKIP_REBUILD" == "1" ]]; then
    export SKIP_BUILD=1
  fi
  case "$name" in
    dev) run_just dev ;;
    gate) run_just gate ;;
    ci) run_just ci ;;
    rust) run_just test-rust ;;
    rust-engine) run_just _test-engine ;;
    capture-rust) run_just capture-test ;;
    search-integration) run_just test-engine-integration ;;
    search-traj-smoke) run_just smoke "$PROFILE" ;;
    search-traj-full) run_just integration "$PROFILE" ;;
    traj-e2e) run_just traj-e2e "$PROFILE" ;;
    capture-integration) run_just capture-integration "$PROFILE" ;;
    capture-stress) run_just capture-stress "$PROFILE" ;;
    capture-run-e2e) run_just capture-run-e2e "$PROFILE" ;;
    capture-all) run_just capture-all "$PROFILE" ;;
    capture-check) run_just capture-check "$PROFILE" ;;
    test-py) run_just test-py ;;
    all-regression)
      run_just capture-test
      run_just capture-all "$PROFILE"
      run_just smoke "$PROFILE"
      ;;
    *)
      die "unknown suite: $name (try: $0 list)"
      ;;
  esac
}

print_list() {
  echo "Persisting 测试 / 回归套件（just test-suite <name>）："
  echo ""
  local i=1
  local n desc
  for n in "${SUITE_NAMES[@]}"; do
    desc="$(suite_desc "$n")"
    printf "  %2d. %-22s %s\n" "$i" "$n" "$desc"
    i=$((i + 1))
  done
  echo ""
  echo "示例："
  echo "  just test-suite              # 交互选单"
  echo "  just test-suite gate"
  echo "  just test-suite traj-e2e"
  echo "  SKIP_REBUILD=1 just test-suite traj-e2e   # 跳过 rebuild"
  echo "  just test-suite all-regression"
}

interactive_menu() {
  echo "Persisting — 测试 / 回归选单"
  echo "profile=$PROFILE  SKIP_REBUILD=$SKIP_REBUILD"
  echo ""
  local i=1
  local labels=()
  local n desc
  for n in "${SUITE_NAMES[@]}"; do
    desc="$(suite_desc "$n")"
    labels+=("$i) $n — $desc")
    i=$((i + 1))
  done
  labels+=("0) 退出")

  PS3=$'\n请选择 > '
  select _ in "${labels[@]}"; do
    if [[ "${REPLY:-}" == "0" ]]; then
      echo "已取消"
      exit 0
    fi
    if [[ -n "${REPLY:-}" && "$REPLY" -ge 1 && "$REPLY" -le ${#SUITE_NAMES[@]} ]]; then
      run_suite "${SUITE_NAMES[$((REPLY - 1))]}"
      exit $?
    fi
    echo "无效选项，请输入 0–${#SUITE_NAMES[@]}"
  done
}

main() {
  local cmd="${1:-}"
  case "$cmd" in
    ""|menu)
      if [[ -t 0 && -t 1 ]]; then
        interactive_menu
      else
        print_list
        die "非交互环境请指定套件：just test-suite <name>"
      fi
      ;;
    list|help|-h|--help)
      print_list
      ;;
    *)
      run_suite "$cmd"
      echo ""
      echo "✓ suite '$cmd' OK"
      ;;
  esac
}

main "$@"

#!/usr/bin/env bash
# 集成测试统一入口（全部是 scripts/integration/*.sh）。
#
#   ./scripts/test_suite.sh              # 列出套件
#   ./scripts/test_suite.sh list
#   ./scripts/test_suite.sh smoke
#   ./scripts/test_suite.sh capture-all
#   SKIP_BUILD=1 ./scripts/test_suite.sh traj-e2e
#
# just 薄封装：just smoke / just traj-e2e / just capture-all / …

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"
INT="$REPO_ROOT/scripts/integration"

PROFILE="${PERSISTING_BUILD_PROFILE:-debug}"
export PERSISTING_BUILD_PROFILE="$PROFILE"
[[ "${SKIP_REBUILD:-0}" == "1" ]] && export SKIP_BUILD=1

die() { echo "error: $*" >&2; exit 1; }

run_sh() {
  local script="$1"
  shift || true
  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "▶ $script${*:+ $*}"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  bash "$INT/$script" "$@"
}

SUITE_NAMES=(
  smoke integration
  traj-e2e
  capture-integration capture-stress capture-run-e2e
  capture-all
  all-integration
)

SUITE_DESCS=(
  "traj CLI 冒烟"
  "traj CLI 集成"
  "history import/stats → eval judge/stats"
  "proxy / import / daemon 集成"
  "capture 写入压测"
  "traj capture -f lance 全链路"
  "全部 capture 集成脚本"
  "全部集成脚本（smoke + capture-all）"
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
  case "$1" in
    smoke)
      run_sh traj_e2e.sh
      ;;
    integration)
      run_sh traj_e2e.sh
      ;;
    traj-e2e)
      run_sh traj_e2e.sh
      ;;
    capture-integration)
      run_sh capture_integration.sh
      ;;
    capture-stress)
      run_sh capture_stress.sh
      ;;
    capture-run-e2e)
      run_sh capture_run_e2e.sh
      ;;
    capture-all)
      run_sh traj_e2e.sh
      run_sh capture_integration.sh
      run_sh capture_stress.sh
      run_sh capture_run_e2e.sh
      ;;
    all-integration|all-regression)
      run_sh traj_e2e.sh
      run_sh capture_integration.sh
      run_sh capture_stress.sh
      run_sh capture_run_e2e.sh
      ;;
    *)
      die "unknown suite: $1 (try: $0 list)"
      ;;
  esac
}

print_list() {
  echo "Persisting 集成测试（shell → scripts/integration/）："
  echo ""
  local i=1 n desc
  for n in "${SUITE_NAMES[@]}"; do
    desc="$(suite_desc "$n")"
    printf "  %2d. %-22s %s\n" "$i" "$n" "$desc"
    i=$((i + 1))
  done
  echo ""
  echo "示例："
  echo "  ./scripts/test_suite.sh list"
  echo "  ./scripts/test_suite.sh smoke"
  echo "  SKIP_BUILD=1 ./scripts/test_suite.sh traj-e2e"
  echo "  just smoke / just traj-e2e / just capture-all   # 等价薄封装"
}

main() {
  local cmd="${1:-list}"
  case "$cmd" in
    list|help|-h|--help|"")
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

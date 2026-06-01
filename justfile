# Persisting — 仓库主任务入口
# 安装：brew install just / cargo install just
#
# 测试选单：just test-suite          # 交互选择测试 / 回归套件
#           just test-suite list     # 列出全部套件
#           just test-suite gate     # 直接运行指定套件

repo := justfile_directory()
docs_dir := repo / "docs"
gen_py := repo / "scripts" / "generate_benchmark_data.py"
test_suite_sh := repo / "scripts" / "test_suite.sh"

engine_filename := if os() == "macos" {
    "libpersisting_engine.dylib"
} else if os() == "windows" {
    "persisting_engine.dll"
} else {
    "libpersisting_engine.so"
}

# ── 帮助 ─────────────────────────────────────────────────────────────────────

default:
    @just --list --unsorted
    @echo ""
    @echo "常用："
    @echo "  just test-suite          # 测试 / 回归选单（推荐入口）"
    @echo "  just test-suite list     # 列出全部套件"
    @echo "  just dev                 # 日常开发（fmt + clippy + check-quick）"
    @echo "  just gate                # 提交前（fmt + clippy + test-rust）"
    @echo "  just ci                  # CI 近似全量"
    @echo "  just capture-all         # 全部 capture 集成"
    @echo "  just docs-serve          # 本地文档"

# ── 测试 / 回归统一入口 ───────────────────────────────────────────────────────

# 交互选单或直接运行：just test-suite [dev|gate|traj-e2e|all-regression|…]
# 环境变量：PERSISTING_BUILD_PROFILE=debug  SKIP_REBUILD=1
test-suite name="":
    #!/usr/bin/env bash
    set -euo pipefail
    export PERSISTING_BUILD_PROFILE="${PERSISTING_BUILD_PROFILE:-debug}"
    export SKIP_REBUILD="${SKIP_REBUILD:-0}"
    bash "{{ test_suite_sh }}" {{ if name != "" { name } else { "menu" } }}

# 列出全部测试套件（非交互）
test-list:
    bash "{{ test_suite_sh }}" list

# 别名
test-menu:
    just test-suite

regression:
    SKIP_REBUILD="${SKIP_REBUILD:-0}" just test-suite all-regression

# ── 构建 ─────────────────────────────────────────────────────────────────────

build profile="debug":
    cargo build -p persisting-cli -p persisting-engine {{ if profile == "release" { "--release" } else { "" } }}

build-release:
    just build profile=release

clean:
    cargo clean

# ── Rust 质量 ────────────────────────────────────────────────────────────────

fmt:
    cargo fmt --all

clippy:
    cargo clippy --workspace --all-targets

# fmt + clippy + 全 workspace cargo test
gate:
    just fmt
    just clippy
    just test-rust

# 日常开发快捷路径
dev:
    just fmt
    just clippy
    just check-quick

# CI 近似：gate + 构建 + capture 集成 + Claude 回归
ci:
    just gate
    just build
    SKIP_BUILD=1 just capture-integration
    just test-capture-claude

# ── Rust 测试 ─────────────────────────────────────────────────────────────────

# 单 crate：engine | proto | core | capture | cli
test-crate crate:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{ crate }}" in
      engine) cargo test -p persisting-engine ;;
      proto) cargo test -p persisting-proto ;;
      core) cargo test -p persisting-core ;;
      capture) cargo test -p persisting-capture ;;
      cli) cargo test -p persisting-cli ;;
      *) echo "unknown crate: {{ crate }} (engine|proto|core|capture|cli)" >&2; exit 2 ;;
    esac

test-rust:
    cargo test --workspace

test-capture-claude:
    cargo test -p persisting-capture --test capture_apps_claude

test-capture-fixtures:
    cargo test -p persisting-capture --test llm_fixtures --test ag_fixture_tests

test-engine-integration:
    cargo test -p persisting-engine --test search_integration

_test-engine:
    cargo test -p persisting-engine --lib

# ── Python ───────────────────────────────────────────────────────────────────

py-sync:
    uv sync --all-extras

py-dev:
    uv run maturin develop --release

test-py:
    uv run pytest tests/ -q

# ── 文档（docs/ 子项目）──────────────────────────────────────────────────────

docs-sync:
    cd "{{ docs_dir }}" && uv sync --all-extras

docs-serve:
    cd "{{ docs_dir }}" && uv run mkdocs serve

docs-serve-dirty:
    cd "{{ docs_dir }}" && uv run mkdocs serve --dirtyreload

docs-build:
    cd "{{ docs_dir }}" && uv run mkdocs build

docs-links:
    cd "{{ docs_dir }}" && uv run mkdocs-linkcheck src

# ── 数据与 fixture ───────────────────────────────────────────────────────────

# 生成 search/traj 集成基准数据（integration 内部也会调用）
generate-benchmark search_rows="100" traj_rows="50" seed="42" search_out="" traj_out="":
    #!/usr/bin/env bash
    set -euo pipefail
    gen_py="{{ gen_py }}"
    [[ -f "$gen_py" ]] || { echo "missing $gen_py" >&2; exit 1; }
    args=(--seed "{{ seed }}" --search-rows "{{ search_rows }}" --traj-rows "{{ traj_rows }}")
    [[ -n "{{ search_out }}" ]] && args+=(--search-out "{{ search_out }}")
    [[ -n "{{ traj_out }}" ]] && args+=(--traj-out "{{ traj_out }}")
    python3 "$gen_py" "${args[@]}"

# ── Search + Trajectory CLI 集成 ─────────────────────────────────────────────

# 全链路：generate → search → trajectory；quick=1 为冒烟规模
integration profile="debug" cli_source="target" cli_bin="" engine_override="" reorder="0" quick="0" search_rows="100000" traj_rows="100000" seed="42" skip_rebuild="0" embed_dim="32" num_partitions="128" nprobes="32" replay_limit="50": (_integration-build-if-target cli_source profile)
    #!/usr/bin/env bash
    set -euo pipefail
    repo="{{ repo }}"
    gen_py="{{ gen_py }}"
    die() { echo "error: $*" >&2; exit 1; }
    command -v python3 >/dev/null || die "need python3"
    [[ -f "$gen_py" ]] || die "missing $gen_py"

    cli_source="{{ cli_source }}"
    profile="{{ profile }}"
    td="$repo/target/$profile"

    if [[ "$cli_source" == "target" ]]; then
      export PERSISTING_CLI="$td/persisting"
      export PERSISTING_ENGINE_LIB="$td/{{ engine_filename }}"
    elif [[ -n "{{ cli_bin }}" ]]; then
      export PERSISTING_CLI="{{ cli_bin }}"
      if [[ -n "{{ engine_override }}" ]]; then
        export PERSISTING_ENGINE_LIB="{{ engine_override }}"
      else
        unset PERSISTING_ENGINE_LIB || true
      fi
    else
      command -v persisting >/dev/null || die "cli_source=path needs persisting on PATH or cli_bin=..."
      export PERSISTING_CLI="$(command -v persisting)"
      if [[ -n "{{ engine_override }}" ]]; then
        export PERSISTING_ENGINE_LIB="{{ engine_override }}"
      else
        unset PERSISTING_ENGINE_LIB || true
      fi
    fi

    [[ -x "$PERSISTING_CLI" ]] || die "not executable: $PERSISTING_CLI"

    reorder="{{ reorder }}"
    quick="{{ quick }}"
    seed="{{ seed }}"
    skip_rebuild="{{ skip_rebuild }}"
    embed_dim="{{ embed_dim }}"
    replay_limit="{{ replay_limit }}"

    if [[ "$quick" == "1" ]]; then
      sr=70; tr=25; np=2; nv=2
    else
      sr="{{ search_rows }}"; tr="{{ traj_rows }}"
      np="{{ num_partitions }}"; nv="{{ nprobes }}"
    fi

    echo "==> CLI: $PERSISTING_CLI"
    if [[ -n "${PERSISTING_ENGINE_LIB:-}" ]]; then
      echo "==> PERSISTING_ENGINE_LIB=$PERSISTING_ENGINE_LIB"
    else
      echo "==> PERSISTING_ENGINE_LIB unset (CLI default lookup)"
    fi
    echo "==> params: cli_source=$cli_source profile=$profile reorder=$reorder quick=$quick rows=$sr/$tr seed=$seed"

    WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/persisting-it.XXXXXX")"
    trap 'rm -rf "$WORKDIR"' EXIT

    DATASET="$WORKDIR/search_ds"
    STORAGE="$WORKDIR/traj_root"
    TRAJ_AGENT="bench_agent"
    TRAJ_SESSION="cli_bench_run"
    SEARCH_JSONL="$WORKDIR/docs.jsonl"
    TRAJ_INPUT="$WORKDIR/traj_records.toml"

    if [[ -t 1 ]]; then
      _b=$'\033[1m'; _c=$'\033[36m'; _m=$'\033[35m'; _y=$'\033[33m'; _n=$'\033[0m'
    else
      _b=; _c=; _m=; _y=; _n=
    fi
    it_rule() { printf '%b%s%b\n' "$_b$_c" "────────────────────────────────────────────────────────" "$_n"; }
    it_sh() {
      it_rule; printf '%b$ ' "$_b$_m"; printf '%q ' "$@"; printf '%b\n' "$_n"; it_rule; "$@"
    }
    it_run_cli_out() {
      it_rule; printf '%b$ ' "$_b$_m"; printf '%q' "$PERSISTING_CLI"; printf ' %q' "$@"; printf '%b\n' "$_n"
      it_rule; out=$("$PERSISTING_CLI" "$@")
      printf '%b[stdout 捕获]%b\n' "$_b$_y" "$_n"; printf '%s\n\n' "$out"
    }
    timer_start() { export TIMER_START="$(python3 -c "import time; print(time.perf_counter())")"; }
    timer_end() {
      python3 -c "import os,time,sys; t0=float(os.environ['TIMER_START']); print(f\"[timer] {sys.argv[1]}: {time.perf_counter()-t0:.3f}s\")" "$1"
    }
    assert_ok() {
      grep -qE 'status:\s*"ok"|status\s*=\s*"ok"' <<<"$1" || { echo "--- bad response ---"; echo "$1"; exit 1; }
    }

    export RUNNER_T0="$(python3 -c "import time; print(time.perf_counter())")"

    echo "==> generate JSONL (seed=$seed)"
    timer_start
    it_sh python3 "$gen_py" --seed "$seed" --search-rows "$sr" --traj-rows "$tr" --search-out "$SEARCH_JSONL" --traj-out "$TRAJ_INPUT"
    timer_end "generate_benchmark_data.py"

    echo "==> search create ($sr rows)"
    timer_start
    it_run_cli_out search create "$DATASET" --input "$SEARCH_JSONL" --format jsonl --embedding-dim "$embed_dim"
    assert_ok "$out"; timer_end "search create"

    echo "==> search index list (pre-build)"
    timer_start
    it_run_cli_out search index list "$DATASET"
    assert_ok "$out"; timer_end "search index list (pre-build)"

    echo "==> search index build"
    timer_start
    it_run_cli_out search index build "$DATASET" \
      --vector-column embedding --text-column text --metric cosine \
      --num-partitions "$np" --ivf-max-iters 12 \
      --pq-num-sub-vectors 8 --pq-num-bits 4 --pq-max-iters 12 \
      --pq-sample-rate 4 --pq-kmeans-redos 1
    assert_ok "$out"; timer_end "search index build"

    echo "==> search index list (post-build)"
    timer_start
    it_run_cli_out search index list "$DATASET"
    assert_ok "$out"
    grep -q 'persisting_ivf_pq' <<<"$out" || die "missing persisting_ivf_pq"
    grep -q 'persisting_fts' <<<"$out" || die "missing persisting_fts"
    timer_end "search index list (post-build)"

    echo "==> search query x3"
    timer_start
    it_run_cli_out search query "$DATASET" "integration alpha gamma" --mode vector --k 5 --embedding-dim "$embed_dim" --nprobes "$nv"
    assert_ok "$out"
    it_run_cli_out search query "$DATASET" "integration" --mode fts --k 8 --embedding-dim "$embed_dim"
    assert_ok "$out"
    it_run_cli_out search query "$DATASET" "keyword beta" --mode hybrid --k 5 --embedding-dim "$embed_dim"
    assert_ok "$out"
    timer_end "search query (3 modes)"

    if [[ "$reorder" == "1" ]]; then
      echo "==> search index reorder"
      timer_start
      it_run_cli_out search index reorder "$DATASET" persisting_ivf_pq --in-place
      assert_ok "$out"; timer_end "search index reorder"
    fi

    if [[ "$skip_rebuild" != "1" ]]; then
      echo "==> search index rebuild"
      timer_start
      it_run_cli_out search index rebuild "$DATASET" --no-retrain
      assert_ok "$out"; timer_end "search index rebuild"
    else
      echo "==> skip search index rebuild"
    fi

    echo "==> trajectory add ($tr rows)"
    timer_start
    it_run_cli_out trajectory add "$STORAGE" --agent-id "$TRAJ_AGENT" --session-id "$TRAJ_SESSION" --format toml --input "$TRAJ_INPUT"
    assert_ok "$out"; timer_end "trajectory add"

    echo "==> trajectory stats"
    timer_start
    it_run_cli_out trajectory stats "$STORAGE" --agent-id "$TRAJ_AGENT" --session-id "$TRAJ_SESSION"
    assert_ok "$out"
    grep -Fq "$TRAJ_SESSION" <<<"$out" || die "stats missing session_id"
    grep -Fq "$TRAJ_AGENT" <<<"$out" || die "stats missing agent_id"
    timer_end "trajectory stats"

    echo "==> trajectory replay"
    timer_start
    it_run_cli_out trajectory replay "$STORAGE" --agent-id "$TRAJ_AGENT" --session-id "$TRAJ_SESSION" --offset 2 --limit "$replay_limit"
    assert_ok "$out"
    grep -qE '\[\[records\]\]|^records\s*=' <<<"$out" || die "replay missing TOML records"
    timer_end "trajectory replay"

    it_sh python3 -c "import os,time; t0=float(os.environ['RUNNER_T0']); print(f'[timer] TOTAL wall: {time.perf_counter()-t0:.3f}s')"
    echo "==> integration OK"

_integration-build-if-target cli_source profile:
    #!/usr/bin/env bash
    if [[ "{{ cli_source }}" == "target" ]]; then
      cd "{{ repo }}"
      if [[ "{{ profile }}" == "release" ]]; then
        cargo build -p persisting-cli -p persisting-engine --release
      else
        cargo build -p persisting-cli -p persisting-engine
      fi
    fi

check profile="debug":
    just _test-engine
    just integration profile="{{ profile }}"

smoke profile="debug":
    just integration profile="{{ profile }}" quick="1"

check-quick profile="debug":
    just _test-engine
    just integration profile="{{ profile }}" quick="1"

# ── Capture 集成（scripts/integration/*.sh，build 由 _common.sh 处理）────────

# traj 子命令：import → stats → judge --score → stats / judge-stats
# 跳过 rebuild：SKIP_BUILD=1 或 SKIP_REBUILD=1
traj-e2e profile="debug" cli_bin="" engine_override="":
    #!/usr/bin/env bash
    set -euo pipefail
    export PERSISTING_BUILD_PROFILE="{{ profile }}"
    [[ "${SKIP_REBUILD:-0}" == "1" || "${SKIP_BUILD:-0}" == "1" ]] && export SKIP_BUILD=1
    [[ -n "{{ cli_bin }}" ]] && export PERSISTING_CLI="{{ cli_bin }}"
    [[ -n "{{ engine_override }}" ]] && export PERSISTING_ENGINE_LIB="{{ engine_override }}"
    echo "==> traj-e2e (profile={{ profile }})"
    bash "{{ repo }}/scripts/integration/traj_e2e.sh"

capture-integration profile="debug" cli_bin="" engine_override="":
    #!/usr/bin/env bash
    set -euo pipefail
    export PERSISTING_BUILD_PROFILE="{{ profile }}"
    [[ "${SKIP_REBUILD:-0}" == "1" || "${SKIP_BUILD:-0}" == "1" ]] && export SKIP_BUILD=1
    [[ -n "{{ cli_bin }}" ]] && export PERSISTING_CLI="{{ cli_bin }}"
    [[ -n "{{ engine_override }}" ]] && export PERSISTING_ENGINE_LIB="{{ engine_override }}"
    echo "==> capture-integration (profile={{ profile }})"
    bash "{{ repo }}/scripts/integration/capture_integration.sh"

capture-stress profile="debug" cli_bin="" engine_override="" requests="80" concurrency="8":
    #!/usr/bin/env bash
    set -euo pipefail
    export PERSISTING_BUILD_PROFILE="{{ profile }}"
    export REQUESTS="{{ requests }}"
    export CONCURRENCY="{{ concurrency }}"
    [[ "${SKIP_REBUILD:-0}" == "1" || "${SKIP_BUILD:-0}" == "1" ]] && export SKIP_BUILD=1
    [[ -n "{{ cli_bin }}" ]] && export PERSISTING_CLI="{{ cli_bin }}"
    [[ -n "{{ engine_override }}" ]] && export PERSISTING_ENGINE_LIB="{{ engine_override }}"
    echo "==> capture-stress requests={{ requests }} concurrency={{ concurrency }}"
    bash "{{ repo }}/scripts/integration/capture_stress.sh"

capture-run-e2e profile="debug" cli_bin="" engine_override="" turns="3":
    #!/usr/bin/env bash
    set -euo pipefail
    export PERSISTING_BUILD_PROFILE="{{ profile }}"
    export TURNS="{{ turns }}"
    [[ "${SKIP_REBUILD:-0}" == "1" || "${SKIP_BUILD:-0}" == "1" ]] && export SKIP_BUILD=1
    [[ -n "{{ cli_bin }}" ]] && export PERSISTING_CLI="{{ cli_bin }}"
    [[ -n "{{ engine_override }}" ]] && export PERSISTING_ENGINE_LIB="{{ engine_override }}"
    echo "==> capture-run-e2e turns={{ turns }}"
    bash "{{ repo }}/scripts/integration/capture_run_e2e.sh"

capture-walkthrough profile="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    td="{{ repo }}/target/{{ profile }}"
    [[ "${SKIP_REBUILD:-0}" == "1" || "${SKIP_BUILD:-0}" == "1" ]] && export SKIP_BUILD=1
    export PERSISTING_CLI="$td/persisting"
    if [[ -f "$td/{{ engine_filename }}" ]]; then
      export PERSISTING_ENGINE_LIB="$td/{{ engine_filename }}"
    fi
    if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
      just build profile="{{ profile }}"
    fi
    bash "{{ repo }}/examples/capture-walkthrough/run.sh"

# 依次跑全部 capture 集成（不含 walkthrough demo）
capture-all profile="debug":
    just traj-e2e profile="{{ profile }}"
    just capture-integration profile="{{ profile }}"
    just capture-stress profile="{{ profile }}"
    just capture-run-e2e profile="{{ profile }}"

# capture 相关 Rust 测试
capture-test:
    just test-crate capture
    just test-capture-fixtures
    just test-capture-claude

# 完整 capture 验证：Rust 测试 + 全部 shell 集成
capture-check profile="debug":
    just capture-test
    just capture-all profile="{{ profile }}"

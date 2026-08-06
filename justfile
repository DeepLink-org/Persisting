# Persisting — 仓库主任务入口
# 安装：brew install just / cargo install just
#
# 集成测试：scripts/integration/*.sh（推荐 ./scripts/test_suite.sh）
#           just 仅作薄封装：just smoke / just traj-e2e / just capture-all

repo := justfile_directory()
docs_dir := repo / "docs"
gen_py := repo / "scripts" / "generate_benchmark_data.py"
test_suite_sh := repo / "scripts" / "test_suite.sh"

# Python 路径（ruff format）
ruff_paths := "persisting tests examples"
# lint 默认只扫包代码（与 CI 一致）；全量用 lint-py-all
ruff_lint_paths := "persisting"

# ── 帮助 ─────────────────────────────────────────────────────────────────────

default:
    @just --list --unsorted
    @echo ""
    @echo "常用："
    @echo "  just gate                # 提交前（fmt + lint + test-rust）"
    @echo "  just smoke               # trajectory CLI 冒烟"
    @echo "  just regression          # capture Rust + 集成 + smoke"
    @echo "  ./scripts/test_suite.sh  # 集成测试套件（shell）"
    @echo "  just ci                  # CI 近似全量"
    @echo "  just py-dev              # maturin develop（Python 扩展）"
    @echo "  just install-cli         # 安装统一 CLI、pvisor 和 ppilot"
    @echo "  just build-wheel         # 打 release wheel → dist/"
    @echo "  just capture-all         # 全部 capture 集成"
    @echo "  just docs-serve          # 本地文档"

# ── 测试套件导航 ──────────────────────────────────────────────────────────────

# 列出推荐测试（Rust/门禁用 just；集成用 scripts/test_suite.sh）
[group('test')]
test-list:
    #!/usr/bin/env bash
    set -euo pipefail
    cat <<'EOF'
    Persisting 测试入口

      门禁 / Rust（just）
        just gate                 提交前：fmt + lint + test-rust
        just dev                  日常：fmt + clippy + check-quick
        just ci                   CI 近似
        just test-rust / test-pchronicle / capture-test / test-py

      集成（shell → scripts/integration/）
        ./scripts/test_suite.sh list
        ./scripts/test_suite.sh smoke
        ./scripts/test_suite.sh traj-e2e
        ./scripts/test_suite.sh capture-all
        ./scripts/test_suite.sh all-integration

      just 薄封装（等价调用上述脚本）
        just smoke / just integration / just traj-e2e
        just capture-integration / just capture-stress / just capture-run-e2e
        just capture-all / just regression

    环境变量：PERSISTING_BUILD_PROFILE  SKIP_BUILD=1  SKIP_REBUILD=1
    EOF

# 集成套件入口（转发到 scripts/test_suite.sh）
[group('test')]
test-suite name="list":
    bash "{{ test_suite_sh }}" {{ name }}

[group('test')]
test-menu:
    bash "{{ test_suite_sh }}" list

# Run the deterministic, quantitative pVisor examples.
[group('test')]
examples-pvisor:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release -q -p persisting-pvisor --bin pvisor
    for example in "{{ repo }}"/examples/pvisor/*; do
        [[ -f "$example/run.sh" ]] || continue
        echo "==> ${example#"{{ repo }}/"}/run.sh"
        (cd "$example" && bash run.sh)
    done

# Run the deterministic, quantitative pChronicle examples.
[group('test')]
examples-pchronicle:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release -q -p persisting-ppilot --features cli --bin ppilot
    for example in "{{ repo }}"/examples/pchronicle/*; do
        [[ -f "$example/run.sh" ]] || continue
        echo "==> ${example#"{{ repo }}/"}/run.sh"
        (cd "$example" && bash run.sh)
    done

# Run the deterministic, quantitative pPilot examples.
[group('test')]
examples-ppilot:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release -q -p persisting-ppilot --features cli --bin ppilot
    for example in "{{ repo }}"/examples/ppilot/*; do
        [[ -f "$example/run.sh" ]] || continue
        echo "==> ${example#"{{ repo }}/"}/run.sh"
        (cd "$example" && bash run.sh)
    done

[group('test')]
examples: examples-pvisor examples-pchronicle examples-ppilot

# capture Rust + 全部 shell 集成 + CLI 冒烟（跳过 fmt/clippy）
[group('test')]
regression profile="debug":
    just capture-test
    just capture-all profile="{{ profile }}"
    just smoke profile="{{ profile }}"

# ── 构建 ─────────────────────────────────────────────────────────────────────

build profile="debug":
    cargo build -p persisting-cli {{ if profile == "release" { "--release" } else { "" } }}
    cargo build -p persisting-pvisor --bin pvisor {{ if profile == "release" { "--release" } else { "" } }}
    cargo build -p persisting-ppilot --features cli --bin ppilot {{ if profile == "release" { "--release" } else { "" } }}

build-release:
    just build profile=release

# Install the complete component set expected by the unified `persisting` CLI.
install-cli:
    #!/usr/bin/env bash
    set -euo pipefail
    install_root="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
    cargo install --path crates/persisting-cli --locked --force --root "$install_root"
    cargo install --path crates/persisting-pvisor --locked --force --root "$install_root"
    cargo install --path crates/persisting-ppilot --features cli --locked --force --root "$install_root"
    printf 'Installed Persisting component set in %s/bin\n' "$install_root"

# PEP 517 release wheel（Python extension + persisting/pvisor/ppilot）→ dist/
build-wheel:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p dist
    uv build --force-pep517 --wheel --out-dir dist
    wheel=$(ls -t dist/*.whl | head -n 1)
    python3 scripts/packaging/verify_wheel.py "$wheel" --install-smoke
    ls -la "$wheel"

# 开发调试 wheel（dev profile，不 strip）
build-wheel-debug:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p dist
    uv build --force-pep517 --wheel --out-dir dist \
      --config-setting 'build-args=--profile dev'
    wheel=$(ls -t dist/*.whl | head -n 1)
    python3 scripts/packaging/verify_wheel.py "$wheel" --install-smoke
    ls -la "$wheel"

clean:
    cargo clean
    rm -rf dist target/wheels .venv htmlcov .coverage coverage.xml

# ── 格式化 / Lint ─────────────────────────────────────────────────────────────

# 格式化 Rust + Python（会改写文件）
fmt: fmt-rust fmt-py

fmt-rust:
    cargo fmt --all

fmt-py:
    uvx ruff format {{ ruff_paths }}

# 只检查格式，不改写（CI / pre-commit）
fmt-check: fmt-check-rust fmt-check-py

fmt-check-rust:
    cargo fmt --all -- --check

fmt-check-py:
    uvx ruff format --check {{ ruff_paths }}

# clippy + ruff（不改写）
lint: lint-rust lint-py

lint-rust:
    cargo clippy --workspace --all-targets --locked

lint-py:
    uvx ruff check {{ ruff_lint_paths }}

# 含 tests/examples（较严，可能有存量告警）
lint-py-all:
    uvx ruff check {{ ruff_paths }}

# 与 Pulsing 同级的严格 clippy（workspace 未清干净前慎用）
clippy-deny:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# 兼容旧名
clippy:
    just lint-rust

# 自动修：format + ruff --fix
fix: fmt
    uvx ruff check {{ ruff_paths }} --fix

# 仅修 Python
fix-py: fmt-py
    uvx ruff check {{ ruff_paths }} --fix

# 格式 + lint 快检（不跑测试；对应 Pulsing 的 check-quick 语义）
style: fmt-check lint
    @echo "✅ format + lint OK"

# fmt + lint + Rust 测试（提交前）
gate:
    just fmt
    just lint
    just test-rust

# 与 GitHub Actions `ci.yml` lint 对齐（只检查、不改写）
ci-lint:
    just fmt-check-rust
    just lint-rust
    uvx ruff check persisting/

# 日常开发快捷路径
dev:
    just fmt
    just lint-rust
    just check-quick

# CI 近似：gate + 构建 + capture 集成 + Claude 回归
ci:
    just gate
    just build
    SKIP_BUILD=1 just capture-integration
    just test-capture-claude

# ── Rust 测试 ─────────────────────────────────────────────────────────────────

# 单 crate：pchronicle | control | core | capture | cli | ppilot | pvisor | dlcapt
test-crate crate:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{ crate }}" in
      pchronicle) cargo test -p persisting-pchronicle ;;
      control) cargo test -p persisting-control ;;
      core) cargo test -p persisting-core ;;
      capture) cargo test -p persisting-gateway ;;
      cli) cargo test -p persisting-cli ;;
      ppilot|compute) cargo test -p persisting-ppilot ;;
      pvisor) cargo test -p persisting-pvisor ;;
      dlcapt) cargo test -p persisting-dlcapt ;;
      *) echo "unknown crate: {{ crate }} (pchronicle|control|core|capture|cli|ppilot|pvisor|dlcapt)" >&2; exit 2 ;;
    esac

test-rust:
    cargo test --workspace

test-capture-claude:
    cargo test -p persisting-gateway --test capture_apps_claude

test-capture-fixtures:
    cargo test -p persisting-gateway --test llm_fixtures --test ag_fixture_tests

test-capture-network:
    cargo test -p persisting-gateway --lib network_policy
    cargo test -p persisting-gateway --test network_policy_http

test-search-integration:
    cargo test -p persisting-pchronicle --test search_integration

# pChronicle 库单测
[group('test')]
test-pchronicle:
    cargo test -p persisting-pchronicle --lib

# Rust + Python
test: test-rust test-py

# ── Python ───────────────────────────────────────────────────────────────────

py-sync:
    uv sync --all-extras

# 调试迭代（更快）
py-dev:
    uv run maturin develop

# 接近发布形态
py-dev-release:
    uv run maturin develop --release

test-py:
    uv run pytest tests/ -q

test-py-v:
    uv run pytest tests/ -v

# 安装本地 nightly 脚本自检（需已有 GitHub nightly release）
install-nightly:
    bash "{{ repo }}/scripts/install-nightly.sh"

# 从 nightly release 安装统一 CLI 组件集（persisting/pvisor/ppilot）
install-cli-nightly:
    bash "{{ repo }}/scripts/install-cli-nightly.sh"

# 从 nightly release 安装 guest pVisor runtime（Container/KVM executor 用）
install-guest-runtimes platform="linux-amd64":
    bash "{{ repo }}/scripts/install-guest-runtimes.sh" --platform {{ platform }}

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

# ── Trajectory CLI 集成（scripts/integration/*.sh）────────────────────

# 默认集成入口：轨迹 import → stats → judge → stats
integration profile="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    export PERSISTING_BUILD_PROFILE="{{ profile }}"
    [[ "${SKIP_REBUILD:-0}" == "1" || "${SKIP_BUILD:-0}" == "1" ]] && export SKIP_BUILD=1
    echo "==> traj integration (profile={{ profile }})"
    bash "{{ repo }}/scripts/integration/traj_e2e.sh"

check profile="debug":
    just test-pchronicle
    just integration profile="{{ profile }}"

smoke profile="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    export PERSISTING_BUILD_PROFILE="{{ profile }}"
    [[ "${SKIP_REBUILD:-0}" == "1" || "${SKIP_BUILD:-0}" == "1" ]] && export SKIP_BUILD=1
    echo "==> traj smoke (profile={{ profile }})"
    bash "{{ repo }}/scripts/integration/traj_e2e.sh"

check-quick profile="debug":
    just test-pchronicle
    just smoke profile="{{ profile }}"

# ── Capture 集成（scripts/integration/*.sh，build 由 _common.sh 处理）────────

# 轨迹子命令：history import/stats → eval judge/stats
# 跳过 rebuild：SKIP_BUILD=1 或 SKIP_REBUILD=1
traj-e2e profile="debug" cli_bin="":
    #!/usr/bin/env bash
    set -euo pipefail
    export PERSISTING_BUILD_PROFILE="{{ profile }}"
    [[ "${SKIP_REBUILD:-0}" == "1" || "${SKIP_BUILD:-0}" == "1" ]] && export SKIP_BUILD=1
    [[ -n "{{ cli_bin }}" ]] && export PERSISTING_CLI="{{ cli_bin }}"
    echo "==> traj-e2e (profile={{ profile }})"
    bash "{{ repo }}/scripts/integration/traj_e2e.sh"

capture-integration profile="debug" cli_bin="":
    #!/usr/bin/env bash
    set -euo pipefail
    export PERSISTING_BUILD_PROFILE="{{ profile }}"
    [[ "${SKIP_REBUILD:-0}" == "1" || "${SKIP_BUILD:-0}" == "1" ]] && export SKIP_BUILD=1
    [[ -n "{{ cli_bin }}" ]] && export PERSISTING_CLI="{{ cli_bin }}"
    echo "==> capture-integration (profile={{ profile }})"
    bash "{{ repo }}/scripts/integration/capture_integration.sh"

capture-stress profile="debug" cli_bin="" requests="80" concurrency="8":
    #!/usr/bin/env bash
    set -euo pipefail
    export PERSISTING_BUILD_PROFILE="{{ profile }}"
    export REQUESTS="{{ requests }}"
    export CONCURRENCY="{{ concurrency }}"
    [[ "${SKIP_REBUILD:-0}" == "1" || "${SKIP_BUILD:-0}" == "1" ]] && export SKIP_BUILD=1
    [[ -n "{{ cli_bin }}" ]] && export PERSISTING_CLI="{{ cli_bin }}"
    echo "==> capture-stress requests={{ requests }} concurrency={{ concurrency }}"
    bash "{{ repo }}/scripts/integration/capture_stress.sh"

capture-run-e2e profile="debug" cli_bin="" turns="3":
    #!/usr/bin/env bash
    set -euo pipefail
    export PERSISTING_BUILD_PROFILE="{{ profile }}"
    export TURNS="{{ turns }}"
    [[ "${SKIP_REBUILD:-0}" == "1" || "${SKIP_BUILD:-0}" == "1" ]] && export SKIP_BUILD=1
    [[ -n "{{ cli_bin }}" ]] && export PERSISTING_CLI="{{ cli_bin }}"
    echo "==> capture-run-e2e turns={{ turns }}"
    bash "{{ repo }}/scripts/integration/capture_run_e2e.sh"

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

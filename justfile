# Persisting — 仓库主任务入口
# 安装：brew install just / cargo install just
#
repo := justfile_directory()
docs_dir := repo / "docs"
gen_py := repo / "scripts" / "generate_benchmark_data.py"

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
    @echo "  just ci                  # CI 近似全量"
    @echo "  just py-dev              # 同步纯 Python 开发环境"
    @echo "  just install-cli         # 安装 pchronicle、pvisor 和 ppilot"
    @echo "  just pvisor              # 构建 release pVisor；macOS 自动签名"
    @echo "  just chronicle-binary    # 构建可直接测试的 pChronicle UI binary"
    @echo "  just echo                # 启动确定性的本地 LLM Echo upstream"
    @echo "  just benchmark-gateway   # Gateway 转发与持久化黑盒压测"
    @echo "  just benchmark-gateway-replay # 回放 examples/data 并生成人工 review bundle"
    @echo "  just build-wheel         # 打 release wheel → dist/"
    @echo "  just docs-serve          # 本地文档"

# ── 测试套件导航 ──────────────────────────────────────────────────────────────

# 列出推荐测试。
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
        just regression           大规模黑盒回归（按场景运行 tests/regression）
        just gateway-fuzz         一分钟 Gateway 四类 fuzz 汇总
        just gateway-fuzz-formats / gateway-fuzz-forwarding
        just gateway-fuzz-storage / gateway-fuzz-network

      组件示例
        just examples-pvisor / examples-pchronicle / examples-ppilot
    EOF

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
    cargo build --release -q -p persisting-pchronicle-cli --bin pchronicle
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
    cargo build --release -q \
      -p persisting-pchronicle-cli --bin pchronicle \
      -p persisting-ppilot --bin ppilot
    cargo build --release -q \
      -p persisting-pvisor --bin pvisor
    for example in "{{ repo }}"/examples/ppilot/*; do
        [[ -f "$example/run.sh" ]] || continue
        echo "==> ${example#"{{ repo }}/"}/run.sh"
        (cd "$example" && bash run.sh)
    done

[group('test')]
examples: examples-pvisor examples-pchronicle examples-ppilot

# Run repository-level black-box regression scenarios against prebuilt real
# component binaries. Long-running scenarios are excluded from this sweep.
[group('test')]
regression:
    bash tests/regression/run.sh

# Run the four Gateway fuzz contracts (about one minute total by default).
# Override duration, concurrency, and rate through PERSISTING_FUZZ_* variables.
[group('test')]
gateway-fuzz:
    bash tests/regression/gateway-fuzz/run.sh

[group('test')]
gateway-fuzz-formats:
    bash tests/regression/gateway-fuzz/formats/run.sh

[group('test')]
gateway-fuzz-forwarding:
    bash tests/regression/gateway-fuzz/forwarding/run.sh

[group('test')]
gateway-fuzz-storage:
    bash tests/regression/gateway-fuzz/storage/run.sh

[group('test')]
gateway-fuzz-network:
    bash tests/regression/gateway-fuzz/network-policy/run.sh

# Benchmark Gateway forwarding, typed capture, WAL, and durable Lance append
# against the deterministic local Echo upstream.
# Set PERSISTING_KEEP_TEST_ARTIFACTS=1 to retain the Dataset, WAL, and logs.
# Usage: `just benchmark-gateway`, or `just benchmark-gateway 30 32 1024 0 16 2048`.
[group('benchmark')]
benchmark-gateway duration="10" concurrency="16" payload_bytes="256" warmup="0" sessions="16" requests="1024":
    #!/usr/bin/env bash
    set -euo pipefail
    extra_args=()
    if [[ "${PERSISTING_KEEP_TEST_ARTIFACTS:-0}" == "1" ]]; then
      extra_args+=(--keep-artifacts)
    fi
    bash benchmark/gateway/run.sh \
      --duration "{{ duration }}" \
      --concurrency "{{ concurrency }}" \
      --sessions "{{ sessions }}" \
      --requests "{{ requests }}" \
      --payload-bytes "{{ payload_bytes }}" \
      --warmup "{{ warmup }}" \
      "${extra_args[@]}"

# Replay every supported trajectory example through Gateway and write a
# timestamped, human-reviewable bundle. Both arguments accept relative paths.
[group('benchmark')]
benchmark-gateway-replay data="examples/data" output="benchmark/gateway/results/replay-review":
    bash benchmark/gateway/replay.sh \
      --data "{{ data }}" \
      --output "{{ output }}"

# Find Gateway's saturation point with a closed-loop concurrency sweep. This
# consumes the existing release binary and deliberately skips the Echo baseline.
# Usage: `just benchmark-gateway-sweep`, or
# `just benchmark-gateway-sweep 10 "1 2 4 8 16 32 64" 256 0 16 256`.
[group('benchmark')]
benchmark-gateway-sweep duration="5" concurrencies="1 2 4 8 16 32" payload_bytes="256" warmup="0" sessions="16" requests="256":
    #!/usr/bin/env bash
    set -euo pipefail
    for concurrency in {{ concurrencies }}; do
      printf '\n==> Gateway concurrency %s\n' "$concurrency"
      bash benchmark/gateway/run.sh \
        --duration "{{ duration }}" \
        --concurrency "$concurrency" \
        --sessions "{{ sessions }}" \
        --requests "{{ requests }}" \
        --payload-bytes "{{ payload_bytes }}" \
        --warmup "{{ warmup }}" \
        --skip-baseline \
        --output "benchmark/gateway/results/sweep-c${concurrency}.json"
    done

# Run the unified Criterion + hyperfine pChronicle smoke benchmark and render
# raw JSON, Markdown, HTML, and a Bencher-compatible metric projection.
[group('benchmark')]
benchmark-pchronicle suite="smoke" output="target/pchronicle-benchmark/current":
    python3 benchmark/pchronicle/bench.py run \
      --suite "{{ suite }}" \
      --output "{{ output }}"

# Compare two pChronicle raw reports generated on the same testbed.
[group('benchmark')]
benchmark-pchronicle-compare baseline candidate output="target/pchronicle-benchmark/comparison":
    python3 benchmark/pchronicle/bench.py compare \
      --baseline "{{ baseline }}" \
      --candidate "{{ candidate }}" \
      --output "{{ output }}"

# ── 构建 ─────────────────────────────────────────────────────────────────────

# Start the deterministic local LLM upstream used to test Gateway forwarding,
# model/protocol rewriting, streaming, and capture.
# Usage: `just echo`, or `just echo 127.0.0.1:19080 base64`.
[group('build')]
echo listen="127.0.0.1:19080" encoding="plain":
    target/release/pchronicle echo --listen "{{ listen }}" --encoding "{{ encoding }}"

# Build the Dioxus trajectory workbench for compile-time embedding.
chronicle-web-build:
    python3 scripts/packaging/stage_wheel_binaries.py --web-only

# Start the Web frontend against a separately running pChronicle server.
chronicle-web-dev:
    cd pchronicle-web && dx serve

# Build the embedded Web UI and a directly runnable pChronicle test binary.
# Usage: `just chronicle-binary` or `just chronicle-binary release`.
[group('build')]
chronicle-binary profile="debug": chronicle-web-build
    #!/usr/bin/env bash
    set -euo pipefail
    profile="{{ profile }}"
    case "$profile" in
      debug)
        binary="{{ repo }}/target/debug/pchronicle"
        cargo build --locked -p persisting-pchronicle-cli --bin pchronicle
        ;;
      release)
        binary="{{ repo }}/target/release/pchronicle"
        cargo build --locked -p persisting-pchronicle-cli --bin pchronicle --release
        ;;
      *)
        echo "unsupported pChronicle profile: $profile (expected debug or release)" >&2
        exit 2
        ;;
    esac

    test -x "$binary"
    "$binary" serve --help >/dev/null
    printf 'Built pChronicle test binary: %s\n' "$binary"
    printf 'Run: %s serve --warehouse %s\n' "$binary" "{{ repo }}/data"

build profile="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    profile="{{ profile }}"
    case "$profile" in
      debug) cargo_args=() ;;
      release) cargo_args=(--release) ;;
      *)
        echo "unsupported build profile: $profile (expected debug or release)" >&2
        exit 2
        ;;
    esac
    # Build the pChronicle storage service beside its lightweight pPilot client.
    # pPilot launches this binary for durable control and does not link Lance.
    cargo build \
      -p persisting-pchronicle-cli --bin pchronicle \
      -p persisting-ppilot --bin ppilot \
      "${cargo_args[@]}"

    # pPilot invokes pVisor at runtime instead of linking it. The default
    # pVisor profile includes local Lance Chronicle for durable Attempt state
    # while keeping cloud object-store SDKs excluded.
    cargo build -p persisting-pvisor --bin pvisor \
      "${cargo_args[@]}"

build-release:
    just build release

# Build pVisor and add the Hypervisor entitlement required by macOS/HVF.
# Usage: `just pvisor` (release) or `just pvisor debug`.
[group('build')]
pvisor profile="release":
    #!/usr/bin/env bash
    set -euo pipefail
    profile="{{ profile }}"
    case "$profile" in
      release)
        cargo_args=(--release)
        binary="{{ repo }}/target/release/pvisor"
        ;;
      debug)
        cargo_args=()
        binary="{{ repo }}/target/debug/pvisor"
        ;;
      *)
        echo "unsupported pVisor profile: $profile (expected release or debug)" >&2
        exit 2
        ;;
    esac

    cargo build --locked -p persisting-pvisor --bin pvisor "${cargo_args[@]}"

    if [[ "$(uname -s)" == "Darwin" ]]; then
        entitlements="{{ repo }}/crates/persisting-pvisor/macos-hypervisor.entitlements"
        command -v codesign >/dev/null
        codesign --force --sign - --entitlements "$entitlements" "$binary"
        codesign --verify --strict --verbose=2 "$binary"
        codesign -d --entitlements :- "$binary" 2>&1 \
          | grep -q 'com.apple.security.hypervisor'
        echo "Built and signed pVisor: $binary"
    else
        echo "Built pVisor: $binary"
    fi

# Install the three product CLIs.
install-cli:
    #!/usr/bin/env bash
    set -euo pipefail
    install_root="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
    cargo install --path crates/persisting-pchronicle-cli --locked --force --root "$install_root"
    cargo install --path crates/persisting-pvisor --locked --force --root "$install_root"
    cargo install --path crates/persisting-ppilot --locked --force --root "$install_root"
    printf 'Installed Persisting component set in %s/bin\n' "$install_root"

# PEP 517 release wheel（Python package + pchronicle/pvisor/ppilot）→ dist/
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
      --config-setting 'cargo-profile=dev'
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

# CI 近似：gate + 构建 + Gateway fixture 回归
ci:
    just gate
    just build
    just test-capture-claude

# ── Rust 测试 ─────────────────────────────────────────────────────────────────

# 单 crate：pchronicle | pchronicle-cli | agentctl | capture | ppilot | pvisor | dlcapt
test-crate crate:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{ crate }}" in
      pchronicle) cargo test -p persisting-pchronicle ;;
      pchronicle-cli) cargo test -p persisting-pchronicle-cli ;;
      agentctl) cargo test -p persisting-agentctl ;;
      capture) cargo test -p persisting-gateway ;;
      ppilot)
        cargo build -p persisting-pvisor --bin pvisor
        cargo test -p persisting-ppilot
        ;;
      pvisor) cargo test -p persisting-pvisor ;;
      dlcapt) cargo test -p persisting-dlcapt ;;
      *) echo "unknown crate: {{ crate }} (pchronicle|pchronicle-cli|agentctl|capture|ppilot|pvisor|dlcapt)" >&2; exit 2 ;;
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

# 同步纯 Python 开发环境
py-dev:
    uv sync --all-extras

test-py:
    uv run pytest tests/ -q

test-py-v:
    uv run pytest tests/ -v

# 安装本地 nightly 脚本自检（需已有 GitHub nightly release）
install-nightly:
    bash "{{ repo }}/scripts/install-nightly.sh"

# ── 文档（docs/ 子项目）──────────────────────────────────────────────────────

docs-sync:
    cd "{{ docs_dir }}" && uv sync --frozen

docs-serve:
    cd "{{ docs_dir }}" && uv run mkdocs serve

docs-serve-dirty:
    cd "{{ docs_dir }}" && uv run mkdocs serve --dirtyreload

docs-build:
    cd "{{ docs_dir }}" && uv run mkdocs build

docs-links:
    cd "{{ docs_dir }}" && uv run mkdocs build --strict

# ── 数据与 fixture ───────────────────────────────────────────────────────────

# 生成 search/traj 基准数据。
generate-benchmark search_rows="100" traj_rows="50" seed="42" search_out="" traj_out="":
    #!/usr/bin/env bash
    set -euo pipefail
    gen_py="{{ gen_py }}"
    [[ -f "$gen_py" ]] || { echo "missing $gen_py" >&2; exit 1; }
    args=(--seed "{{ seed }}" --search-rows "{{ search_rows }}" --traj-rows "{{ traj_rows }}")
    [[ -n "{{ search_out }}" ]] && args+=(--search-out "{{ search_out }}")
    [[ -n "{{ traj_out }}" ]] && args+=(--traj-out "{{ traj_out }}")
    python3 "$gen_py" "${args[@]}"

check:
    just test-pchronicle

check-quick:
    just test-pchronicle

# capture 相关 Rust 测试
capture-test:
    just test-crate capture
    just test-capture-fixtures
    just test-capture-claude

# 完整 Gateway 验证。
capture-check:
    just capture-test

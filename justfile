# Persisting — 仓库主任务入口
# 安装：brew install just / cargo install just
#
repo := justfile_directory()
docs_dir := repo / "docs"
gen_py := repo / "scripts" / "generate_benchmark_data.py"

# Python checks follow the active pChronicle scope in AGENTS.md.
python_tests := "tests/test_release_packaging.py tests/test_trajectory_dialogue.py benchmark/pchronicle"
ruff_paths := "scripts/packaging scripts/ci scripts/build-docs.py scripts/check-docs.py scripts/serve-docs.py scripts/run-pchronicle-cases.py " + python_tests

# ── 帮助 ─────────────────────────────────────────────────────────────────────

default:
    @just --list --unsorted
    @echo ""
    @echo "常用："
    @echo "  just dev                 # 提交前（fmt + lint + test-rust）"
    @echo "  just test [package]      # 日常功能测试（可指定 Cargo 包）"
    @echo "  just proptest pchronicle # pChronicle 全量 Proptest 回归"
    @echo "  just ci                  # CI 近似全量"
    @echo "  just py-dev              # 同步纯 Python 开发环境"
    @echo "  just install-cli         # 安装 pchronicle"
    @echo "  just chronicle-binary    # 构建可直接测试的 pChronicle UI binary"
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
        just dev                  提交前：fmt + lint + test-rust
        just ci                   CI 近似
        just test [package]        日常功能测试；可指定 Cargo 包
        just proptest pchronicle   pChronicle 全量性质测试回归
        just test-py  其他定向测试入口
        （Rust 测试由 cargo nextest 执行；文档测试仍用 cargo test）
        just regression           大规模黑盒回归（按场景运行 tests/regression）
        just cases pchronicle|pchronicle-cluster

      组件示例
        just examples-pchronicle

    EOF

# Run the deterministic, quantitative pChronicle examples.
[group('test')]
examples-pchronicle:
    #!/usr/bin/env bash
    set -euo pipefail
    just build release
    cargo build --release --locked -p persisting-pchronicle --example pchronicle_storage_query_benchmark
    bash "{{ repo }}/examples/pchronicle/test.sh" --profile release
    bash "{{ repo }}/examples/pchronicle/output-contract.sh" >/dev/null

[group('test')]
examples: examples-pchronicle

# Run repository-level black-box regression scenarios against prebuilt real
# component binaries. Long-running scenarios are excluded from this sweep.
[group('test')]
regression:
    bash tests/regression/run.sh

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
      debug|release) ;;
      *)
        echo "unsupported pChronicle profile: $profile (expected debug or release)" >&2
        exit 2
        ;;
    esac
    just build "$profile"
    binary="{{ repo }}/target/$profile/pchronicle"
    test -x "$binary"
    "$binary" serve --help >/dev/null
    printf 'Built pChronicle test binary: %s\n' "$binary"
    printf 'Run: %s serve --warehouse %s\n' "$binary" "{{ repo }}/data"

# Build the pChronicle CLI; use chronicle-binary to embed the Web UI.
[group('build')]
build profile="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{ profile }}" in
      debug) cargo_profile=dev ;;
      release) cargo_profile=release ;;
      *) echo "unsupported build profile: {{ profile }} (expected debug or release)" >&2; exit 2 ;;
    esac
    cargo build --profile "$cargo_profile" --locked -p persisting-pchronicle-cli --bin pchronicle

# Install the pChronicle CLI.
install-cli:
    #!/usr/bin/env bash
    set -euo pipefail
    install_root="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
    cargo install --path crates/persisting-pchronicle-cli --locked --force --root "$install_root"
    printf 'Installed pchronicle in %s/bin\n' "$install_root"

# PEP 517 release wheel（Python package + pchronicle）→ dist/
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
    cargo clean --manifest-path pchronicle-web/Cargo.toml
    rm -rf dist target/wheels .venv htmlcov .coverage coverage.xml

# ── 格式化 / Lint ─────────────────────────────────────────────────────────────

# 格式化 Rust + Python（会改写文件）
fmt: fmt-rust fmt-py

fmt-rust:
    cargo fmt -p persisting-pchronicle -p persisting-pchronicle-cli
    cargo fmt --manifest-path pchronicle-web/Cargo.toml

fmt-py:
    uvx ruff format {{ ruff_paths }}

# 只检查格式，不改写（CI / pre-commit）
fmt-check: fmt-check-rust fmt-check-py

fmt-check-rust:
    cargo fmt -p persisting-pchronicle -p persisting-pchronicle-cli -- --check
    cargo fmt --manifest-path pchronicle-web/Cargo.toml -- --check

fmt-check-py:
    uvx ruff format --check {{ ruff_paths }}

# clippy + ruff（不改写）
lint: lint-rust lint-py

lint-rust: clippy-deny clippy-pchronicle-web clippy-pchronicle-features

lint-py:
    uvx ruff check {{ ruff_paths }}

# Compatibility alias; Python lint already covers in-scope scripts and tests.
lint-py-all: lint-py

clippy-deny:
    cargo clippy -p persisting-pchronicle -p persisting-pchronicle-cli --all-targets --locked -- -D warnings
    cargo clippy -p persisting-pchronicle --lib --locked -- -D warnings -D clippy::unwrap_used -D clippy::expect_used -D clippy::unreachable

# pchronicle-web is a separate Cargo workspace and has its own Clippy check.
clippy-pchronicle-web:
    cargo clippy --manifest-path pchronicle-web/Cargo.toml --all-targets --locked -- -D warnings

clippy-pchronicle-features:
    cargo clippy -p persisting-pchronicle --lib --no-default-features --locked -- -D warnings -D clippy::unwrap_used -D clippy::expect_used -D clippy::unreachable
    cargo clippy -p persisting-pchronicle --lib --no-default-features --features lance-store --locked -- -D warnings -D clippy::unwrap_used -D clippy::expect_used -D clippy::unreachable
    cargo clippy -p persisting-pchronicle --lib --no-default-features --features oss-store --locked -- -D warnings -D clippy::unwrap_used -D clippy::expect_used -D clippy::unreachable

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

# fmt + lint + Rust 测试（日常 / 提交前）
[group('test')]
dev:
    just fmt
    just lint
    just test-rust

# 与 GitHub Actions `ci.yml` lint 对齐（只检查、不改写）
ci-lint:
    just fmt-check-rust
    just lint-rust
    just lint-py

# CI 近似：功能门禁 + Proptest 回归 + 构建
ci:
    just ci-lint
    just test
    just proptest pchronicle
    just build

# ── Rust 测试 ─────────────────────────────────────────────────────────────────

# 单 crate：pchronicle（含 CLI，对齐 CI pchronicle shard）| pchronicle-cli | …
test-crate crate:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{ crate }}" in
      pchronicle)
        cargo nextest run --locked \
          -p persisting-pchronicle \
          -p persisting-pchronicle-cli
        ;;
      pchronicle-cli) cargo nextest run -p persisting-pchronicle-cli --locked ;;
      dlcapt) cargo test -p persisting-dlcapt ;;
      *) echo "unknown crate: {{ crate }} (pchronicle|pchronicle-cli|dlcapt)" >&2; exit 2 ;;
    esac

test-rust package="":
    #!/usr/bin/env bash
    set -euo pipefail
    package="{{ package }}"
    if [[ -n "$package" ]]; then
        cargo nextest run --locked -p "$package"
    else
        just test-crate pchronicle
    fi

[group('test')]
smoke-pchronicle-cli:
    just build debug
    target/debug/pchronicle query --help >/dev/null

# Separate Cargo workspace covering the Dioxus trajectory workbench.
[group('test')]
test-pchronicle-web:
    cargo nextest run --manifest-path pchronicle-web/Cargo.toml --locked

# Real S3/MinIO contract (ignored by default; requires PCHRONICLE_S3_TEST_URI).
[group('test')]
test-pchronicle-s3:
    cargo nextest run -p persisting-pchronicle --test s3_storage --locked --run-ignored all

test-search-integration:
    cargo test -p persisting-pchronicle --test search_integration

# 按包执行全量性质测试回归，例如：`just proptest pchronicle`。
[group('test')]
proptest package:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{ package }}" in
      pchronicle)
        cargo nextest run -p persisting-pchronicle --features proptest --locked \
          -E 'test(/proptest/) or binary(proptest_*)'
        ;;
      *)
        echo "unknown proptest package: {{ package }} (pchronicle)" >&2
        exit 2
        ;;
    esac

# Rust + Python. Rust tests run debug-mode nextest for faster iteration; use
# `just test-rust` with a package for targeted coverage. Passing a package runs
# only that Rust package; `pchronicle` also runs persisting-pchronicle-cli.
# In-scope Python tests run only for the no-argument invocation.
test package="":
    #!/usr/bin/env bash
    set -euo pipefail
    package="{{ package }}"
    if [[ -z "$package" ]]; then
      just test-rust
      just test-py
      exit 0
    fi
    case "$package" in
      pchronicle|pchronicle-cli|dlcapt)
        just test-crate "$package"
        ;;
      *)
        just test-rust "$package"
        ;;
    esac

# ── Python ───────────────────────────────────────────────────────────────────

# 同步纯 Python 开发环境
py-dev:
    uv sync --all-extras

test-py:
    uv run --extra dev pytest {{ python_tests }} -q

test-py-v:
    uv run --extra dev pytest {{ python_tests }} -v

# 安装本地 nightly 脚本自检（需已有 GitHub nightly release）
install-nightly:
    bash "{{ repo }}/scripts/install-nightly.sh"

# ── 文档（docs/ 子项目）──────────────────────────────────────────────────────

docs-sync:
    cd "{{ docs_dir }}" && if [[ ! -x .venv/bin/zensical ]]; then uv venv .venv && UV_CACHE_DIR=/tmp/uv-cache uv pip install --python .venv/bin/python zensical==0.0.61; fi

docs-serve: docs-sync
    cd "{{ docs_dir }}" && .venv/bin/python "{{ repo }}/scripts/build-docs.py" && .venv/bin/python "{{ repo }}/scripts/serve-docs.py" --host 127.0.0.1 --port 3000 --directory site

docs-serve-dirty: docs-sync
    cd "{{ docs_dir }}" && .venv/bin/python "{{ repo }}/scripts/build-docs.py" && .venv/bin/python "{{ repo }}/scripts/serve-docs.py" --host 127.0.0.1 --port 3000 --directory site --watch

docs-build: docs-sync
    cd "{{ docs_dir }}" && .venv/bin/python "{{ repo }}/scripts/build-docs.py"

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

check-quick:
    cargo check -p persisting-pchronicle-cli --locked
    cargo check -p persisting-pchronicle --no-default-features --locked

# Execute pChronicle single-machine/self-service cases.
[group('test')]
test-pchronicle-cases:
    just build release
    python3 scripts/run-pchronicle-cases.py --document docs/src/zh/pchronicle/reference/cases-self.md --pchronicle target/release/pchronicle --report target/pchronicle-self-case-report.md

# List and execute pChronicle platform/Catalog cases. Server lifecycle cases are
# reported as MANUAL unless explicitly selected with PCHRONICLE_CASE_MODE.
[group('test')]
test-pchronicle-cases-platform:
    just build release
    python3 scripts/run-pchronicle-cases.py --document docs/src/zh/pchronicle/reference/cases-platform.md --pchronicle target/release/pchronicle --report target/pchronicle-platform-case-report.md

# Run documented integration cases by component.
# Examples:
#   just cases pchronicle
#   just cases pchronicle-cluster
# Extra runner flags can be passed directly, e.g.
[group('test')]
cases target *args:
    #!/usr/bin/env bash
    set -euo pipefail
    # Variadic args are interpolated by just (not shebang $@). A leading `--`
    # may be present when callers stop just flag parsing; strip it.
    set -- {{ args }}
    if [[ "${1:-}" == "--" ]]; then
      shift
    fi
    case "{{target}}" in
      pchronicle)
        just build release
        python3 scripts/run-pchronicle-cases.py --document docs/src/zh/pchronicle/reference/cases-self.md --pchronicle "{{ repo }}/target/release/pchronicle" --report target/pchronicle-self-case-report.md "$@"
        ;;
      pchronicle-cluster)
        just build release
        python3 scripts/run-pchronicle-cases.py --document docs/src/zh/pchronicle/reference/cases-platform.md --pchronicle "{{ repo }}/target/release/pchronicle" --report target/pchronicle-platform-case-report.md "$@"
        ;;
      *)
        echo "usage: just cases pchronicle|pchronicle-cluster [runner-args...]" >&2
        exit 2
        ;;
    esac

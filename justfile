# Persisting — 仓库主任务入口
# 安装：brew install just / cargo install just
#
repo := justfile_directory()
docs_dir := repo / "docs"
gen_py := repo / "scripts" / "generate_benchmark_data.py"

# Product CLI component set. Keep all Cargo build entry points below routed
# through `build-components` so package/bin changes have one source in just.
component_pchronicle := "-p persisting-pchronicle-cli --bin pchronicle"
component_pvisor := "-p persisting-pvisor --bin pvisor"
component_ppilot := "-p persisting-ppilot --bin ppilot"

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
    @echo "  just test [package]      # 日常功能测试（可指定 Cargo 包）"
    @echo "  just proptest pchronicle # pChronicle 全量 Proptest 回归"
    @echo "  just ci                  # CI 近似全量"
    @echo "  just py-dev              # 同步纯 Python 开发环境"
    @echo "  just install-cli         # 安装 pchronicle、pvisor 和 ppilot"
    @echo "  just pvisor              # 构建 release pVisor；macOS 自动签名"
    @echo "  just examples-pvisor     # 构建并验证全部 pVisor examples"
    @echo "  just benchmark-pvisor    # pVisor 进程启动与 Bundle 访问基准"
    @echo "  just chronicle-binary    # 构建可直接测试的 pChronicle UI binary"
    @echo "  just echo                # 启动确定性的本地 LLM Echo upstream"
    @echo "  just benchmark-gateway   # Gateway 转发与持久化黑盒压测"
    @echo "  just benchmark-gateway-replay # 回放 examples/data 并生成人工 review bundle"
    @echo "  just build-wheel         # 打 release wheel → dist/"
    @echo "  just build-analysis      # nightly 构建分析（按需）"
    @echo "  just sanitize            # nightly Sanitizer 测试（按需）"
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
        just build-analysis       nightly 构建瓶颈分析（按需）
        just sanitize              nightly Sanitizer 测试（按需）
        just test [package]        日常功能测试；可指定 Cargo 包
        just proptest pchronicle   pChronicle 全量性质测试回归
        just capture-test / test-py  其他定向测试入口
        （Rust 测试由 cargo nextest 执行；文档测试仍用 cargo test）
        just regression           大规模黑盒回归（按场景运行 tests/regression）
        just gateway-fuzz         一分钟 Gateway 四类 fuzz 汇总
        just gateway-fuzz-formats / gateway-fuzz-forwarding
        just gateway-fuzz-storage / gateway-fuzz-network
        just cases pvisor|pchronicle|pchronicle-cluster

      组件示例
        just examples-pvisor              全部 pVisor 场景
        just examples-pvisor-filesystem   需要 FUSE 的 01/02 场景
        just examples-pvisor-portable     普通 runner 可跑的 03/04 场景
        just example-pvisor 03-network-isolation
        just examples-pchronicle / examples-ppilot

      pVisor 回归 / 基准
        just test-pvisor / test-pvisor-lance / test-pvisor-isolation
        just smoke-pvisor-cli
        just benchmark-pvisor             快速 smoke 基准
        just benchmark-pvisor nightly     稳定分布基准
    EOF

# Run and validate every deterministic, quantitative pVisor example.
[group('test')]
examples-pvisor profile="release": (pvisor profile)
    bash examples/pvisor/test.sh --profile "{{ profile }}" \
      01-filesystem-isolation \
      02-changeset-management \
      03-network-isolation \
      04-gateway-llm-control

# Run and validate the FUSE-backed workspace and changeset examples.
[group('test')]
examples-pvisor-filesystem profile="release": (pvisor profile)
    bash examples/pvisor/test.sh --profile "{{ profile }}" \
      01-filesystem-isolation 02-changeset-management

# Run and validate pVisor examples that do not require FUSE or user namespaces.
[group('test')]
examples-pvisor-portable profile="release": (pvisor profile)
    bash examples/pvisor/test.sh --profile "{{ profile }}" \
      03-network-isolation 04-gateway-llm-control

# Run and validate one named pVisor example.
[group('test')]
example-pvisor scenario profile="release": (pvisor profile)
    bash examples/pvisor/test.sh --profile "{{ profile }}" "{{ scenario }}"

# Run the deterministic, quantitative pChronicle examples.
[group('test')]
examples-pchronicle:
    #!/usr/bin/env bash
    set -euo pipefail
    just build-components release pchronicle-benchmark
    bash "{{ repo }}/examples/pchronicle/test.sh" --profile release
    bash "{{ repo }}/examples/pchronicle/output-contract.sh" >/dev/null

# Run the deterministic, quantitative pPilot examples.
[group('test')]
examples-ppilot:
    #!/usr/bin/env bash
    set -euo pipefail
    just build-components release all
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

# Run pVisor's process-level startup and durable Run Bundle benchmark. The
# smoke suite is intended for PR CI; nightly raises warmups and sample counts.
[group('benchmark')]
benchmark-pvisor suite="smoke" output="target/pvisor-benchmark/current" target_dir="target/pvisor-benchmark-build":
    bash benchmark/pvisor/run.sh run \
      --suite "{{ suite }}" \
      --output "{{ output }}" \
      --target-dir "{{ target_dir }}"

# Compare reports from the same host. A missing baseline is valid for the first
# commit that introduces the benchmark and produces a candidate-only report.
[group('benchmark')]
benchmark-pvisor-compare candidate baseline="" output="target/pvisor-benchmark/comparison" regression_threshold="15":
    bash benchmark/pvisor/run.sh compare \
      --candidate "{{ candidate }}" \
      --baseline "{{ baseline }}" \
      --output "{{ output }}" \
      --regression-threshold "{{ regression_threshold }}"

# Unit-test the benchmark report and comparison contract without running it.
[group('test')]
test-pvisor-benchmark:
    PYTHONDONTWRITEBYTECODE=1 python3 benchmark/pvisor/test_bench.py

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
        just build-components debug pchronicle
        ;;
      release)
        binary="{{ repo }}/target/release/pchronicle"
        just build-components release pchronicle
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
      debug|release) ;;
      *)
        echo "unsupported build profile: $profile (expected debug or release)" >&2
        exit 2
        ;;
    esac
    just build-components "$profile" all

# Build one product component, the benchmark binary, or the complete runtime component set.
# This is the single Cargo build entry point used by local recipes and CI.
[group('build')]
build-components profile="debug" components="all":
    #!/usr/bin/env bash
    set -euo pipefail
    profile="{{ profile }}"
    components="{{ components }}"
    case "$profile" in
      debug) cargo_profile=dev ;;
      release) cargo_profile=release ;;
      *) echo "unsupported build profile: $profile (expected debug or release)" >&2; exit 2 ;;
    esac
    case "$components" in
      all|runtime)
        cargo build --profile "$cargo_profile" --locked \
          {{ component_pchronicle }} {{ component_pvisor }} {{ component_ppilot }}
        ;;
      pchronicle)
        cargo build --profile "$cargo_profile" --locked {{ component_pchronicle }}
        ;;
      pchronicle-benchmark)
        cargo build --profile "$cargo_profile" --locked \
          {{ component_pchronicle }} \
          -p persisting-pchronicle --example pchronicle_storage_query_benchmark
        ;;
      pvisor-pchronicle)
        cargo build --profile "$cargo_profile" --locked \
          {{ component_pvisor }} {{ component_pchronicle }}
        ;;
      pvisor)
        cargo build --profile "$cargo_profile" --locked {{ component_pvisor }}
        ;;
      ppilot)
        cargo build --profile "$cargo_profile" --locked {{ component_ppilot }}
        ;;
      pchronicle-ppilot)
        cargo build --profile "$cargo_profile" --locked \
          {{ component_pchronicle }} {{ component_ppilot }}
        ;;
      *)
        echo "unsupported component set: $components (all|runtime|pchronicle|pchronicle-benchmark|pvisor|pvisor-pchronicle|ppilot|pchronicle-ppilot)" >&2
        exit 2
        ;;
    esac

build-release:
    just build release

# Collect detailed Cargo build metrics without enabling the overhead for every
# normal build. Reports are persisted under CARGO_HOME/log and can be queried
# with `just build-analysis-report`.
[group('diagnostics')]
build-analysis package="persisting-pvisor" target_dir="target/build-analysis":
    #!/usr/bin/env bash
    set -euo pipefail
    cargo +nightly -Zbuild-analysis \
      --config unstable.build-analysis=true \
      --config build.analysis.enabled=true \
      build --locked --timings --target-dir "{{ target_dir }}" -p "{{ package }}"
    echo "Build analysis recorded. Run: just build-analysis-report"

# Query metrics collected by build-analysis. `report` may be sessions, timings,
# or rebuilds; timings/rebuilds use the most recent session by default.
[group('diagnostics')]
build-analysis-report report="sessions":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{ report }}" in
      sessions|timings|rebuilds) ;;
      *) echo "unsupported report: {{ report }} (sessions|timings|rebuilds)" >&2; exit 2 ;;
    esac
    cargo +nightly -Zbuild-analysis report "{{ report }}"

# Run one crate's tests with a nightly sanitizer. Sanitizers require LLVM
# instrumentation and a rebuilt standard library, and are substantially slower.
[group('diagnostics')]
sanitize sanitizer="address" package="persisting-agentctl":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{ sanitizer }}" in
      address|leak|thread|undefined) ;;
      *) echo "unsupported sanitizer: {{ sanitizer }} (address|leak|thread|undefined)" >&2; exit 2 ;;
    esac
    host="$$(rustup run nightly rustc -vV | sed -n 's/^host: //p')"
    [[ -n "$$host" ]] || { echo "unable to determine nightly target; install nightly rustc" >&2; exit 1; }
    export RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }-Zsanitizer={{ sanitizer }}"
    cargo +nightly -Zbuild-std \
      test --locked --target "$$host" --target-dir "target/sanitizer/{{ sanitizer }}" \
      -p "{{ package }}" --lib

# Build pVisor and add the Hypervisor entitlement required by macOS/HVF.
# Usage: `just pvisor` (release) or `just pvisor debug`.
[group('build')]
pvisor profile="release":
    #!/usr/bin/env bash
    set -euo pipefail
    profile="{{ profile }}"
    case "$profile" in
      release)
        binary="{{ repo }}/target/release/pvisor"
        just build-components release pvisor
        ;;
      debug)
        binary="{{ repo }}/target/debug/pvisor"
        just build-components debug pvisor
        ;;
      *)
        echo "unsupported pVisor profile: $profile (expected release or debug)" >&2
        exit 2
        ;;
    esac

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
    cargo fmt --manifest-path pchronicle-web/Cargo.toml

fmt-py:
    uvx ruff format {{ ruff_paths }}

# 只检查格式，不改写（CI / pre-commit）
fmt-check: fmt-check-rust fmt-check-py

fmt-check-rust:
    cargo fmt --all -- --check
    cargo fmt --manifest-path pchronicle-web/Cargo.toml -- --check

fmt-check-py:
    uvx ruff format --check {{ ruff_paths }}

# clippy + ruff（不改写）
lint: lint-rust lint-py

lint-rust: clippy-deny clippy-pchronicle-web clippy-pchronicle-panics clippy-pchronicle-features

lint-py:
    uvx ruff check {{ ruff_lint_paths }}

# 含 tests/examples（较严，可能有存量告警）
lint-py-all:
    uvx ruff check {{ ruff_paths }}

clippy-deny:
    cargo clippy --workspace --exclude persisting-dlcapt --all-targets --locked -- -D warnings

# pchronicle-web is a separate Cargo workspace and is not covered by the root
# workspace Clippy invocation above.
clippy-pchronicle-web:
    cargo clippy --manifest-path pchronicle-web/Cargo.toml --all-targets --locked -- -D warnings

clippy-pchronicle-panics:
    cargo clippy -p persisting-pchronicle --lib --locked -- -D warnings -D clippy::unwrap_used -D clippy::expect_used -D clippy::unreachable

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

# 日常开发快捷路径。完整 workspace/all-targets lint 仍由 gate/CI 负责；这里
# 只检查日常会改动的运行时 crate，并用无存储 feature 的 pChronicle 核心检查
# 保持增量反馈快速。
dev:
    just fmt
    cargo clippy \
      -p persisting-agentctl \
      -p persisting-events \
      -p persisting-gateway \
      -p persisting-ppilot \
      -p persisting-pvisor \
      --lib --bins --locked -- -D warnings
    just check-quick

# CI 近似：功能门禁 + Proptest 回归 + 构建
ci:
    just gate
    just proptest pchronicle
    just build

# ── Rust 测试 ─────────────────────────────────────────────────────────────────

# 单 crate：pchronicle | pchronicle-cli | agentctl | capture | ppilot | pvisor | dlcapt
test-crate crate:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{ crate }}" in
      pchronicle) cargo nextest run -p persisting-pchronicle --locked ;;
      pchronicle-cli) cargo nextest run -p persisting-pchronicle-cli --locked ;;
      agentctl) cargo nextest run -p persisting-agentctl --locked ;;
      capture) cargo nextest run -p persisting-gateway --locked ;;
      ppilot)
        just build-components debug pvisor
        cargo nextest run -p persisting-ppilot --locked
        ;;
      pvisor) cargo nextest run -p persisting-pvisor --locked ;;
      dlcapt) cargo test -p persisting-dlcapt ;;
      *) echo "unknown crate: {{ crate }} (pchronicle|pchronicle-cli|agentctl|capture|ppilot|pvisor|dlcapt)" >&2; exit 2 ;;
    esac

test-rust package="":
    #!/usr/bin/env bash
    set -euo pipefail
    package="{{ package }}"
    if [[ -z "$package" || "$package" == "persisting-ppilot" ]]; then
        target_dir="${CARGO_TARGET_DIR:-$PWD/target}"
        if [[ "$target_dir" != /* ]]; then
            target_dir="$PWD/$target_dir"
        fi
        just build-components debug pvisor-pchronicle
        export PATH="$target_dir/debug:$PATH"
    fi
    if [[ -n "$package" ]]; then
        cargo nextest run --locked -p "$package"
    else
        cargo nextest run --workspace --exclude persisting-dlcapt --locked
    fi

# Default pVisor crate profile, including CLI and integration regressions.
[group('test')]
test-pvisor:
    cargo nextest run -p persisting-pvisor --locked

# Execute the bash blocks embedded in the pVisor Markdown case checklist.
# VM/container cases are skipped unless their host prerequisites are supplied;
# use --strict-skips through the script when a fully provisioned matrix is
# required.
[group('test')]
test-pvisor-cases:
    cargo build --release -p persisting-pvisor --locked
    python3 scripts/run-pvisor-cases.py --report target/pvisor-case-report.md

# Execute every documented case even when VM/container resources are absent;
# missing prerequisites become real case failures for diagnosis.
[group('test')]
test-pvisor-cases-all:
    cargo build --release -p persisting-pvisor --locked
    python3 scripts/run-pvisor-cases.py --run-unavailable --keep --report target/pvisor-case-report.md

# Mandatory pVisor ↔ pChronicle capture bridge feature profile.
[group('test')]
test-pvisor-lance:
    cargo nextest run -p persisting-pvisor --features lance-chronicle --locked

# Strict Linux rootless/FUSE boundary tests. This deliberately does not allow
# the optional-userns skip used by the broad cross-platform workspace job.
[group('test')]
test-pvisor-isolation:
    env -u PERSISTING_TEST_ALLOW_NO_USERNS \
      cargo nextest run -p persisting-pvisor --test rootless_local --locked -- --nocapture

# Product CLI surface exercised by CI after the debug component build.
[group('test')]
smoke-pvisor-cli:
    target/debug/pvisor run --help >/dev/null
    target/debug/pvisor status --help >/dev/null
    target/debug/pvisor review --help >/dev/null

test-capture-claude:
    cargo nextest run -p persisting-gateway --test capture_apps_claude --locked

test-capture-fixtures:
    cargo nextest run -p persisting-gateway --locked --test llm_fixtures --test ag_fixture_tests

test-capture-network:
    cargo nextest run -p persisting-gateway --locked --lib network_policy
    cargo nextest run -p persisting-gateway --locked --test network_policy_http

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
# only that Rust package; the full Python suite runs only for the no-argument
# repository-wide invocation.
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
      pchronicle|pchronicle-cli|agentctl|capture|ppilot|pvisor|dlcapt)
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
    uv run pytest tests/ -q

test-py-v:
    uv run pytest tests/ -v

# 安装本地 nightly 脚本自检（需已有 GitHub nightly release）
install-nightly:
    bash "{{ repo }}/scripts/install-nightly.sh"

# ── 文档（docs/ 子项目）──────────────────────────────────────────────────────

docs-sync:
    cd "{{ docs_dir }}" && if [[ ! -x node_modules/.bin/docusaurus ]]; then npm ci; fi

docs-serve: docs-sync
    cd "{{ docs_dir }}" && npm run build && python3 "{{ repo }}/scripts/serve-docs.py" --host 0.0.0.0 --port 3000 --directory build

# Hot-reload development mode. Use docs-serve for a stable static preview.
docs-serve-dirty: docs-sync
    cd "{{ docs_dir }}" && npm run start -- --host 0.0.0.0

docs-build: docs-sync
    cd "{{ docs_dir }}" && npm run build

docs-links: docs-sync
    cd "{{ docs_dir }}" && npm run build

# Fail if a translatable English page lacks a Chinese counterpart (or vice versa).
docs-i18n:
    python3 "{{ repo }}/scripts/check-docs-i18n.py"

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
    cargo check \
      -p persisting-agentctl \
      -p persisting-events \
      -p persisting-gateway \
      -p persisting-ppilot \
      -p persisting-pvisor \
      --locked
    cargo check -p persisting-pchronicle --no-default-features --locked

# capture 相关 Rust 测试（Gateway 包测试已覆盖全部 capture targets）。
capture-test:
    just test-crate capture

# Execute pChronicle single-machine/self-service cases.
[group('test')]
test-pchronicle-cases:
    cargo build --release -p persisting-pchronicle-cli --locked
    python3 scripts/run-pchronicle-cases.py --document docs/src/pchronicle/reference/cases-self.md --pchronicle target/release/pchronicle --report target/pchronicle-self-case-report.md

# List and execute pChronicle platform/Catalog cases. Server lifecycle cases are
# reported as MANUAL unless explicitly selected with PCHRONICLE_CASE_MODE.
[group('test')]
test-pchronicle-cases-platform:
    cargo build --release -p persisting-pchronicle-cli --locked
    python3 scripts/run-pchronicle-cases.py --document docs/src/pchronicle/reference/cases-platform.md --pchronicle target/release/pchronicle --report target/pchronicle-platform-case-report.md

# Run documented integration cases by component.
# Examples: just cases pvisor | pchronicle | pchronicle-cluster
cases target:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{target}}" in
      pvisor)
        cargo build --release -p persisting-pvisor --locked
        python3 scripts/run-pvisor-cases.py --report target/pvisor-case-report.md
        ;;
      pchronicle)
        cargo build --release -p persisting-pchronicle-cli --locked
        python3 scripts/run-pchronicle-cases.py --document docs/src/pchronicle/reference/cases-self.md --pchronicle target/release/pchronicle --report target/pchronicle-self-case-report.md
        ;;
      pchronicle-cluster)
        cargo build --release -p persisting-pchronicle-cli --locked
        python3 scripts/run-pchronicle-cases.py --document docs/src/pchronicle/reference/cases-platform.md --pchronicle target/release/pchronicle --report target/pchronicle-platform-case-report.md
        ;;
      *)
        echo "usage: just cases pvisor|pchronicle|pchronicle-cluster" >&2
        exit 2
        ;;
    esac

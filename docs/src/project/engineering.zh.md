# 工程说明

这些笔记记录对贡献者有用的仓库交付工作，但不属于产品契约。产品实现状态与
路线图细节属于各产品的 Design 页面。

## 贡献者命令

从仓库根目录运行。`just --list` 列出全部 recipe。

| 命令 | 作用 |
|---|---|
| `just test` | 通过 `cargo nextest` 跑工作区 Rust 测试，再跑 Python 套件 |
| `just test <package>` | 单个 crate 或 Cargo package（例如 `pvisor` 或 `persisting-pvisor`） |
| `just docs-sync` | 安装锁定的文档环境 |
| `just docs-serve` | 本地 MkDocs 预览，带自动重载 |
| `just docs-serve-dirty` | 自动重载卡住时用 `--dirtyreload` 预览 |
| `just docs-build` | 构建静态文档站点 |
| `just docs-links` | 严格 MkDocs 构建，死链即失败 |
| `just examples` | 全部产品示例套件（pVisor、pChronicle 与 pPilot） |
| `just gate` | 格式化、lint 以及完整 Rust 测试工作区 |
| `just dev` | 限定范围的 runtime crate 检查；不是完整工作区矩阵 |

`just test` 使用 debug nextest profile 以便更快迭代。传入 Cargo package 名
或短 crate 别名（`pvisor`、`ppilot`、`pchronicle`、`pchronicle-cli`、
`agentctl`、`capture`）。无参数形式还会跑 `just test-py`。

## 当前笔记

| 笔记 | 读者 | 用途 |
|---|---|---|
| [发布 Persisting](releasing.md) | 维护者 | 版本、trusted-publisher 与稳定发布流程 |
| [可复现示例](examples.md) | 贡献者 | `examples/` 下的产品 CLI 套件 |

## 快速本地构建

仓库的 `rust-toolchain.toml` 为常规编译器校验和可选诊断选择 stable。正常
开发、测试和发布构建都使用该 toolchain 的默认 LLVM backend。

Rust 测试用 `cargo nextest` 做进程隔离和并行执行；用
`cargo install cargo-nextest --version 0.9.137 --locked` 安装 `0.9.137`，
或使用仓库 CI setup action。

本地和普通 CI 构建使用平台默认 linker。manylinux wheel job 用
`cargo zigbuild` 链接 glibc 2.17 sysroot，因为当前 rustc libstd 会引用
`statx` / `copy_file_range`，在 manylinux2014 上 rust-lld 和 GNU ld 都无法满足。

`just dev` 刻意限定在 runtime crate 以及无默认 feature 的 pChronicle 检查。
完整工作区、all-targets 和 storage-feature 矩阵请用 `just gate` 或 CI
工作流。

`cargo nextest` 不跑 doctest。需要时把文档测试留在常规 Cargo runner 上，
例如 `cargo test --doc -p <package>`。

### Nightly 诊断（可选）

仓库把两项昂贵 / nightly 诊断排除在日常编辑循环之外：

- `just build-analysis persisting-pvisor` 为一个 package 启用 Cargo 的
  `-Z build-analysis`，并把每会话 JSONL 指标写到 `$CARGO_HOME/log`。用
  `just build-analysis-report` 查看（也可传 `report=timings` 或
  `report=rebuilds`）。独立的 target 目录避免诊断产物污染普通增量缓存。
- `just sanitize address persisting-agentctl` 用 LLVM AddressSanitizer 跑
  所选 crate 的测试。该 recipe 使用 `-Z build-std`，因此需要 nightly
  `rust-src` 组件。其他支持值是 `leak`、`thread` 和 `undefined`；可用性
  取决于宿主平台。

Sanitizer 构建故意不进入 `just dev` / CI 的默认路径：它们会重建标准库，
只适合聚焦的调试会话。

支持的行为请从 [pVisor 指南](../pvisor/guides/index.md)、
[pPilot 编排](../ppilot/guides/orchestrate.md) 或
[pChronicle 指南](../pchronicle/guides/index.md) 开始，实现理由见
[系统架构](../system-design/index.md)。

# 工程说明

这些笔记记录对贡献者有用的仓库交付工作，但不属于产品契约。产品实现状态与
路线图细节属于各产品的 Design 页面。

## 贡献者命令

从仓库根目录运行。`just --list` 列出全部 recipe。

| 命令 | 作用 |
|---|---|
| `just test` | 通过 `cargo nextest` 跑pChronicle 与 CLI 测试，再跑范围内的 Python 测试 |
| `just test <package>` | 单个 crate 或 Cargo package（例如 `pchronicle` 或 `persisting-pchronicle`） |
| `just docs-sync` | 安装锁定的文档环境 |
| `just docs-serve` | 本地 Zensical 预览，文件修改时自动刷新 |
| `just docs-serve-dirty` | 自动重载卡住时重新启动 Zensical 预览 |
| `just docs-build` | 构建静态文档站点 |
| `just examples` | pChronicle 产品示例套件 |
| `just dev` | 自动格式化、lint，然后运行 Rust 测试 |
| `just ci` | 只读 lint 检查、Rust/Python 测试、性质测试，然后构建 |
| `just check-quick` | pChronicle CLI 与无默认 feature 的 pChronicle 检查 |

`just test` 使用 debug nextest profile 以便更快迭代。传入 Cargo package 名
或短 crate 别名（`pchronicle`、`pchronicle-cli`）。`just test pchronicle` 会同时跑
`persisting-pchronicle` 与 `persisting-pchronicle-cli`（与 CI 的 pchronicle
shard 一致）；只要 CLI 时用 `just test pchronicle-cli`。无参数形式还会跑
`just test-py`，覆盖打包、轨迹行契约与基准报告。Python 格式化和 lint 检查这些
测试及 pChronicle 构建、文档和用例脚本，遵循 `AGENTS.md` 的范围。

## 当前笔记

| 笔记 | 读者 | 用途 |
|---|---|---|
| [发布 Persisting](releasing.md) | 维护者 | 版本、trusted-publisher 与稳定发布流程 |
| [可复现示例](examples.md) | 贡献者 | `examples/` 下的产品 CLI 套件 |

## 快速本地构建

仓库的 `rust-toolchain.toml` 选择 stable、rustfmt 与 Clippy。正常
开发、测试和发布构建都使用该 toolchain 的默认 LLVM backend。

Rust 测试用 `cargo nextest` 做进程隔离和并行执行；用
`cargo install cargo-nextest --version 0.9.137 --locked` 安装 `0.9.137`，
或使用仓库 CI setup action。

本地和普通 CI 构建使用平台默认 linker。Linux wheel 使用 manylinux_2_28
镜像（glibc 2.28），以便 rustc libstd 能链接 `statx` /
`copy_file_range`。

`just check-quick` 检查 pChronicle CLI 以及无默认 feature 的 pChronicle。
`just dev` 先格式化文件，再运行 lint 和 Rust 测试；`just ci` 使用只读 lint 检查，
并额外运行 Python 和性质测试。GitHub Actions 还覆盖平台分片、Web 构建、S3 与示例。

`cargo nextest` 不跑 doctest。需要时把文档测试留在常规 Cargo runner 上，
例如 `cargo test --doc -p <package>`。

### CI 构建复用

MinIO 工具按操作系统、架构、Go 版本和固定源码版本缓存。基准测试在同一 runner 上
顺序执行两个版本，复用 Cargo 构建目录，分别保存报告。Wheel 构建缓存两个 Rust
工作区，内嵌 Web 资源的缓存签名包含锁文件。

纯 Python 任务跳过 Rust 安装，只构建的任务跳过 cargo-nextest。文档任务按 ref
划分并发组，PR 构建不会取消主分支的文档部署。

支持的行为请从 [pChronicle 指南](../pchronicle/guides/index.md) 开始，实现理由见
[系统架构](../system-design/index.md)。

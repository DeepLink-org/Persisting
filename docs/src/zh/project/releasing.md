# 发布 Persisting

稳定发布由 GitHub Actions 从版本 tag 构建，并通过 Trusted Publishing 发布到
PyPI。项目仍然以 Python wheel 交付，但不包含 PyO3 扩展，也不使用 Maturin。

每个平台 wheel 标记为 `py3-none-<platform>`，并包含：

- Python `persisting` 包；
- 原生命令行脚本；
- 捆绑的 pChronicle Web 资源；
- pVisor 所需的平台 libkrun firmware payload。

当前发布集包含 Linux x86_64 和 Apple Silicon macOS wheel。源码分发不是已
发布产物的一部分。

## 一次性设置

1. 创建名为 `pypi` 的 GitHub environment。不要要求 reviewer；把部署限制到
   匹配 `v*` 的 tag。
2. 在 PyPI 发布设置中添加 pending Trusted Publisher：
   - PyPI project: `persisting`
   - GitHub owner: `DeepLink-org`
   - Repository: `Persisting`
   - Workflow: `release.yml`
   - Environment: `pypi`

GitHub 中不存放 PyPI API token。pending publisher 可以在首次成功上传时
创建项目，但不预留名称。

## 准备一次发布

1. 在 `pyproject.toml`、`Cargo.toml` 的 workspace package 段以及
   `persisting/__init__.py` 中更新同一 `X.Y.Z` 版本。
2. 刷新 lockfile 中的本地 workspace 版本，且不升级依赖：

   ```bash
   cargo metadata --format-version 1 --no-deps >/dev/null
   ```

3. 提交版本变更并合并到 `main`。工作流会拒绝其 commit 无法从 `main` 到达
   的 tag。
4. 可选地手动运行 **Publish PyPI**。手动运行会构建并校验全部 wheel，但不
   发布它们。
5. 创建并推送匹配的稳定 tag：

   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

## 构建与校验路径

PEP 517 backend 是 setuptools，配合仓库自有的
`scripts/packaging/build_backend.py`。组装 wheel 之前，它会构建三条 Rust
CLI、暂存 firmware，并确保 Dioxus bundle 存在。`setup.py` 把 wheel 标为
平台相关，同时把 Python 与 ABI tag 保持为 `py3-none`。

打包脚本会拉取 pinned 的 libkrun firmware 归档（Linux x86_64 与 Apple
Silicon macOS），除非 `PERSISTING_LIBKRUNFW_PATH` 指向已有 payload。本地
wheel 构建必须走这些受支持路径之一；缺少 payload 是构建错误，而不是不完整
的 wheel。

Linux wheel 使用 manylinux_2_28 / glibc 2.28 标签。当前 rustc libstd 和
libkrun 的 virtiofs passthrough 需要 `statx` 与 `copy_file_range`，
manylinux2014 上没有这些符号。

每个 wheel 都会检查组件集和安装时 CLI smoke test。发布集检查随后要求每个
平台恰好一个受支持 wheel、版本匹配、包元数据有效，以及发布前产物大小有界。

对部分完成的 tagged 发布再跑一遍时，会跳过 PyPI 已经接受的文件，并补齐
缺失的 GitHub Release 资源。

## 相关文档

- [安装指南](../installation.md) 描述面向消费者的安装路径与支持平台。
- [工程说明](engineering.md) 把贡献者状态与公开产品契约分开。
- [可复现示例](examples.md) 演练已安装产品的工作流。

# 安装指南

Persisting 提供两条公开命令行路径：

- `pvisor` 在可审查的执行边界中运行 Agent。
- `pchronicle` 打开、查询、交换和服务轨迹 Dataset。

推荐安装包含 pChronicle 快速开始所需的 Lance Dataset 能力。

## 1. 安装工具

```bash
pip install 'persisting[lance]'
```

确认命令可用：

```bash
pvisor --version
pchronicle --help
```

wheel 会把匹配版本的 Python 包和公开 CLI 入口安装到当前 Python 环境。项目有其他
Python 依赖时，建议使用虚拟环境：

```bash
python3 -m venv .venv
source .venv/bin/activate
python -m pip install --upgrade pip
pip install 'persisting[lance]'
```

:::tip 可以从任意一个产品开始
探索 pChronicle 不要求先运行 pVisor。如果你想先运行 Agent，继续阅读[运行第一个 Agent](pvisor/get-started.md)；
如果已经有轨迹数据，继续阅读[探索第一个 Dataset](pchronicle/get-started.md)。
:::

## 2. 检查平台要求

CLI 支持 macOS 和 Linux，要求 Python 3.10 或更新版本。普通 host Run 不需要文件系统扩展。
在 macOS 上使用 host-process 的 `pvisor run --safe` 前，先安装 macFUSE：

```bash
brew install --cask macfuse
```

macOS 提示时允许 macFUSE system extension。如果挂载能力不可用，`--safe` 会 fail closed，
不会退化为直接写入项目目录。libkrun VM executor 不需要 macFUSE。

## 3. 需要时从源码安装

需要 `main` 最新构建时，可以使用 nightly wheel：

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

本地开发时，从 checkout 安装 Python 包：

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
pip install -e '.[lance]'
```

也可以从源码构建 CLI 组件：

```bash
just install-cli
```

只有在明确测试特定 pVisor 二进制时才设置 `PERSISTING_PVISOR_BIN`。排查 Provider 行为时，
应尽量让 Python 包和 CLI 来自同一 revision。

## 4. 需要时启用 VM 或 OCI 执行

默认本地工作流不要求安装 Docker 或 Podman。使用 VM executor 运行 OCI 镜像时，可以显式指定：

```bash
pvisor run --image ubuntu:latest -- COMMAND
```

未指定时 VM 也默认使用 `ubuntu:latest`。`--image-store DIR` 修改本地内容寻址缓存，
`--overlayfs-target` 选择 guest workspace，`--vm-rootfs DIR` 指向预先准备的 Linux rootfs。
Linux 使用 KVM；Apple Silicon macOS 使用 HVF。从源码在 macOS 构建 VM 支持还需要 Zig：

```bash
brew install zig
```

把这些选项当作独立的平台步骤。先完成 staged host workflow，再用已有 Run Bundle 对比不同
执行环境的边界。

## 5. 选择下一步

- [运行第一个 Agent](pvisor/get-started.md) —— 暂存、审查并选择性应用修改。
- [探索第一个 Dataset](pchronicle/get-started.md) —— 在准备真实 Source 前查询临时数据。
- [选择工作流](overview.md) —— 按当前任务决定使用哪个产品。
- [执行环境](pvisor/guides/execution.md) —— 比较 host、OCI 与 VM 的边界。

# 安装指南

Persisting 通过 Python wheel 发布，wheel 同时包含 Python 包和版本匹配的 CLI 组件集：

| 安装物 | 内容 | 用途 |
|---|---|---|
| 宿主机 wheel | Python 包以及 `pchronicle`、`pvisor`、`ppilot` | Python API 和完整宿主机 CLI 组件集 |

## 环境要求

- Python 3.10+
- Pulsing（作为依赖自动安装）
- CLI 需要 macOS 或 Linux
- macOS：宿主进程模式的 `pvisor run --safe` 需要 macFUSE 5（libkrun Run 不需要）

首次执行 safe Run 前安装一次 macOS 文件系统运行时：

```bash
brew install --cask macfuse
```

Apple Silicon 需要在 macOS 提示时允许 macFUSE system extension。普通非 staged 的
host Run 不依赖 macFUSE；`--safe` 在挂载能力不可用时会 fail closed，不会退化为直接
写项目目录。libkrun OCI 镜像 executor 使用内置 Overlay virtio-fs 保证缓存 rootfs
不被修改，不依赖 macFUSE。

## Python 包

```bash
# 推荐：带 Lance 支持
pip install persisting[lance]

# 最小安装（不含 Lance，仅用于自定义后端）
pip install persisting
```

上面两种安装命令都会把版本匹配的 `pchronicle`、`pvisor`、`ppilot` 安装到 Python 环境的
scripts 目录。

### Nightly wheel

每日 / `main` 推送会滚动更新 GitHub Release 标签 [`nightly`](https://github.com/DeepLink-org/Persisting/releases/tag/nightly)：

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

### 从源码

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
pip install -e ".[lance]"
```

## CLI 组件集

wheel 内是一组版本匹配的 CLI 组件：单 Run 与环境使用 `pvisor`，批量编排使用
`ppilot`，Dataset 目录、SQL、分析、格式交换与只读服务使用 `pchronicle`。

### 通过 Cargo 从源码安装

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
just install-cli
```

这是不安装 Python 包的替代路径，会把版本匹配的 `pchronicle`、`pvisor` 和 `ppilot`
安装到 Cargo bin 目录。

### 组件覆盖

`PERSISTING_PVISOR_BIN`、`PERSISTING_PPILOT_BIN` 可显式指定组件二进制路径。

## Container/libkrun executor

Container executor 仍需显式配置兼容的 Linux guest pVisor。`vm` executor 则把
libkrun 和 Linux guest init 静态链接进宿主 `pvisor`；发行 wheel 还会把 libkrunfw
安装在 `pvisor` 同目录。源码构建会自动下载固定版本的官方 release，校验 SHA-256
后放入用户缓存；macOS 使用 `/usr/bin/cc` 把其中的 kernel bundle 编译成 dylib。
仍可用 `--vm-library-dir` 显式指向系统 libkrunfw。在 macOS 上从源码构建 pVisor
还需安装 Zig（`brew install zig`），用于交叉编译 libkrun 内嵌的 Linux guest init。

`pvisor run --image ubuntu:latest -- COMMAND` 可在不安装 Docker 或 Podman 的情况下
直接拉取并运行公开 OCI 镜像；`--executor vm` 未指定 rootfs 时也默认使用
`ubuntu:latest`。`--image-store DIR` 可覆盖本地内容寻址缓存目录，
`--overlayfs-target` 用于指定 guest 工作路径。`--vm-rootfs DIR` 仍可指定预先准备的 Linux 根文件
系统。Linux 宿主使用 KVM，Apple Silicon macOS 宿主使用 HVF。

## 验证

```python
import persisting
print(persisting.__version__)
```

```bash
pvisor --version
ppilot --help
pchronicle --help
```

## 依赖

| 包名 | 版本 | 必需 | 说明 |
|------|------|------|------|
| `pulsing` | >=0.1.0 | 是 | 分布式 actor 运行时（控制面） |
| `lance` | >=0.9.0 | 可选 (`[lance]`) | Lance 列式存储 |
| `pyarrow` | >=14.0.0 | 可选 (`[lance]`) | Apache Arrow |

## 下一步

- [运行第一个 Agent](pvisor/get-started.md) — 完成 run、review 与选择性 apply 闭环
- [Persisting 的完整故事](overview.md) — 理解一个 Run 如何扩展到编排与历史
- 按产品进入 [pVisor](pvisor/index.md) 或 [pChronicle](pchronicle/index.md)

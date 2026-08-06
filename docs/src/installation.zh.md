# 安装指南

Persisting 通过 Python wheel 发布，wheel 同时包含 Python 包和版本匹配的 CLI 组件集：

| 安装物 | 内容 | 用途 |
|---|---|---|
| 宿主机 wheel | Python 包以及 `persisting`、`pvisor`、`ppilot` | Python API 和完整宿主机 CLI 组件集 |

## 环境要求

- Python 3.10+
- Pulsing（作为依赖自动安装）
- CLI 需要 macOS 或 Linux

## Python 包

```bash
# 推荐：带 Lance 支持
pip install persisting[lance]

# 最小安装（不含 Lance，仅用于自定义后端）
pip install persisting
```

上面两种安装命令都会把版本匹配的 `persisting`、`pvisor`、`ppilot` 安装到 Python 环境的
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

## 统一 CLI

wheel 内的 CLI 是匹配的组件集：`persisting` 把执行/环境命令转发给 `pvisor`，批量/查询命令转发给
`ppilot`，history/eval 直接调用 pChronicle。

### 通过 Cargo 从源码安装

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
just install-cli
```

这是不安装 Python 包的替代路径，会把版本匹配的 `persisting`、`pvisor` 和 `ppilot`
安装到 Cargo bin 目录。

### 组件覆盖

`PERSISTING_PVISOR_BIN`、`PERSISTING_PPILOT_BIN` 可显式指定组件二进制路径。

## Container/KVM executor

`pvisor run --executor container|kvm` 需要与目标平台兼容的 Linux pVisor。请通过 executor
的 `pvisor_binary` 配置显式提供。Nightly 不再发布单独的 guest runtime；host executor
不需要这个额外产物。

## 验证

```python
import persisting
print(persisting.__version__)
```

```bash
pvisor --version
ppilot --help
```

## 依赖

| 包名 | 版本 | 必需 | 说明 |
|------|------|------|------|
| `pulsing` | >=0.1.0 | 是 | 分布式 actor 运行时（控制面） |
| `lance` | >=0.9.0 | 可选 (`[lance]`) | Lance 列式存储 |
| `pyarrow` | >=14.0.0 | 可选 (`[lance]`) | Apache Arrow |

## 下一步

- [快速开始](quickstart.md) — 5 分钟跑通一个 Agent Run
- [选择能力](guide/index.md) — 按目标选择工作流
- [设计文档](design/index.md) — 架构与内部实现

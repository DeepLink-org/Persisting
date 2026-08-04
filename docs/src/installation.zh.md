# 安装指南

Persisting 拆成三个安装物，按用途选择：

| 安装物 | 内容 | 用途 |
|---|---|---|
| Python 包 | `persisting`（wheel） | Python API：tensor memory、queue、search |
| 统一 CLI | `persisting`、`pvisor`、`ppilot` + `libpersisting_engine` | `execute` / `env` / `batch` / `query` / `history` / `eval` / `gateway` |
| Guest runtime | 静态 Linux `pvisor`（`linux-amd64` / `linux-arm64`） | Container/KVM executor 注入 guest 用 |

## 环境要求

- Python 3.10+
- Pulsing（作为依赖自动安装）
- CLI 需要 macOS 或 Linux；guest runtime 发布的是静态 Linux 二进制

## Python 包

```bash
# 推荐：带 Lance 支持
pip install persisting[lance]

# 最小安装（不含 Lance，仅用于自定义后端）
pip install persisting
```

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

CLI 是匹配的组件集：`persisting` 把执行/环境命令转发给 `pvisor`，批量/查询命令转发给
`ppilot`，search/history/eval 等动态加载 `libpersisting_engine`。三种安装方式：

### 从源码（推荐开发）

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
just install-cli
```

会安装与仓库版本匹配的 `persisting`、`pvisor`、`ppilot` 和引擎库到 Cargo bin 目录。

### Nightly 二进制（无需 Rust 工具链）

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-cli-nightly.sh | bash
```

安装到 `~/.persisting/cli/bin`（可用 `PERSISTING_CLI_ROOT` 覆盖），脚本会打印 PATH 导出行。
每个发布资产都附带 `.sha256` 校验文件。

### 组件覆盖

`PERSISTING_PVISOR_BIN`、`PERSISTING_PPILOT_BIN`、`PERSISTING_ENGINE_LIB` 可显式指定
二进制或引擎库路径。

## Guest runtime（Container/KVM executor）

`pvisor run --executor container|kvm` 需要与目标平台匹配的静态 Linux pVisor：

```bash
# 默认 linux-amd64；需要 arm64 guest 时再跑一次 --platform linux-arm64
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-guest-runtimes.sh | bash -s -- --platform linux-amd64
```

安装到 `~/.persisting/runtimes/<version>/<platform>/pvisor`（或
`$PERSISTING_PVISOR_RUNTIME_DIR/<platform>/pvisor`），pVisor 的 artifact 发现会自动命中。
没装也不影响 host executor 和 `--safe` profile，只有 Container/KVM 路径会报缺少 runtime。

`just install-cli-nightly`、`just install-guest-runtimes` 是同一脚本的封装。

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

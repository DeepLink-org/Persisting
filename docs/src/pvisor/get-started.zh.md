# 运行第一个 Agent

这篇指南只完成一条有用的闭环：安装 Persisting，在 staged environment 中运行 Agent，
检查它产生的 Effect，再选择性接受修改。支持 macOS 与 Linux。

## 1. 安装 CLI

Wheel 会同时安装 Persisting 当前的命令行入口：

```bash
pip install persisting[lance]
```

也可以安装当前 nightly build：

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

macOS 使用 staged host workspace 前，需要安装一次 macFUSE：

```bash
brew install --cask macfuse
```

确认命令入口：

```bash
pvisor --help
pchronicle --help
```

源码构建、VM 支持、平台要求与组件覆盖见[安装指南](../installation.md)。

## 2. 运行一个 Agent

进入项目目录：

```bash
pvisor -- codex
```

也可以把 `codex` 换成其他 Agent 命令。`--stage ./runs/task-001` 为 workspace 写入创建 Run 独占 stage，
并安装当前平台支持的控制机制。它不会把所有平台描述成具有相同隔离强度；Run Bundle
会分别记录文件系统、网络与其他 capability evidence。

Run 期间，Agent 修改的是 staged view，基础项目保持不变。

## 3. 检查 Effect

```bash
pvisor review last
pvisor inspect last -- git status --short
```

接受修改前，检查文件变化、网络计数、实际控制机制与警告。

## 4. 接受一部分修改

先应用一个区域：

```bash
pvisor apply last --path src
```

其他修改继续留在 stage 中。再次 review，并选择下一批：

```bash
pvisor review last
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
```

最后接受全部剩余修改，或者丢弃：

```bash
pvisor apply last --all
# 或者
pvisor drop last
```

这就是本地工作流的核心：Agent 无需为每次编辑弹出 approval，而用户仍然决定哪些
Effect 可以进入真实项目。

## 5. 按任务继续

- [查看 Persisting 产品概览](../overview.md)
- [学习选择性、多次 apply](guides/review-apply.md)
- [选择 host、Container 或 VM 运行方式](guides/execution.md)
- [控制网络访问](guides/network.md)
- [捕获 Agent 轨迹](guides/capture.md)
- [查询持久化历史](../pchronicle/get-started.md)

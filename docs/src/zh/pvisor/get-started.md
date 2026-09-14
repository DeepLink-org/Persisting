# 运行第一个 Agent

这条路径把你从空项目带到一次经过审查的修改。每一步都有明确的检查点，
你可以在需要时停下来，不必一次学完所有能力。

!!! tip "pVisor 的工作循环"

    **运行 → 审查 → 选择 → 继续。** Agent 在 staged view 中工作，只有
    `apply` 的 Effect 才会进入真实项目。

## 开始前

你需要 macOS 或 Linux、一个项目目录，以及 `codex` 这样的 Agent 命令。
先安装 CLI，并确认两个产品入口都可用：

```bash
pip install persisting
pvisor --help
pchronicle --help
```

macOS 使用 staged host workspace 前，需要安装一次 macFUSE：

```bash
brew install --cask macfuse
```

源码构建、VM 支持和平台要求见[安装指南](../installation.md)。

## 1. 在 stage 中运行一个 Agent

进入项目目录，先使用一个明确的 stage：

```bash
pvisor run --stage ./runs/task-001 -- codex
```

也可以换成你的 Agent 命令。Agent 修改的是 staged view，基础项目保持不变。
命令结束后，你会得到一个可以审查的 Run Bundle。

!!! success "检查点：基础项目仍然安全"

    在基础项目中运行 `git status`。在 `apply` 之前，不应看到 Agent 的修改。

## 2. 审查实际发生的事情

先看汇总，再检查 staged view：

```bash
pvisor review last
pvisor inspect last -- git status --short
```

在决定哪些内容越过边界前，检查文件 Effect、实际控制机制、网络证据和警告。
命令成功并不代表所有请求的 capability 都可用；Run Bundle 会记录实际生效的机制。

## 3. 先应用一小块可信修改

先应用一个路径，其余内容继续留在 stage 中：

```bash
pvisor apply last --path src
pvisor review last
```

之后可以继续应用另一组依赖闭合的选择：

```bash
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
```

完成时使用 `pvisor apply last --all`，或用 `pvisor drop last` 丢弃剩余 Effect。

!!! success "检查点：边界由你控制"

    已接受的批次进入真实项目；剩余批次仍然可以独立审查、应用或丢弃。

## 4. 选择下一层能力

只为下一次 Run 增加你需要的控制：

- [多次选择性 apply，并保留检查点](guides/review-apply.md)
- [选择 host、OCI 或 VM 执行环境](guides/execution.md)
- [控制网络访问](guides/network.md)
- [把 Run 捕获为 pChronicle 历史](guides/capture.md)
- [回放或比较 sandbox](guides/sandbox-replay.md)

要学习配套的历史工作流，请继续阅读[探索第一个 Dataset](../pchronicle/get-started.md)。

# 从你当前的问题开始

Persisting 有两个独立入口。先选择与你当前任务相符的入口，再沿着短路径完成一个有用结果。

## 我想运行 Agent，并审查它产生的修改

从 **pVisor** 开始。它为单个 Agent 提供 Run 独占 workspace，记录执行边界，并把文件修改
留在 stage 中，直到你决定哪些内容进入真实项目。

1. [安装命令行工具](installation.md)。
2. [运行第一个 Agent](pvisor/get-started.md)。
3. [审查并选择性应用修改](pvisor/guides/review-apply.md)。
4. [选择 host、OCI 或 VM 执行环境](pvisor/guides/execution.md)。

完成后，你应该得到一个已结束的 Run、一份可读的 Run Bundle，以及一个已经 apply 或 drop 的
stage。

```bash
pvisor run --stage ./runs/task-001 -- codex
pvisor review last
pvisor apply last --path src
```

## 我已经有 Agent 轨迹数据

从 **pChronicle** 开始。它可以检查本地或对象存储中的数据，也可以导入支持的外部格式。第一
个 walkthrough 会创建临时示例数据，因此无需先准备 Dataset 就能学习查询流程。

1. [探索第一个 Dataset](pchronicle/get-started.md)。
2. [发现并查询自己的数据](pchronicle/guides/discover-and-query.md)。
3. [导入或导出支持的格式](pchronicle/guides/exchange.md)。
4. [在本地提供 Dataset 服务](pchronicle/guides/serve.md)。

完成后，你应该能执行只读查询，看到规范化视图，并明确所查看数据的 Source lineage。

```bash
pchronicle onboard query
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) FROM dataset.steps GROUP BY source'
```

## 我想把执行和历史连接起来

等两个独立工作流都能运行后，再连接它们。配置 pVisor capture，把选定的 Gateway 轨迹事件和
lifecycle record 发布到 pChronicle。这个交接是显式且有限的：它不会搬运私有 Run Bundle，也
不会补造原始 Source 没有提供的 Evidence。

1. [捕获 Agent 轨迹](pvisor/guides/capture.md)。
2. [理解 event 与 sidecar 契约](rfcs/0007-events-contract-pchronicle-sidecar.md)。
3. [阅读从执行到历史的架构](system-design/architecture.md)。

```text
pVisor Run ── configured capture ──> canonical event Source ──> Dataset views
external trajectory Source ──────────────────────────────────> Dataset views
```

## 我需要先理解边界，再运行 Agent

按这个顺序阅读核心概念：

1. [Run、Attempt 与 Effect](pvisor/concepts/run-model.md) —— 稳定对象。
2. [Capability 与 Evidence](pvisor/concepts/capabilities-and-evidence.md) —— Run 能够声明什么。
3. [执行环境](pvisor/guides/execution.md) —— Provider 选择如何改变边界。
4. [安全与 Evidence](system-design/security-evidence.md) —— 哪些内容会持久化，哪些仍留在本地。

整套文档遵循同一条规则：命令成功退出，不代表所有请求的 capability 都已 enforcement。
Run Bundle 会记录实际生效的机制与限制。

## 继续阅读

- [pVisor 命令模型](pvisor/design/cli.md)
- [pVisor Case 目录](pvisor/reference/cases.md)
- [pChronicle 核心概念](pchronicle/concepts/index.md)
- [系统设计](system-design/index.md)

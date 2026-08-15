# Persisting 概览

Persisting 把一条 Agent 命令变成持久的 **Run**：它拥有隔离的执行环境、可审查的
Effect、稳定身份，以及不会随进程退出而消失的历史。

同一套 Run 模型既用于开发者电脑，也用于批量编排多个 Run。

![Persisting 执行架构](assets/diagrams/persisting/execution-story.svg)

## 1. 从一个 Agent Run 开始

Agent 需要的不只是进程。它还需要工作区、工具、网络、凭据、状态，以及约束外部修改的
边界。

`pvisor` 创建这条边界。它是 Persisting 对 [AgentVisor](pvisor/concepts/agentvisor.md) 品类的实现：
面向 Agent 执行的 Hypervisor。每个 Run 获得独立的 Agent 虚拟执行环境，底层 host、
Container、VM 或集群资源仍然可以复用。

```bash
pvisor run --safe codex
```

该命令使用 staged workspace，并记录实际安装的控制机制。Run identity 不依赖进程 ID
或 execution provider。

## 2. 把执行与接受结果分开

允许 Agent 工作和接受 Agent 产生的 Effect，是两个不同决策。Persisting 把文件修改保存
在 Run 独占的 stage 中，因此 Agent 无需为每一次编辑请求批准。

Run 结束后，先检查结果，再只接受应该进入基础工作区的部分：

```bash
pvisor review last
pvisor apply last --path src
pvisor apply last --include 'tests/**'
pvisor apply last --all
```

`apply` 可以执行多次。每次成功调用只消费选中的、依赖闭合的变更批次；未选中的修改
继续留在 stage 中。网络调用、消息、数据库写入和部署等不能表示为 staged file 的 Effect，
需要各自的 admission 与 evidence 机制。

## 3. 扩展 Run，而不是扩展 Shell 命令

当一个 Run 已经拥有稳定身份、输入、生命周期、结果和证据后，就可以在不改变语义的
前提下被批量编排。`pPilot` 规划任务、限制并发、隔离 lease、记录持久结果，并对已支持
的崩溃窗口执行 reconcile。

```bash
ppilot run plan.py --workers 4 --per-worker 2 --sink ./results
```

本地工作流与集群工作流共享同一个执行单位：Run。物理 placement 可以变化，但 Run
identity、authority、checkpoint lineage 和结果所有权不能随之改变。

## 4. 在执行结束后保留历史

进程会消失，解释一次 Run 所需的事实不应该消失。Gateway capture 在运行时产生 canonical
event；`pChronicle` 发现轨迹 Source、规范化视图，并提供查询入口。

```bash
pchronicle analysis overview examples/data/atif
pchronicle query examples/data/atif \
  'SELECT source, COUNT(*) AS steps FROM dataset.steps GROUP BY source'
```

历史层既不是调度器，也不是执行边界。它是用于检查、回放、评测、交换和分析的持久记录。

## 从本地到集群保持同一模型

| 关注点 | 单个本地 Run | 多 Run 或集群 |
| --- | --- | --- |
| 执行 | 一个 Agent 虚拟执行环境 | 跨 Provider 放置的一组执行环境 |
| 身份 | 稳定 Run ID 与一个 active Attempt | 稳定 Run ID 与 lease-fenced ownership |
| Effect | Review、选择性 apply、drop | 策略驱动的 promotion 与 reconciliation |
| 连续性 | Stage、checkpoint、fork | 跨 placement 的恢复、迁移与 lineage |
| 证据 | Run Bundle 与 capture event | 持久历史与跨 Run 分析 |

组件边界很直接：**pVisor 运行并约束一个 Run，pPilot 编排多个 Run，pChronicle 保存
发生过的事实。** Gateway、OverlayFS 与 OverlayNet 是 pVisor 内部的运行时机制。

## 按任务继续

- [运行第一个 Agent](pvisor/get-started.md)
- [审查并选择性应用修改](pvisor/guides/review-apply.md)
- [编排多个 Run](pvisor/guides/orchestrate.md)
- [查询持久化历史](pchronicle/get-started.md)
- [阅读系统架构](system-design/index.md)

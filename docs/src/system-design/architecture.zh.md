# 端到端架构

本文只定义 Persisting 产品之间的契约。Provider 机制属于 pVisor Design，存储布局属于
pChronicle Design，命令属于各产品 Reference。

![Persisting product domains and integration](../assets/diagrams/persisting/system-products.svg)

## 产品 Ownership

| 产品或层次 | 拥有 | 不拥有 |
| --- | --- | --- |
| pVisor | 一个 Run、Attempt、执行环境、capability admission、Effect 与运行时 Evidence | 多 Run 调度或持久历史查询 |
| pPilot | 多 Run planning 与 reconciliation、lease、有界并发、基础设施重试和结果收集 | Agent reasoning、Provider enforcement 或轨迹存储 |
| pChronicle | canonical event、终态事实、Dataset discovery、规范化 projection、revision 与读取面 | 启动、调度或控制 Run |
| Runtime Provider | 一种物理执行机制 | 逻辑 Run identity 或产品策略 |

Gateway、OverlayFS 和 OverlayNet 是 pVisor 运行时机制，不构成独立控制面。pPilot 扩展
pVisor 的 Run 模型，仍位于 pVisor 产品路径中。

## 稳定对象

```text
RunSpec
  └── Run
      ├── Attempt 1
      ├── Attempt 2
      ├── Artifact references
      ├── Effect decisions
      └── terminal RunResult
              └── canonical events and history projections
```

逻辑 Run 可以迁移，Attempt 与 Provider 绑定。基础设施重试创建新 Attempt；语义重试创建
派生 Run。一个 Run 可以有多个 Attempt，但只能有一个可见终态结果。

跨产品稳定身份是 `run_id`。Session、Step、call、event 和 Artifact identity 保留自己的
scope 与 Source lineage。进程 ID、Container ID、VM ID 或 worker lease 都不能代替 Run identity。

## 单 Run 路径

```text
User or Agent framework
  → RunSpec
  → pVisor admission
  → capability-by-dimension provider selection
  → Attempt execution
  → runtime events and Artifact references
  → Effect review or direct policy decision
  → terminal RunResult
  → pChronicle canonical history
```

Admission 比较请求的 capability 维度与选中 Provider 能提供的 Evidence。必需维度无法
enforce 时，在 workload 执行前失败；可选降级必须明确写入 Run Bundle。

文件 promotion 是 Effect 决策，不是 Run 终态提交。只要 stage 仍存在，就可以多次 apply
不同路径。网络请求和远程工具修改属于不同 Effect 维度，不能从文件状态推断。

## 多 Run 路径

```text
Manifest or task stream
  → pPilot planner
  → stable task_id and run_id
  → bounded RunFuture set
  → pVisor placement and Attempts
  → pPilot checkpoint and reconciliation
  → terminal results
  → pChronicle history
```

pPilot 调度 Run future，而不是 Agent conversation。它持久保存：

```text
job_id → task_id → run_id → attempt_id / lease_epoch → terminal result
```

系统不承诺物理执行 exactly once。Lease fencing、稳定 identity、幂等 event ingestion 和
终态 compare-and-swap 的目标是 at-least-once Attempt 与一个可见 Run 结果。

## 历史路径

pVisor、Gateway、Provider 和 importer 产生事实；ingestion 之后的持久解释由 pChronicle 拥有：

```text
producers
  → canonical events
  → terminal fact and Artifact manifest
  → normalized Run / Step / ToolCall projections
  → exchange formats and lineage-bearing revisions
```

Canonical fact 采用 append-oriented 模型。Storyline 等规范化视图是可重建 projection；交换
文件是互操作边界，不替代事实源。每次读取固定一个 Catalog Snapshot，但不会虚构跨无关
Source 的全局事务。

## 故障与恢复

| 故障 | Owner | 必需行为 |
| --- | --- | --- |
| Attempt 退出或 Provider 消失 | pVisor | finalise Evidence；暴露失败或创建 fenced replacement Attempt |
| Worker lease 过期 | pPilot | 阻止 stale terminal publication；reconcile 期望与实际状态 |
| capture queue 饱和 | producer/Gateway | 永不阻塞请求 callback；按配置报告丢失或进入持久路径 |
| 历史发布冲突 | pChronicle writer | 保留已发布 Snapshot；按 writer contract 报错或重试 |
| 控制面重启 | pPilot | reconcile checkpoint、活跃 Attempt 与终态历史事实 |
| 视图生成失败 | pChronicle | 保持 canonical fact 可读；重建派生视图 |

恢复不能把不确定性升级为成功。缺失终态事实、丢失 callback、未 enforce 的 capability 都必须
保持可见。

## 安全与 Evidence 链

安全性按 capability 维度报告。pVisor 记录请求策略、实际机制、Provider identity、
enforcement 结果与观察到的 Effect；pPilot 在 placement 间保存 authority generation 和
lease history；pChronicle 保存 Evidence 引用与不可变结果事实。

```text
requested policy
  → admission decision
  → installed mechanism
  → provider-bound evidence
  → observed effects
  → terminal result
  → durable history
```

Evidence 层级见[安全与 Evidence](security-evidence.md)，可迁移要求见
[从本地到集群](local-to-fleet.md)。

## 公共边界

| 边界 | 契约 Owner | 详细文章 |
| --- | --- | --- |
| Agent 执行与 Effect review | pVisor | [pVisor 概念](../pvisor/concepts/index.md)与[指南](../pvisor/guides/index.md) |
| Provider 与运行时机制 | pVisor | [pVisor Design](../pvisor/design/index.md) |
| 多 Run 编排 | pVisor 中的 pPilot | [pPilot Design](../pvisor/design/orchestration.md) |
| Dataset、事实与 Projection | pChronicle | [pChronicle 概念](../pchronicle/concepts/index.md) |
| 存储与 Catalog 实现 | pChronicle | [pChronicle Design](../pchronicle/design/index.md) |
| 稳定命令语法与格式 | 各产品 | [pVisor Reference](../pvisor/reference/index.md)与 [pChronicle Reference](../pchronicle/reference/index.md) |
| 规范性 ownership 决策 | Project RFC | [RFC 索引](../rfcs/index.md) |

只有跨产品契约变化时才修改本文。产品实现状态和 roadmap 属于对应 Design 页或 Project
工程说明。

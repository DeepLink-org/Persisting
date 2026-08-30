# 端到端架构

本文只定义 Persisting 产品之间的契约。Provider 机制属于 pVisor Design，存储布局属于
pChronicle Design，命令属于各产品 Reference。

![Persisting 产品域与集成关系](../assets/diagrams/persisting/system-products.svg)

## 产品 Ownership

| 产品或层次 | 拥有 | 不拥有 |
| --- | --- | --- |
| `persisting-events` 契约 | 存储无关的 `EventRecord` identity/envelope 与可选的版本化 pChronicle control 协议 | 物理 row、存储引擎、查询或 projection |
| pVisor | 一个 Run、Attempt、执行环境、capability admission、Effect 与运行时 Evidence | 多 Run 调度或持久历史查询 |
| pPilot | 多 Run planning、有界执行、lease、重试与恢复、reconciliation、结果收集及 task-to-Run mapping | Agent reasoning、Provider enforcement 或轨迹格式与存储 |
| pChronicle | Agent 轨迹存储引擎：path 身份、Snapshot、canonical event、projection、查询与交换 | 启动、调度或控制 Run |
| Runtime Provider | 一种物理执行机制 | 逻辑 Run identity 或产品策略 |

Gateway、OverlayFS 和 OverlayNet 是 pVisor 运行时机制，不构成独立控制面。pPilot 扩展
pVisor 的 Run 模型，仍位于 pVisor 产品路径中。

## 独立 Ingress 路径

```text
Configured runtime capture
  Gateway trajectory events ─┐
  pVisor lifecycle records ──┴─> canonical event Source ──────────────┐
Pinned external Sources                                                │
  local/S3 ATIF, ACTF, OpenAI Messages files ──────────────────────────┼─> Snapshot
  local/S3 Storyline Sources ──────────────────────────────────────────┘
                                                                         └─> normalized Dataset views
```

pPilot 是受治理执行路径上可选的多 Run 编排器。pVisor 可以用 staged Effect 与私有、带版本的
Run Bundle 及 terminal RunResult 完成独立闭环。配置 capture 并不是 pVisor 运行时前置条件。
外部文件与 Storyline Source 会被直接固定版本并规范化；它们既不经过 pVisor，也不会变成
canonical runtime event，因此不会获得 pVisor 执行保证。

## 稳定对象

```text
RunSpec
  └── Run
      ├── Attempt 1
      ├── Attempt 2
      └── Attempt finalization
          ├── terminal RunResult
          ├── private versioned Run Bundle
          └── staged Effects → later review / apply / drop

Optional configured event handoff
  └── Gateway trajectory events + pVisor lifecycle records
```

逻辑 Run 可以迁移，Attempt 与 Provider 绑定。基础设施重试创建新 Attempt；语义重试创建
派生 Run。一个 Run 可以有多个 Attempt，但只能有一个可见终态结果。

Source 携带 `run_id` 时，它就是跨产品稳定身份。Session、Step、call、event 和 Artifact
identity 保留自己的 scope 与 Source lineage。进程 ID、Container ID、VM ID 或 worker lease
都不能代替 Run identity。

## 单 Run 路径

```text
User or Agent framework
  → RunSpec
  → pVisor admission
  → capability-by-dimension provider selection
  → Attempt execution
  → terminal RunResult + private versioned Run Bundle + staged Effects
  → later review / apply / drop
```

Admission 比较请求的 capability 维度与选中 Provider 能提供的 Evidence。必需维度无法
enforce 时，在 workload 执行前失败；可选降级必须明确写入 Run Bundle。

文件 promotion 是 Effect 决策，不是 Run 终态提交。只要 stage 仍存在，就可以多次 apply
不同路径。网络请求和远程工具修改属于不同 Effect 维度，不能从文件状态推断。

配置后，pVisor 会向 pChronicle 发布 Gateway 轨迹 event，以及 `run.created`、
`run.state_changed` 和终态 lifecycle record。这些 record 携带 Run/Attempt identity、
lifecycle fact 与其中可用的 Evidence。Artifact reference、lineage、staged filesystem Effect、
AgentCtl/network/resource Evidence 和完整 Run Bundle 仍留在本地，除非由单独 adapter 搬运。

## 多 Run 路径

```text
Manifest or task stream
  → pPilot planner
  → stable task_id and run_id
  → bounded RunFuture set
  → pVisor placement and Attempts
  → pPilot checkpoint and reconciliation
  → terminal results and task-to-Run mapping
```

pPilot 调度 Run future，而不是 Agent conversation。它持久保存：

```text
job_id → task_id → run_id → attempt_id / lease_epoch → terminal result
```

系统不承诺物理执行 exactly once。Lease fencing、稳定 identity、幂等 event ingestion 和
终态 compare-and-swap 的目标是 at-least-once Attempt 与一个可见 Run 结果。

使用 `run --sink` 时，默认路径会写入配置的结果 journal，并让 pChronicle control 子进程
保存所选 coordination record。启用 `traj-sink` 构建并传入 `--traj` 时，pPilot 只额外发出
终态 `ppilot.result` 或 `ppilot.failure` record；它不会捕获通用 Run 轨迹。Delegated pVisor
Run 不会获得 Chronicle capture 配置。在所有模式中，编排决策与 task-to-Run mapping 仍归
pPilot 所有。命令与 resume 行为见 [pPilot CLI](../ppilot/reference/cli.md) 与
[编排设计](../ppilot/design/orchestration.md)。

## Dataset 路径

Canonical runtime writer 与固定版本的外部 Source 是相互独立的 Source 路径；它们只在
Snapshot 与规范化 Dataset 视图处汇合：

```text
configured Gateway and pVisor lifecycle writers
  → canonical event Source ────────────────────────────────┐
pinned local/S3 external Sources                           │
  → ATIF / ACTF / OpenAI Messages files ───────────────────┼─> Snapshot
  → Storyline Sources ─────────────────────────────────────┘     ├─> normalized Run / Step / ToolCall views
                                                                 └─> query / export / revision lineage
```

Canonical fact 采用 append-oriented 模型。Storyline 等规范化视图是可重建 projection；交换
文件是互操作边界，不替代事实源。每次读取固定一个 Snapshot，但不会虚构跨无关
Source 的全局事务。固定外部文件不会把它转换为 canonical runtime event Source。

## Source 特定保证

| Source 路径 | 支持的主张 | 明确不主张 |
| --- | --- | --- |
| 外部文件或 imported Source | 已发现的内容、固定的 Source version、规范化表示，以及在已实现位置记录的 conversion lineage | 外部 task manifest 的完整性，或不存在未报告轨迹 |
| Gateway capture | 通过所配置 Gateway 路径观察到并持久发布的 request 与 response | 不存在绕过 Gateway 的流量 |
| pVisor Run | Run/Attempt identity、已记录终态事实、已安装机制、观察到的 Effect 与 Provider 特定 Evidence | 所选 Provider 未提供的 enforcement |
| pPilot job | 所选模式支持的持久 task/Run mapping、重试与 lease 历史，以及终态结果行为 | 物理 exactly-once execution |

Ingestion 保留这些边界。规范化表示或 Snapshot 不会升级 Source 提供的 Evidence。

pVisor 默认构建不链接 Lance/DataFusion。配置后的 Chronicle 发布会通过带认证的
loopback IPC 启动 pChronicle sidecar，并且只把 sidecar 成功响应视为 durable
acknowledgement。旧模式名 `lance` 是 `spawn` 的兼容别名；pVisor 不再自行写 Lance。
Sidecar 标志与模式名见 [pVisor CLI](../pvisor/reference/cli.md) 与
[RFC-0007](../rfcs/0007-events-contract-pchronicle-sidecar.md)。

## 故障与恢复

| 故障 | Owner | 必需行为 |
| --- | --- | --- |
| Attempt 退出或 Provider 消失 | pVisor | finalise Evidence；暴露失败或创建 fenced replacement Attempt |
| Worker lease 过期 | pPilot | 阻止 stale terminal publication；reconcile 期望与实际状态 |
| sidecar append queue 饱和或关闭 | pVisor/Gateway producer | 提交前明确拒绝并报告失败；不得声称已经 durable |
| append 连接或 ACK 丢失 | producer 与 pChronicle writer | 由于写入可能已经提交，保留 unknown 状态；不得按“明确拒绝”复用序号 |
| 历史发布冲突 | pChronicle writer | 保留已发布 Snapshot；按 writer contract 报错或重试 |
| 控制面重启 | pPilot | reconcile checkpoint、活跃 Attempt 与终态历史事实 |
| 视图生成失败 | pChronicle | 保持 canonical fact 可读；重建派生视图 |

恢复不能把不确定性升级为成功。缺失终态事实、丢失 callback、未 enforce 的 capability 都必须
保持可见。

## 安全与 Evidence 链

安全性按 capability 维度报告。pVisor 记录请求策略、实际机制、Provider identity、
enforcement 结果与观察到的 Effect；pPilot 在 placement 间保存 authority generation 和
lease history。配置后的 pChronicle capture 会持久保存 Gateway 轨迹 event、pVisor lifecycle
record，以及这些 event 实际携带的 Evidence。完整 Artifact、lineage、filesystem Effect、
AgentCtl/network/resource Evidence 和 Run Bundle 仍留在本地，除非由单独 publisher 或
adapter 搬运。

下面保留 Evidence 的形成层级；它描述本地 Run 的证据链，不表示各层都会自动交接到持久历史：

```text
requested policy
  → admission decision
  → installed mechanism
  → provider-bound evidence
  → observed effects
  → terminal result

Optional configured persistence
  Gateway trajectory events + pVisor lifecycle records
    → event-carried Evidence only
    → pChronicle durable history
```

Evidence 层级见[安全与 Evidence](security-evidence.md)，可迁移要求见
[从本地到集群](local-to-fleet.md)。

## 公共边界

| 边界 | 契约 Owner | 详细文章 |
| --- | --- | --- |
| 逻辑运行事件与本地 Chronicle control 协议 | `persisting-events` | [RFC-0007](../rfcs/0007-events-contract-pchronicle-sidecar.md) |
| Agent 执行与 Effect review | pVisor | [pVisor 概念](../pvisor/concepts/index.md)与[指南](../pvisor/guides/index.md) |
| Provider 与运行时机制 | pVisor | [pVisor Design](../pvisor/design/index.md) |
| 多 Run 编排 | pVisor 中的 pPilot | [pPilot Design](../ppilot/design/orchestration.md) |
| Dataset、事实与 Projection | pChronicle | [pChronicle 概念](../pchronicle/concepts/index.md) |
| 存储与 Snapshot 实现 | pChronicle | [pChronicle Design](../pchronicle/design/index.md) |
| 稳定命令语法与格式 | 各产品 | [pVisor Reference](../pvisor/reference/index.md)与 [pChronicle Reference](../pchronicle/reference/index.md) |
| 规范性 ownership 决策 | Project RFC | [RFC 索引](../rfcs/index.md) |

只有跨产品契约变化时才修改本文。产品实现状态和 roadmap 属于对应 Design 页或 Project
工程说明。

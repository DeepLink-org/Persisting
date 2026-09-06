# pPilot 架构

pPilot 是 durable Run 编排器。产品面刻意只暴露 `ppilot run` 和 `ppilot produce`。

本页负责多 Run 控制算法。用户工作流属于
[编排多个 Agent Run](../guides/orchestrate.md)，跨产品提交边界属于
[系统架构](../../system-design/architecture.md)。

```text
planner / plan()
      │ stable task identity + backpressure
      ▼
pPilot ── RunSpec/file + process ──► pVisor ── RunResult ──► durable result journal
      │                                │
      │ lease / CAS                    │ EventRecord + Attempt state
      └──────────────┬─────────────────┘
                     ▼
             pChronicle control sidecar
                     │
                     ▼
              durable history store
```

pPilot 负责 planning、有界并发、lease、基础设施重试、resume/reconciliation
和结果收集。pVisor 负责每个 Run/Attempt 及其 runtime driver。pChronicle 负责
轨迹 Dataset catalog、SQL、分析、find、导入导出与 serving。

pPilot 不链接 pVisor 实现 crate。它为每个 Run 启动一个前台 `pvisor` 二进制，
提交带版本的 `RunSpec`，并读取原子 `RunResult`。作业 Supervisor 的注册、
heartbeat、quota 和 cancel 消息是共享的 agentctl 契约。进程退出仍是生命周期
边界；Supervisor 连接提供实时控制，而不需要常驻 pVisor daemon。

默认 pVisor 构建没有内嵌 Chronicle 存储适配器，也不链接 Lance 或 DataFusion。
配置了 durable publication 时，pVisor 启动 `pchronicle serve` 的 Control
组件，并通过共享的 `persisting-events` control 契约发布 Attempt 状态以及
lifecycle/Gateway 事件。pPilot 用同一契约做 lease/CAS 和结果日志协调；每条
命令各自拥有它启动的 sidecar 进程。可执行文件由 pVisor 安装或 Run 配置选择；
录制由 `--record-format` 与 `--record-destination` 选择。

本地 control 协议带版本、按请求关联、用进程级 token 鉴权，并绑定 loopback。
它是进程边界，而不是第二套存储实现：只有 pChronicle 选择物理 backend，并
发送 durable acknowledgement。见
[RFC-0007](../../rfcs/0007-events-contract-pchronicle-sidecar.md)。

`run` 执行 map 风格的 `plan()` / `execute(item)` 工作负载。`produce` 从
planner 流式接收完整 Run 描述，并为每项创建独立的 pVisor workspace。两者都
启动进程内 job Supervisor，并发布稳定谱系（`parent_run_id`、`task_id` 和
作业元数据）。

durable 路径使用单调递增的 lease epoch 和终端 CAS，拒绝过期 worker。重启时，
pPilot 先核对 journal、Attempt 记录和 Run 控制记录，再决定 defer、recover
还是 redispatch。外部 Effect 仍需要应用层幂等；系统不承诺 exactly-once 执行。

公开接口见 [`ppilot` 命令参考](../reference/cli.md)，此处使用的身份与重试
模型见 [Run、Attempt 与 Effect](../../pvisor/concepts/run-model.md)。

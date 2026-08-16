# pVisor

**pVisor 是 Persisting 对 AgentVisor 品类的实现。** 它虚拟化 Agent 执行：共享的 host、
Container、VM 和未来集群资源，会被映射为每个 Run 独立的 Agent 虚拟执行环境。

![pVisor 架构](../assets/diagrams/pvisor/agentvisor-architecture.svg)

## pVisor 负责什么

- 提供独立于物理进程和 Provider 的稳定 Run identity；
- 管理创建、admission、取消、恢复、checkpoint 与终态；
- 管理 workspace、网络、工具、模型、凭据与算力 capability；
- 在介质支持时隔离并审查 Effect；
- 记录实际安装了哪些控制机制的 evidence。

pVisor 不定义 Agent reasoning loop。它把已有 Agent CLI、脚本和 framework 放入受治理的
执行边界。

## 从一个 Run 开始

```bash
pvisor run --safe codex
pvisor review last
pvisor apply last --path src
```

Agent 可以在 stage 内自由编辑。用户决定哪些 filesystem Effect 进入基础项目，并且可以
分多批、多次 apply。

pVisor 的独立产品闭环是：

```text
RunSpec -> admission -> Attempt
  -> terminal RunResult + private Run Bundle + staged Effects
  -> later review/apply/drop
```

Attempt finalization 会写入 terminal RunResult 与私有、带版本的 Run Bundle，同时保留 staged
filesystem Effect。后续 `review`、`apply` 或 `drop` 操作再读取并处理 Bundle 与 stage。
pChronicle 并非此闭环的运行时前置条件。

## 按目的阅读 pVisor

| 目标 | 文档 |
| --- | --- |
| 完成第一个本地 Run | [Get Started](get-started.md) |
| 理解品类与对象模型 | [Concepts](concepts/index.md) |
| 选择 executor 或治理 Effect | [Guides](guides/index.md) |
| 检查隔离与运行时机制 | [Design](design/index.md) |
| 查找精确命令语法 | [Reference](reference/index.md) |

要把配置后的 Gateway 轨迹 event 与 pVisor lifecycle record 作为持久 Dataset 查询，请继续
阅读 [pChronicle](../pchronicle/index.md)。当前交接不会发布完整 Run Bundle，也不会发布其中的
Artifact、lineage、Effect 与更完整的 Evidence 清单。

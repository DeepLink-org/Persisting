# 从模型状态到 Agent 历史

**Persisting 是 Agent 时代的持久化基础设施。** 它连接 Agent 工作所需的状态，以及理解
Agent 做过什么所需的历史。

## 一条持久化主线

| 层次 | 典型数据 | 为什么需要持久化 |
| --- | --- | --- |
| 模型状态 | 模型参数与 checkpoint | 加载、共享、版本管理和恢复模型状态 |
| 推理状态 | KV Cache 与可复用中间状态 | 避免在请求和 Run 之间重复计算 |
| Agent 历史 | 轨迹、tool call、执行记录与 effect | 审查、查询、比较和复现行为 |

这些层次不要求使用同一种物理格式或同一个 API。共同点是持久身份与显式生命周期：需要复用的
状态不应随创建它的进程消失，已经完成的工作应当继续可检查。

## 当前用户工作流

当前产品是从执行到可查询历史的路径。直接选择当前任务对应的入口。

| 命令 | 适用任务 | 持久产物 |
| --- | --- | --- |
| `pvisor` | 用明确的 workspace 和 runtime 控制运行一个现有 Agent | Run result、私有 Run Bundle 和 staged 文件修改 |
| `pchronicle` | 检查或交换 Agent 轨迹数据 | 可浏览、可查询的 Dataset 视图 |

## 命令分工

| 命令 | 主要职责 |
|---|---|
| `pvisor` | 单个 Run、执行环境、审查、检查点、apply/drop |
| `ppilot` | 成组的 Run：规划、并发、恢复、结果汇聚 |
| `pchronicle` | Dataset 目录、SQL、内建分析、find、导入导出、只读服务 |

pVisor 不依赖 pChronicle 也能独立工作；pChronicle 可以读取从未经过 pVisor 的外部轨迹。

## 工作流一：运行并审查一个 Agent

当问题是“怎样让 Agent 工作，同时让它的文件修改保持可审查”时，使用 pVisor：

```bash
pvisor run --safe codex
pvisor review last
pvisor apply last --all    # 或：pvisor drop last
```

Agent 在 Run 独占的 stage 中工作。`review` 查看 staged 修改；`apply` 把选中的修改接受到
基础工作区；`drop` 放弃修改。具体文件系统和网络边界取决于所选 execution provider，
并随 Run 一起记录。

[完成第一个 pVisor Run →](pvisor/get-started.md)

## 工作流二：查看 Agent 历史

已经有轨迹数据，需要浏览、查询、分析、导入、导出或提供本地服务时，使用 pChronicle：

```bash
pchronicle onboard
pchronicle onboard query
```

本演练会创建临时示例 Dataset。Dataset 可以是本地路径、对象存储 URI prefix 或配置好的
alias；其中的数据既可以由 Persisting capture，也可以来自支持的外部格式。

[查看第一个 Dataset →](pchronicle/get-started.md)

## 贯通路径：从执行到可查询历史

配置 capture 后，两条工作流连成一条路径：

```text
pVisor Run ── configured Gateway/lifecycle capture ──> pChronicle Dataset

External trajectory files ───────────────────────────> pChronicle Dataset
```

当前交接会发布 Gateway 轨迹 event 和选定的 lifecycle record，但不会自动发布完整的私有
Run Bundle、全部 staged effect 或所有 provider 特定 evidence。类似地，导入外部轨迹不会
事后补造原始记录中从未存在的执行控制。详见
[捕获 Agent 轨迹](pvisor/guides/capture.md)。

两者仍可独立使用：pVisor 可以在没有 pChronicle 的情况下完成一次 Run；pChronicle 也可以
查询从未经过 pVisor 的 Dataset。

![Persisting 当前工作流与从执行到历史的贯通路径](assets/diagrams/persisting/system-products.svg)

## 下一步

- [安装 Persisting](installation.md)
- [选择 pVisor 执行环境](pvisor/guides/execution.md)
- [审查并选择性应用 Agent 修改](pvisor/guides/review-apply.md)
- [查阅 pChronicle CLI 指南](pchronicle/reference/cli.md)
- [检查架构与交付边界](system-design/index.md)

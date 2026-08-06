# 选择能力

请从你要完成的事情开始。使用指南描述已支持的工作流；架构页面解释内部设计和实验性能力。

## 按目标选择

| 你想… | 从这里开始 |
|---|---|
| 运行单个 Agent 并拥有可审查工作区 | [pVisor：run / review / checkpoint](../design/cli-pvisor.md)（设计） |
| 批量编排 Agent Run 并支持恢复 | [pPilot：run / produce](../design/cli-ppilot.md)（设计） |
| 用 SQL 查询轨迹历史 | [pPilot：query / analysis](../design/cli-ppilot.md)（设计） |
| 记录 Agent LLM 调用 | [Capture](capture.md) |
| 控制 proxy-aware Agent 工具的 HTTP/HTTPS 出口 | [OverlayNet](overlaynet.md) |
| 用张量下标存储/读取参数或 KV Cache | [Tensor Memory](tensor-memory.md) |
| 流式传输事件并持久化 | [Queue](queue.md) |
| 索引和检索文档 | [Search](search.md) |
| 接入自定义存储 | [Custom Backends](custom-backends.md) |
| 复现一个可测量的结论 | [可复现示例](examples.md) |

多数指南在 `examples/` 下配有可运行示例：每个脚本清理自己的 `.work/`，依次运行产品命令，
并直接打印生成的文件和报告。`just examples` 可一次跑完。

## 能力成熟度

| 能力 | 提供内容 | 状态 |
|---|---|---|
| [pVisor](../design/cli-pvisor.md) | 单个 Run 的执行、控制与事务工作区 | 已实现 |
| [pPilot](../design/cli-ppilot.md) | 批量编排、durable 结果、SQL 分析 | 已实现 |
| [pChronicle](../design/trajectory.md) | Canonical 事件、Storyline/ATIF、S3 存储 | 已实现 |
| [Capture](capture.md) | 捕获 LLM 流量并生成 Lance 与 Markdown 视图 | 已实现 |
| [OverlayNet](overlaynet.md) | Cooperative HTTP/HTTPS 代理策略与带宽控制 | 已实现 |
| [Search](search.md) | 文档索引与向量/混合检索 | 稳定 |
| [Queue](queue.md) | 持久事件流和 KV 风格访问 | 稳定 |
| [Tensor Memory](tensor-memory.md) | 张量下标 API 与 host/SSD block 存储 | 实验性 |
| [Custom Backends](custom-backends.md) | Queue 存储后端扩展点 | 参考 |

## 这些能力如何关联

pVisor、pPilot、pChronicle 是 Agent 基础设施：pVisor 运行单个 Run，pPilot 调度并恢复批量
Run，pChronicle 保存 canonical 历史。Gateway、OverlayNet、Control 与 OverlayFS 是 pVisor
组装的运行时驱动。Tensor Memory、Queue、Search 是独立的能力型数据系统——可单独使用，
不是 Agent runtime 的必需依赖。实现模型与成熟度说明见[架构与内部实现](../design/index.md)。

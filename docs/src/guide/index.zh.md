# 选择能力

请从你要完成的事情开始。使用指南描述已支持的工作流；架构页面解释内部设计和实验性能力。

## 按目标选择

| 你想… | 从这里开始 |
|---|---|
| 用张量下标存储/读取参数或 KV Cache | [Tensor Memory](tensor-memory.md) |
| 记录 Agent LLM 调用 | [Capture](capture.md) |
| 流式传输事件并持久化 | [Queue](queue.md) |
| 索引和检索文档 | [Search](search.md) |
| 接入自定义存储 | [Custom Backends](custom-backends.md) |

## 能力成熟度

| 能力 | 提供内容 | 状态 |
|---|---|---|
| [Capture](capture.md) | 捕获 LLM 流量并生成 Lance 与 Markdown 视图 | 稳定 |
| [Search](search.md) | 文档索引与向量/混合检索 | 稳定 |
| [Queue](queue.md) | 持久事件流和 KV 风格访问 | 稳定 |
| [Tensor Memory](tensor-memory.md) | 张量下标 API 与 host/SSD block 存储 | 实验性 |
| [Custom Backends](custom-backends.md) | Queue 存储后端扩展点 | 参考 |

## 这些能力如何关联

Capture、Search 和 Queue 都可以独立使用。pPilot 当前是内部编排库，不是受支持的
CLI 工作流。Tensor Memory 是带 TTAS
寻址的实验性存储底座，不是稳定工具的必需依赖。需要了解实现模型或路线图时，
请阅读[架构与内部实现](../design/index.md)。

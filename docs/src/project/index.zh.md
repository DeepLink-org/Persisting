# Project

Persisting 的公共定位横跨模型状态与 Agent 历史。这一节记录交付状态、稳定决策、贡献者
工作流，以及不在当前 pVisor → pChronicle 主路径中的独立系统。

## 架构

- [系统概览](../system-design/index.md)
- [端到端架构](../system-design/architecture.md)
- [从本地到集群](../system-design/local-to-fleet.md)
- [安全与 Evidence](../system-design/security-evidence.md)

## 构建与发布

- [工程笔记](engineering.md)
- [发布 Persisting](releasing.md)
- [可复现示例](examples.md)

## 决策

- [RFC 索引](../rfcs/index.md)
- [实现状态](engineering.md)

## 调研

- [TransferQueue 对比](../design/references/transfer-queue-comparison.md)
- [TransferQueue 接口映射](../design/references/transfer-queue-interface.md)
- [LMCache 分析](../design/references/lmcache.md)

## 独立数据系统

Queue 及其 Python API 独立于 Agent 执行主路径：

- [Queue 指南](../guide/queue.md)
- [Queue API](../api/queue.md)
- [自定义 Queue backend](../guide/custom-backends.md)
- [Queue 持久化设计](../design/architecture.md)

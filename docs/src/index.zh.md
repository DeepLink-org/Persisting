---
template: home.html
title: Persisting - AI 系统持久化存储
description: 为参数、KV Cache 和 Trajectory 提供的持久化存储解决方案。基于 Lance 列式存储格式，与 Pulsing Actor 框架深度集成。
hide: toc
---

<!-- 此内容被 home.html 模板隐藏，但会被搜索引擎索引 -->

# Persisting

**Persisting** 是为 AI 系统设计的持久化存储解决方案，为参数、KV Cache 和 Trajectory 提供高效存储。

## 核心特性

- **可插拔后端** - 支持多种存储后端，包括内存、Lance 和自定义实现。
- **Lance 集成** - 基于 Lance 列式存储格式，提供高性能随机访问和零拷贝版本控制。
- **Write-Ahead Log** - 内置 WAL 支持，确保数据持久性和崩溃恢复。
- **Pulsing 集成** - 与 Pulsing Actor 框架无缝集成，为分布式队列提供持久化能力。
- **Schema 演进** - 支持动态 Schema 演进，无需停机。
- **监控指标** - 内置 Prometheus 指标导出，实时监控存储状态。

## 快速开始

```bash
# 安装
pip install persisting

# 或与 Pulsing 一起安装
pip install persisting[pulsing]
```

```python
from pulsing.queue import register_backend, write_queue
from persisting.queue import LanceBackend

# 注册 Lance 后端
register_backend("lance", LanceBackend)

# 使用持久化队列
writer = await write_queue(system, "my_topic",
    backend="lance",
    storage_path="/data/queues")

await writer.put({"id": "1", "value": 42})
await writer.flush()
```

## 应用场景

- **KV Cache 存储** - 存储和检索 LLM KV Cache，支持跨会话复用。
- **Trajectory 存储** - 持久化存储强化学习和训练过程中的 Trajectory 数据。
- **参数检查点** - 高效的模型参数检查点存储，支持版本管理。

## 社区

- [GitHub 仓库](https://github.com/reiase/Persisting)
- [问题追踪](https://github.com/reiase/Persisting/issues)
- [讨论区](https://github.com/reiase/Persisting/discussions)


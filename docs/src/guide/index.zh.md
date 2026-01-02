# 用户指南

欢迎阅读 Persisting 用户指南。本指南涵盖核心概念和使用模式。

## 概述

Persisting 为 Pulsing 的分布式队列系统提供持久化存储后端。它支持可靠的数据存储，具有以下特性：

- **基于 Lance 的持久化** - 高性能列式存储
- **Write-Ahead Log (WAL)** - 数据持久性保证
- **Schema 演进** - 灵活的数据结构变更
- **监控** - 内置指标和可观测性

## 架构

```
┌─────────────────────────────────────────────────────────┐
│                    Pulsing 队列                         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐     │
│  │ QueueWriter │  │ QueueReader │  │   Queue     │     │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘     │
│         │                │                │             │
│         └────────────────┼────────────────┘             │
│                          ▼                              │
│              ┌───────────────────────┐                  │
│              │   StorageManager      │                  │
│              └───────────┬───────────┘                  │
│                          ▼                              │
│              ┌───────────────────────┐                  │
│              │   BucketStorage       │                  │
│              └───────────┬───────────┘                  │
│                          │                              │
└──────────────────────────┼──────────────────────────────┘
                           ▼
┌──────────────────────────────────────────────────────────┐
│                  StorageBackend 协议                     │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐  │
│  │MemoryBackend│  │ LanceBackend│  │PersistingBackend│  │
│  └─────────────┘  └─────────────┘  └─────────────────┘  │
│       (Pulsing)        (Persisting)     (Persisting)     │
└──────────────────────────────────────────────────────────┘
```

## 核心概念

### 存储后端

存储后端实现 `StorageBackend` 协议，提供不同的存储策略：

| 后端 | 位置 | 持久化 | 用途 |
|------|------|--------|------|
| `MemoryBackend` | Pulsing | 否 | 测试、临时数据 |
| `LanceBackend` | Persisting | 是 | 生产工作负载 |
| `PersistingBackend` | Persisting | 是 | 高级功能（WAL、指标） |

### 后端注册

使用前必须注册后端：

```python
from pulsing.queue import register_backend
from persisting.queue import LanceBackend

register_backend("lance", LanceBackend)
```

### 后端选择

创建队列时指定后端：

```python
from pulsing.queue import write_queue

# 按名称（必须已注册）
writer = await write_queue(system, "topic", backend="lance")

# 直接使用类
writer = await write_queue(system, "topic", backend=LanceBackend)
```

## 本指南主题

- [队列后端](backends.md) - 存储后端概述
- [Lance 后端](lance.md) - 使用 Lance 进行持久化
- [Persisting 后端](persisting.md) - 带 WAL 的增强后端
- [自定义后端](custom.md) - 实现自定义后端

## 下一步

选择最符合你需求的主题，或继续阅读[队列后端](backends.md)概述。


# 架构设计

## 概述

本文档描述 Persisting 的**队列持久化**架构。

### 队列持久化

Persisting 为 Pulsing 分布式 streaming 队列系统提供可插拔的存储后端。数据通过 Pulsing 的 Actor 网络流入，最终落盘为 Lance 列式数据集。

```
应用代码
    │
    ▼ put(record)
QueueWriter (Pulsing)
    │
    ▼ 按 bucket_column 路由
StorageManager Actor (Pulsing, 每节点一个)
    │
    ▼ 一致性哈希 → bucket 所有者
BucketStorage Actor (Pulsing)
    │
    ▼ 委托给后端
StorageBackend (Persisting)
    │
    ▼ 缓冲 → flush
Lance Dataset (磁盘)
```

## LanceBackend

内存缓冲 + Lance 持久化的核心存储：

- 记录先写入内存缓冲区。
- 缓冲达到 `batch_size` 或调用 `flush()` 时，数据写入 Lance 数据集。
- 读取时透明合并已持久化数据和缓冲区数据。
- 启动时从已有数据集恢复持久化记录计数。

## PersistingBackend

继承 LanceBackend，增加操作指标：

- put/get/flush 计数器
- 最近 flush 时间戳
- 通过 `stats()` 或 `get_metrics()` 获取

## 并发模型

所有后端使用 `asyncio.Condition` 保证线程安全：

- 写入端：获取锁，追加到缓冲区，通知等待中的读取端。
- 读取端：获取锁，读取持久化 + 缓冲区数据；如果 `wait=True`，阻塞在条件变量上直到有新数据。
- Flush：获取锁，交换缓冲区，释放锁，写入 Lance。

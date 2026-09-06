# Queue

有时候你不需要随机访问——你需要一个流。训练数据从生产者流向消费者。Agent 轨迹事件按顺序追加。日志记录你以后会顺序扫描。

Persisting Queue 提供基于 Lance 的持久化追加式队列——与 Persisting 其他部分相同的 Lance 引擎。本地启动只需三行代码，扩展到分布式零代码改动。

---

## 最简单的开始

```python
import asyncio
from persisting import Queue

async def main():
    q = Queue("events", storage_path="./data")

    await q.put({"step": 1, "reward": 0.5})
    await q.put({"step": 2, "reward": 0.8})
    await q.flush()

    records = await q.get(limit=100)
    print(f"读取了 {len(records)} 条记录")  # 读取了 2 条记录

asyncio.run(main())
```

就这些——Lance 持久化、达到 `batch_size` 自动 flush、重启后恢复。记录在内存缓冲中累积，缓冲满或显式调用 `flush()` 时落盘。

```python
q = Queue("events",
    storage_path="./data",
    batch_size=100,                # 累积 100 条时 flush
    auto_flush_interval_sec=1.0,   # 或者每秒一次，先到为准
    enable_metrics=True,           # 追踪 put/get/flush 计数
)
```

---

## 从本地到分布式——零代码改动

Pulsing 运行时，Queue 自动切换到分布式模式。相同的 `q.put()` 和 `q.get()` 调用，现在通过 Pulsing actor 路由，数据通过一致性哈希跨节点分片：

```python
import pulsing

await pulsing.init()           # 启动 Pulsing 集群
q = Queue("events")            # 自动分布式——API 不变

await q.put({"step": 1})       # 哈希到某个 bucket，路由到某个节点
records = await q.get(limit=100)  # 从所有 bucket 读取
```

底层，记录按 `bucket_column`（默认 `"id"`）哈希到 `num_buckets` 个分片。每个分片由某节点上的 Pulsing BucketStorage actor 持有。Reader 可以分配子集：

```python
# 两个消费者，各自读不同的 bucket
reader_0 = q.reader()   # Pulsing 下: rank=0, world_size=2
reader_1 = q.reader()   # Pulsing 下: rank=1, world_size=2
```

大张量负载可启用零拷贝传输：

```python
q = Queue("events", zerocopy_mode="auto")   # 避免 pickle + 拷贝
```

---

## 超越简单消息：KV 和 Tensor API

简单的 `put`/`get` 够用于消息。但训练流水线通常需要更多：键控访问、字段级读取、消费追踪。

### 批量读取（含元数据）

对于张量密集型工作负载，`get_meta` + `get_data` 提供兼容 TransferQueue 的两阶段读取：

```python
from persisting import Queue, SequentialSampler

q = Queue("training_data", storage_path="./data")
reader = q.reader()

# 第一阶段：有哪些数据可用？
meta = await reader.get_meta(
    fields=["input_ids", "attention_mask", "labels"],
    batch_size=32,
    task_name="actor_train",
    partition_id="train_0",
    sampler=SequentialSampler(),
)
# meta.size, meta.global_indexes, meta.field_names

# 第二阶段：拉取实际张量
batch = await reader.get_data(meta, partition_id="train_0")

# 或者一步完成
batch = await reader.get_batch(
    fields=["input_ids", "attention_mask"],
    batch_size=32,
    task_name="actor_train",
    partition_id="train_0",
    sampler=SequentialSampler(),
)
```

这解耦了"决定读什么"和"读取数据"——当不同消费者需要不同字段子集时很有用。

### 键值访问

对于需要按键随机访问（而非仅顺序扫描）的工作负载，用 `KVInterface` 包装 Queue：

```python
from persisting import Queue, KVInterface

q = Queue("kv_store", storage_path="./data")
kv = KVInterface(q)

# 按键写入
await kv.kv_put("sess-42", data=tensor_dict, partition_id="prod",
    tag={"status": "complete", "model": "llama-70b"})

# 批量写入
await kv.kv_batch_put(
    ["sess-43", "sess-44"],
    data=two_session_batch,
    partition_id="prod",
)

# 按键获取
data = await kv.kv_batch_get(["sess-42", "sess-43"], partition_id="prod")

# 列出 key 及其 tag
pairs = await kv.kv_list("prod")
# → [("sess-42", {"status": "complete", ...}), ("sess-43", {}), ...]

# 清理
await kv.kv_clear(["sess-42"], partition_id="prod")
```

### 消费追踪

训练循环需要知道哪些已被消费：

```python
# 标记记录已被此任务处理
await q.mark_consumed("actor_train", global_indexes=[1, 2, 3], partition_id="train_0")

# 重置消费状态，重新训练
await q.reset_consumption("actor_train", partition_id="train_0")
```

---

## 采样策略

Sampler 控制 reader 拉取哪些记录。选择匹配你训练设置的方案：

```python
from persisting import SequentialSampler, RankAwareSampler, GRPOGroupNSampler

# 简单：按顺序消费
seq = SequentialSampler()

# 分布式训练：每个 rank 拿独立分片
rank = RankAwareSampler()

# GRPO：每个 prompt 组采样 N 条
grpo = GRPOGroupNSampler(n_samples_per_prompt=4)
```

---

## 流式消费

当消费者比队列中当前数据活得更久时，阻塞等待新记录：

```python
async for batch in q.stream(limit=1000, wait=True, timeout=5.0):
    process(batch)
```

- `wait=True`：不返回空——阻塞直到数据到达
- `timeout=5.0`：等待 5 秒后放弃

---

## 存储布局

```
./data/events/
└── data.lance/         ← Lance 列式数据集
    ├── _versions/      ← 版本化写入
    └── data/           ← 列文件
```

记录以 Lance 列式格式存储。Schema 从第一批数据推断——`int` → `int64`，`float` → `float64`，`bool` → `bool`，`str` → `string`，其他 → pickle 二进制。Lance 引擎处理一致性、恢复和高效列扫描。

---

## 下一步

- [API 参考 — Queue](../api/queue.md) — 所有方法签名
- [自定义后端](custom-backends.md) — 用自己的存储引擎替换 Lance

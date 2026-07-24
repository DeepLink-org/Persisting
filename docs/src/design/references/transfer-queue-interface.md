# TransferQueue 与 Pulsing Queue 接口设计对比

本文档对 **TransferQueue (TQ)** 与 **Pulsing Queue** 的接口做逐项对照，便于迁移或能力对齐。

---

## 1. 初始化与入口

| 能力 | TransferQueue | Pulsing Queue |
|------|---------------|---------------|
| **初始化** | `tq.init(conf?)` 一次，全局单例 Controller + Client | 无全局 init；使用 `ActorSystem`，按 topic 打开 writer/reader |
| **关闭** | `tq.close()` 清理 Controller、Storage、Client | 无显式 close；随 ActorSystem/进程结束 |
| **写入口** | 无独立“写句柄”；`tq.put()` / `tq.kv_put()` 等直接用 | `writer = await write_queue(system, topic, ...)` → `QueueWriter` |
| **读入口** | 无独立“读句柄”；`tq.get_meta()` + `tq.get_data()` 或 `tq.kv_batch_get()` | `reader = await read_queue(system, topic, ...)` → `QueueReader` |
| **底层句柄** | `client = tq.get_client()` 获得 `TransferQueueClient` 用低层 API | 直接使用 `Queue` / `QueueWriter` / `QueueReader`，无单独 Client 类 |

**差异要点**：TQ 是“先 init 再 put/get”的全局单例；Pulsing 是“按 topic 打开 writer/reader”，无全局队列 init。

---

## 2. 写入（Put）

| 能力 | TransferQueue | Pulsing Queue |
|------|---------------|---------------|
| **单条写入** | `tq.kv_put(key, partition_id, fields?, tag?)`；或 `client.put(data: TensorDict, metadata?, partition_id?)` | `await writer.put(record: dict)`，单条或 `list[dict]` |
| **批量写入** | `tq.kv_batch_put(keys, partition_id, fields?, tags?)`；或 `client.put(data, metadata?, partition_id?)` | `await writer.put(records: list[dict])`（同一 API，传入 list） |
| **数据形态** | TensorDict / dict of tensors；支持 tag/custom_meta | `dict[str, Any]`（可含任意可序列化字段） |
| **分区语义** | `partition_id: str`（逻辑分区，由 Controller 管理） | `topic` + 按 `bucket_column` 哈希到 `num_buckets`（无显式 partition_id 字符串） |
| **键语义** | KV：显式 `key`（及可选的 tag） | 无内置 key；可用 record 中某字段当逻辑 key（如 `id`） |
| **Flush** | 无独立 flush API（写入走 Storage 单元） | `await writer.flush()` 刷写各 bucket 缓冲 |

**对应关系**：
- TQ `partition_id` ≈ Pulsing `topic`（或 topic + bucket 子集）。
- TQ `kv_put` / `kv_batch_put` ≈ Pulsing `writer.put(record)` / `writer.put([...])`，若不需要“先 get_meta 再填数据”的流程，可简化为一次 put。
- Pulsing 的 `bucket_column` + 哈希 ≈ TQ 的 partition 内再分桶；Pulsing 无“按 key 查”的 KV 层。

---

## 3. 读取（Get）

| 能力 | TransferQueue | Pulsing Queue |
|------|---------------|---------------|
| **两阶段读** | `get_meta(data_fields, batch_size, partition_id, mode?, ...)` → BatchMeta，再 `get_data(batch_meta)` → TensorDict | 无；直接 `get` 一次拿数据 |
| **单次读** | 无“单次 get”高层 API；KV 路径：`kv_batch_get(keys, partition_id, fields?)` → TensorDict | `await reader.get(limit=..., wait=..., timeout=...)` → `list[dict]` |
| **按 key 读** | `kv_batch_get(keys, partition_id, fields?)` | 无；按 offset 顺序读，无按 key 取 |
| **流式读** | 无独立“流式”API；可循环 get_meta + get_data | `bucket.get_stream(limit, offset, wait, timeout)` 或 Queue 内部用 get_stream 做 fallback |
| **读模式** | `get_meta(..., mode="fetch"|"force_fetch")` 控制是否只拿 ready / 是否强制拿 | 无 mode；通过 `wait`/`timeout` 控制阻塞等待新数据 |
| **Sampler** | Controller 集成 Sampler（如 Sequential、GRPO），get_meta 时用 `sampling_config` | 无；消费顺序由 offset 或应用层决定 |

**对应关系**：
- TQ 的“get_meta → get_data”若只用于“先看元数据再拉数据”，在 Pulsing 可等价为一次 `reader.get(limit)`；若需要“只取部分字段”，可在应用层从 `list[dict]` 里筛字段。
- TQ `kv_batch_get(keys, ...)` 在 Pulsing 无直接对应；若需“按 key 取”，需在 Pulsing 上做薄层（例如维护 key→offset 或二次查询）。

---

## 4. 多消费者与分区

| 能力 | TransferQueue | Pulsing Queue |
|------|---------------|---------------|
| **分区/分桶** | `partition_id` 字符串；Controller 管理分区与状态 | `topic` + `num_buckets`；每个 bucket 一个 BucketStorage Actor |
| **多消费者** | 通过 Sampler + 状态（生产/消费状态）在 Controller 侧协调 | 每个 `QueueReader` 可指定 `bucket_ids` 或 `rank`/`world_size`，自动分配 bucket，各 reader 独立 offset |
| **按 rank 分配** | 无内置 rank/world_size 读 API；需自行与 Sampler 或 partition 约定 | `read_queue(system, topic, rank=i, world_size=N)` 自动分配 bucket 子集 |
| **Offset** | 由 Controller/Sampler 管理，不暴露给用户 | `QueueReader` 内部维护 `_offsets[bucket_id]`；可 `reset()`、`set_offset(offset, bucket_id?)` |

**对应关系**：Pulsing 的 `rank`/`world_size` 读 + 每 reader 独立 offset，可覆盖 TQ 多消费者场景；TQ 的“状态 + Sampler”在 Pulsing 中需在应用层或薄层实现。

---

## 5. KV 与元数据

| 能力 | TransferQueue | Pulsing Queue |
|------|---------------|---------------|
| **按 key 列出** | `kv_list(partition_id?)` → `{partition_id: {key: {tag...}}}` | 无；无 key 概念 |
| **按 key 删除** | `kv_clear(keys, partition_id)` | 无；无按 key 删除 |
| **仅写 tag（无 data）** | `kv_put(key, partition_id, tag=...)` 或 `set_custom_meta(batch_meta)` | 无；put 必须带 record（可只含元数据字段） |
| **custom_meta/tag** | BatchMeta 带 custom_meta；get_meta 可拿到 | record 为任意 dict，可自带元数据字段 |

**对应关系**：TQ 的 KV + tag 在 Pulsing 中可用“record 内嵌 key/tag 字段 + 应用层 list/filter”近似；若需精确 kv_list/kv_clear，需在 Queue 之上做薄层或单独存储。

---

## 6. 清除与统计

| 能力 | TransferQueue | Pulsing Queue |
|------|---------------|---------------|
| **按分区清空** | `client.clear_partition(partition_id)` | 无内置 clear；可新 topic 或后端实现 truncate |
| **按样本清空** | `client.clear_samples(batch_meta)` | 无 |
| **统计信息** | 无统一 stats 接口（Controller/Storage 内部） | `await queue.stats()` / `bucket.stats()`；backend 有 `total_count()` |

---

## 7. 同步 / 异步

| 能力 | TransferQueue | Pulsing Queue |
|------|---------------|---------------|
| **同步 API** | `put`, `get_meta`, `get_data`, `kv_put`, `kv_batch_put`, `kv_batch_get`, `kv_list`, `kv_clear` | `SyncQueueWriter` / `SyncQueueReader`（`writer.sync()`, `reader.sync()`） |
| **异步 API** | `async_put`, `async_get_meta`, `async_get_data`, `async_kv_*` | `QueueWriter` / `QueueReader` 全为 async（`put`, `get`, `flush` 等） |

---

## 8. 小结：迁移与简化建议

- **写入**：TQ `kv_put` / `kv_batch_put` 或 `put(data, metadata?, partition_id?)` → Pulsing `writer.put(record)` / `writer.put(list)`；partition 用 topic（或 topic+bucket 子集）表达。
- **读取**：TQ `get_meta` + `get_data` 可简化为 Pulsing 一次 `reader.get(limit)`；若需“按 key 取”或“只取部分字段”，在应用层或薄层实现。
- **多消费者**：用 `read_queue(..., rank=, world_size=)` + 每 reader 独立 offset 即可。
- **KV / 状态 / Sampler**：TQ 独有；在 Pulsing 上可用 record 内字段 + 应用逻辑或薄层补齐，不必在核心 Queue 里实现同等复杂度。

上述对照表可直接用于迁移清单或接口对齐讨论；若某条需要更细的语义（如 mode、sampling_config），可再按 TQ 文档逐参数对齐。

---

## 9. 性能差异分析

以下从数据路径、序列化、传输、瓶颈与扩展性等维度分析两套队列可能存在的性能差异（定性为主，具体数值依赖部署与负载）。

### 9.1 读路径延迟（RTT）

| 维度 | TransferQueue | Pulsing Queue |
|------|---------------|---------------|
| **一次“读一批”的 RTT** | **2 次**：先 `get_meta`（Client → Controller），再 `get_data`（Client → Storage 单元，可能多台并行） | **1 次**：`reader.get(limit)` 直接对 BucketStorage Actor 发起请求，一次往返拿数据 |
| **KV 路径** | `kv_batch_get` 内部也是 get_meta（按 key 查） + get_data，仍是 2 次 | 无对应；若在应用层按 key 查，可能多次 get |
| **影响** | 纯延迟敏感、小 batch 场景下，TQ 多一次网络往返，尾延迟和平均延迟更高 | 单次 get 延迟更易做低 |

**结论**：在“一次读一批”的简单场景下，Pulsing 读路径更短，延迟通常更优；TQ 的两次往返换来的是“先看元数据、再按需拉数据”或“只拉部分字段”等能力，若不需要这些，等价于多付出一轮 RTT。

### 9.2 序列化与零拷贝

| 维度 | TransferQueue | Pulsing Queue |
|------|---------------|---------------|
| **数据格式** | TensorDict / torch.Tensor；定制 msgpack 编码，**支持零拷贝**（tensor 用 buffer 引用，不复制大块内存） | `dict[str, Any]`；Python 间走 **pickle**（运行时内部），大对象会复制 |
| **大 tensor 传输** | 理论上有零拷贝或共享内存潜力（见 `serial_utils.py` 的 buffer 收集与 zmq `copy=False`） | 默认无零拷贝；大 payload 序列化/反序列化与拷贝成本明显 |
| **适用负载** | 大量 tensor、大 batch、高带宽场景更有利 | 小 record、控制面或非 tensor 为主时足够 |

**结论**：**大 tensor、高吞吐**场景下 TQ 的零拷贝与专用序列化更有优势；Pulsing 若以 dict/小对象为主，差距较小，若在 Python 侧传大 numpy/tensor，可考虑在应用层做共享内存或仅传引用，或接持久化后端减少大对象经 Actor 传递。

### 9.3 传输层与缓冲

| 维度 | TransferQueue | Pulsing Queue |
|------|---------------|---------------|
| **传输** | **ZMQ**（DEALER/ROUTER），单次连接、大 socket 缓冲（如 0.5GB RCVBUF/SNDBUF） | Actor RPC（Pulsing 底层，Python 侧通常 pickle）；缓冲与背压由运行时决定 |
| **背压** | ZMQ 高水位与缓冲可吸收短时突发，但无应用层流控 | 依赖 Actor 队列与调度，易形成背压 |
| **批量发送** | `send_multipart(..., copy=False)` 可减少内核拷贝 | 每请求单独序列化与发送 |

**结论**：TQ 在大批量、高带宽、单连接长会话上，ZMQ + 大缓冲 + copy=False 有利于吞吐；Pulsing 更依赖并发多 bucket 和请求级并行，单连接未必达到同等峰值带宽。

### 9.4 瓶颈与扩展性

| 维度 | TransferQueue | Pulsing Queue |
|------|---------------|---------------|
| **中心节点** | **Controller 单点**：get_meta、状态更新、Sampler 均经 Controller，高 QPS 时易成瓶颈 | 无全局 Controller；每个 topic 的 **多个 BucketStorage** 分散在不同节点，读写在 bucket 维度并行 |
| **写扩展** | 写数据直连 Storage 单元，可多单元并行；但 metadata/状态仍经 Controller | 写按 bucket 哈希到不同 Actor，**天然多路并行**，无单点 |
| **读扩展** | get_meta 受 Controller 限制；get_data 可对多 Storage 单元并行 | 多 Reader 按 bucket 或 rank 分片，**各读各 bucket**，线性扩展 |
| **分区数** | partition 数由配置/Controller 管理；Storage 单元数可独立配置 | bucket 数 `num_buckets` 直接决定并行度，调参简单 |

**结论**：**多生产者/多消费者、高 QPS** 场景下，Pulsing 的“无中心 + 多 bucket”更容易水平扩展；TQ 在 Controller 未做分片或多副本前，get_meta 容易成为瓶颈。

### 9.5 批量化与并发

| 维度 | TransferQueue | Pulsing Queue |
|------|---------------|---------------|
| **批量 put** | `put(TensorDict)` 或 `kv_batch_put`，一次请求一批；Storage 侧按单元接收 | `writer.put(list[dict])` 一次多条；内部按 bucket 分组后可能多 actor 并发调用 |
| **批量 get** | get_meta 指定 `batch_size`，get_data 一次取回整批；可对多 Storage 单元并发 get_data | `reader.get(limit=N)` 一次取 N 条；若跨 bucket 则内部多 bucket 串行或有限并行 |
| **流式** | 无原生流式 API，靠循环 get_meta + get_data | `get_stream` 支持，便于长时消费与背压 |

**结论**：两者都支持批量读写；TQ 的 get_data 在跨多 Storage 单元时已做并发，Pulsing 的 `Queue.get` 跨 bucket 时目前是顺序向各 bucket 要数据，若 bucket 很多且每 bucket 数据均匀，可考虑在 Queue 层做并发 get 以进一步提高读吞吐。

### 9.6 小结：何时谁可能更快

- **读延迟（单次 get）**：Pulsing 更优（1 RTT vs 2 RTT），尤其小 batch、延迟敏感。
- **大 tensor / 高带宽读写**：TQ 更有潜力（零拷贝、ZMQ 大缓冲、专用序列化）；Pulsing 需避免在 Python 侧经 pickle 传大块。
- **多客户端、高 QPS、水平扩展**：Pulsing 更易扩展（无 Controller 单点、多 bucket 并行）；TQ 需关注 Controller 与 get_meta 的瓶颈。
- **实际差异**：取决于部署（同机/跨机、网络、磁盘）、记录大小、batch 大小、并发度；建议用各自 benchmark（如 Pulsing 的 `benchmarks/queue_benchmark.py`、`baseline_throughput.py`）在目标负载下实测。

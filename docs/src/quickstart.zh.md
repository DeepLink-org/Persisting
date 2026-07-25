# 快速开始

5 分钟上手 Persisting。

## 安装

```bash
pip install persisting[lance]
```

CLI 工具（`persisting traj`、`persisting compute`、`persisting search`）需[从源码构建](installation.md)。

---

## 核心：统一张量存储

Persisting 通过同一个 `persisting.open()` 接口存储轨迹、参数和 KV Cache。三者共用 TTAS（分层张量地址空间）——同一套寻址、同一个 Lance 引擎、同一种分层。

```python
import persisting
from persisting.core import Dimension
```

### 参数

按名称和分片寻址模型权重：

```python
PARAM_ID = Dimension("param_id", "str")
SHARD    = Dimension("shard", "int")

ps = persisting.open("params/llama-70b",
    dims=(PARAM_ID, SHARD),
    backend="tiered",
    shape=(100, 8),
)

weights = ps["embed.weight", 0].tensor()
ps["lm_head.weight", 0].put(updated_tensor)
```

### KV Cache

跨会话、多层 KV Cache，Block 粒度分层：

```python
SESSION = Dimension("session", "str")
LAYER   = Dimension("layer", "int")
HEAD    = Dimension("head", "int")
TIME    = Dimension("time", "int")

kv = persisting.open("kvcache/v1",
    dims=(SESSION, LAYER, HEAD, TIME),
    order_dim=TIME,
    backend="tiered",
    shape=(100, 32, 8, 4096),
    block_tokens=64,
)

h = kv["s1", 0, 2, 0:512]      # 切片（零拷贝）
arr = h.tensor()                 # 从最快层物化
h.put(other_data)                # 写回
kv.prefetch(("s1", 0, 0:1024))  # 异步拉 block 到 host 内存
```

### 轨迹（通过 Queue）

轨迹事件通过 Queue API 存储，底层同一套 Lance 引擎：

```python
from persisting import Queue

q = Queue("trajectories", storage_path="./data")
await q.put({"run_id": "r1", "step": 1, "reward": 0.5})
await q.flush()
records = await q.get(limit=100)
```

→ [Tensor Memory 指南](guide/tensor-memory.md) 了解后端、维度和 Block 存储详情。

---

## 同一底座上的工具

### Agent 采集

记录每一次 LLM 调用——Claude Code、Codex 或自定义脚本：

```bash
persisting traj capture -o ./store -c proxy.toml -f md -- claude
```

→ [Capture 指南](guide/capture.md)

### 队列与 KV API

兼容 TransferQueue 的追加/消费 API：

```python
from persisting import Queue, SequentialSampler

q = Queue("training_data", storage_path="./data")
reader = q.reader()

meta = await reader.get_meta(
    fields=["input_ids"], batch_size=32, task_name="train",
    partition_id="p0", sampler=SequentialSampler())
batch = await reader.get_data(meta, partition_id="p0")
```

→ [Queue 指南](guide/queue.md)

### 检索

```python
from persisting.search import add_document, query

add_document("docs", "要索引的文本...")
results = query("docs", "搜索查询", mode="hybrid", k=10)
```

→ [Search 指南](guide/search.md)

### 计算编排

Map 式任务，支持断点续跑：

```bash
persisting compute task.py -w 4 --check       # 验证
persisting compute task.py -w 4 -- --n 1000   # 运行
```

→ [Compute 指南](guide/compute.md)

---

## 下一步

- [用户指南](guide/index.md) — 深入指南
- [API 参考](api/index.md) — 完整 API 文档
- [设计文档](design/index.md) — 架构、TTAS、分层存储

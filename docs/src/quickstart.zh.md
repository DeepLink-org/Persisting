# 快速开始

5 分钟上手 Persisting。

## 你将获得什么

Persisting 为参数、KV Cache 和轨迹提供持久化存储——以 Lance 为存储引擎，GPU/host/SSD 分层。当前已可用：**流式追加**（append-only 队列）。即将推出：**Tensor Memory API**（多维寻址访问）。

## 步骤 1：安装

```bash
pip install persisting[lance]
```

## 步骤 2：Tensor Memory（即将推出）

主要 API——tensor 下标式访问分层内存：

```python
import persisting
from persisting.core import Dimension

SESSION = Dimension("session", "str")
LAYER   = Dimension("layer", "int")
HEAD    = Dimension("head", "int")
TIME    = Dimension("time", "int")

kv = persisting.open("kvcache/v1",
    dims=(SESSION, LAYER, HEAD, TIME), order_dim=TIME)

arr = kv["s1", 0, 2, 0:512].tensor()
```

## 步骤 3：流式追加（已可用）

Lance 存储引擎上的 append-only 队列——用于轨迹收集和事件流：

```python
import asyncio
from persisting import Queue

async def main():
    queue = Queue("my_topic", storage_path="./data")

    for i in range(10):
        await queue.put({"id": str(i), "value": i * 10})
    await queue.flush()

    records = await queue.get(limit=100)
    print(f"读取了 {len(records)} 条记录")

asyncio.run(main())
```

带指标：

```python
queue = Queue("my_topic", storage_path="./data", enable_metrics=True)
await queue.put({"id": "1", "value": 42})
await queue.flush()
stats = await queue.stats()
print(stats["metrics"])
```

## 下一步

- [用户指南](guide/index.md) — 详细指南
- [设计文档](design/index.md) — 架构和 TTAS 规范
- [API 参考](api_reference.md) — 完整 API 文档

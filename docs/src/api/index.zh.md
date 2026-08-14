# Python API 参考

Python wheel 提供持久化 Queue 和采样 API。pVisor、pPilot 和 pChronicle
等 Agent 运行时能力由同一个平台 wheel 携带的原生 CLI 程序提供。

| 模块 | 用途 | 状态 |
|---|---|---|
| [Queue](queue.md) | `persisting.Queue` — 事件流、KV 接口和 Sampler | 稳定 |

## Queue 与采样

```python
from persisting import Queue, SequentialSampler

queue = Queue("events", storage_path="./data")
sampler = SequentialSampler()
```

完整接口见 [Queue API](queue.md)。

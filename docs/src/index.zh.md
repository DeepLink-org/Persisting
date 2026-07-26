---
template: home.html
title: Persisting — 轨迹、参数与 KV Cache 的统一持久化存储
description: AI 工作负载的统一分层存储。Agent 轨迹、模型参数和 KV Cache 共享同一套多维寻址、Lance 列式引擎和 Pulsing 分布式能力。
hide: toc
---

# Persisting

**轨迹、参数与 KV Cache 的统一持久化存储。**

Agent 轨迹、模型参数和 KV Cache 共享同一套分层存储 —— **一套寻址模型、一个存储引擎、一个分布式运行时**。数据跨 GPU、host 内存和 SSD 分布，以张量下标寻址，按需物化。

---

## 一套存储，三类数据

<div class="grid cards" markdown>

-   **📝 Agent 轨迹**

    ---

    记录每一次 LLM 调用。以 `(agent_id, run_id, time)` 张量存储，附带 canonical Lance 事件日志和人读 Markdown。

    ```bash
    persisting traj capture -o ./store -c proxy.toml -- claude
    ```

    → [Capture 指南](guide/capture.md)

-   **⚖️ 模型参数**

    ---

    按名称和分片寻址权重。Host 内存与 SSD 分层存储。Write-through 到 Lance 基线。

    ```python
    ps = persisting.open("params/llama-70b",
        dims=(PARAM_ID, SHARD), shape=(100, 8),
        backend="tiered")
    weights = ps["embed.weight", 0].tensor()
    ```

    → [Tensor Memory 指南](guide/tensor-memory.md)

-   **🧠 KV Cache**

    ---

    跨会话、多层的 KV Cache。Block 粒度分层，支持预取。与轨迹相同的 `(session, layer, head, time)` 寻址。

    ```python
    kv = persisting.open("kvcache/v1",
        dims=(SESSION, LAYER, HEAD, TIME),
        order_dim=TIME, backend="tiered",
        shape=(100, 32, 8, 4096))
    arr = kv["s1", 0, 2, 0:512].tensor()
    ```

    → [Tensor Memory 指南](guide/tensor-memory.md)

</div>

---

## 同一底座上的工具

<div class="grid cards" markdown>

-   **📬 流式队列**

    ---

    追加/消费事件流。Lance 持久化，兼容 TransferQueue 的 KV API、Sampler。

    ```python
    q = Queue("events", storage_path="./data")
    await q.put({"step": 1, "reward": 0.5})
    ```

    → [Queue 指南](guide/queue.md)

-   **🔍 Agent 检索**

    ---

    导入文档，构建 IVF-PQ 索引，向量/混合检索。同样的 Lance 引擎。

    ```python
    from persisting.search import add_document, query
    results = query("docs", "如何配置代理")
    ```

    → [Search 指南](guide/search.md)

-   **⚡ 计算编排**

    ---

    Map 式任务编排。`plan()` + `execute()`，本地并行或 torchrun。

    ```bash
    persisting ppilot task.py -w 4 -- --n 1000
    ```

    → [pPilot 指南](guide/ppilot.md)

</div>

---

## 架构

```
┌──────────────────────────────────────────────────────────────┐
│  轨迹                  参数                  KV Cache        │
│  (run_id, time)        (param_id, shard)    (sess, layer, …)│
├──────────────────────────────────────────────────────────────┤
│                      TTAS                                    │
│              分层张量地址空间                                  │
│         一套寻址模型，覆盖所有 AI 数据                          │
├──────────────────────────────────────────────────────────────┤
│   分层:  GPU (L0)  ↔  Host (L1)  ↔  SSD (L3)               │
│   路由:  Pulsing actor 运行时                                  │
├──────────────────────────────────────────────────────────────┤
│              Lance 列式存储                                    │
│              所有数据共享的 SSD 基线                            │
└──────────────────────────────────────────────────────────────┘
```

---

## 状态

| 组件 | 状态 |
|------|------|
| 轨迹采集 (`traj`) | ✅ 稳定 |
| 流式队列 | ✅ 稳定 |
| 计算编排 | ✅ 稳定 |
| Agent 检索 | ✅ 稳定 |
| 张量内存 (TTAS) | 🧪 实验性 |
| GPU 分层 / 跨节点 | 📋 规划中 |

---

## 快速安装

```bash
pip install persisting[lance]
```

→ [安装指南](installation.md) · [快速开始](quickstart.md)

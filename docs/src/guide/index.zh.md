# 用户指南

Persisting 为 AI 工作负载提供**统一的分层存储**。轨迹、参数和 KV Cache 共享同一套寻址模型（TTAS）、存储引擎（Lance）和分布式运行时（Pulsing）。

## 核心：统一存储

| 指南 | 数据类型 | 维度 | 状态 |
|------|---------|------|------|
| [Tensor Memory](tensor-memory.md) | 参数、KV Cache、轨迹 | `(param_id, shard)`, `(session, layer, head, time)`, `(run_id, time)` | 🧪 实验性 |

三类数据都使用 `persisting.open()` 加同一套 TTAS 寻址。Block 粒度分层，跨 host 内存和 SSD（GPU 规划中）。

## 同一底座上的工具

| 指南 | 描述 | 状态 |
|------|------|------|
| [Capture](capture.md) | 代理并记录 LLM 流量 — `persisting traj` | ✅ 稳定 |
| [Queue](queue.md) | 追加/消费事件流、KV API、Sampler | ✅ 稳定 |
| [Search](search.md) | 文档索引与向量/混合检索 | ✅ 稳定 |
| [Compute](compute.md) | Map 式任务编排 — `plan()` + `execute()` | ✅ 稳定 |
| [Custom Backends](custom-backends.md) | 实现自定义存储后端 | 📖 参考 |

---

## 架构

```
┌───────────────────────────────────────────────────────┐
│  轨迹                 参数                 KV Cache   │
│  (run_id, time)       (param_id, shard)   (sess, …)  │
├───────────────────────────────────────────────────────┤
│                    TTAS                                │
│              分层张量地址空间                            │
├───────────────────────────────────────────────────────┤
│   分层:  GPU (L0)  ↔  Host (L1)  ↔  SSD (L3)         │
│   路由:  Pulsing actor 运行时                           │
├───────────────────────────────────────────────────────┤
│              Lance 列式存储                              │
└───────────────────────────────────────────────────────┘
```

## 选择入口

| 你想… | 从这里开始 |
|------|-----------|
| 用张量下标存储/读取参数或 KV Cache | [Tensor Memory](tensor-memory.md) |
| 记录 Agent LLM 调用 | [Capture](capture.md) |
| 流式传输事件并持久化 | [Queue](queue.md) |
| 索引和检索文档 | [Search](search.md) |
| 运行批量任务并断点续跑 | [Compute](compute.md) |
| 接入自定义存储 | [Custom Backends](custom-backends.md) |

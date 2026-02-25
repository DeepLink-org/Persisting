# 用户指南

欢迎阅读 Persisting 用户指南。

## Persisting 是什么？

Persisting 为参数、KV Cache 和轨迹提供持久化存储。数据分布在 GPU、host 内存和 SSD 上——以 tensor 下标寻址，按需物化。

## 架构

```
┌───────────────────────────────────────────────────────┐
│  应用层                                               │
│  kv["s1", 0, 2, 0:512].tensor()                      │
├───────────────────────────────────────────────────────┤
│  访问模式                                             │
│  多维查找 · 流式追加 · 批量点查                        │
├───────────────────────────────────────────────────────┤
│  Persisting 核心                                      │
│  TAA (寻址) · 分层 (GPU/Host/SSD) · 路由              │
├───────────────────────────────────────────────────────┤
│  存储引擎: Lance                                      │
│  列式格式 · SSD 持久化 · 基线读取路径                  │
└───────────────────────────────────────────────────────┘
```

## 指南

### Tensor Memory（即将推出）

主要 API——tensor 下标式访问分层内存：

```python
kv = persisting.open("kvcache/v1", dims=(...), order_dim=TIME)
arr = kv["s1", 0, 2, 0:512].tensor()
```

### 流式追加（已可用）

Lance 存储引擎上的 append-only 访问模式——用于轨迹收集和事件流。这是当前已可用的生产能力。

- [队列后端](backends.md) — 存储后端概述
- [Lance 后端](lance.md) — 使用 Lance 进行持久化
- [Persisting 后端](persisting.md) — 带指标的增强后端
- [自定义后端](custom.md) — 实现自定义后端

## 下一步

- Tensor Memory API 请参阅[设计文档](../design/index.md)和 [TAA 规范](../design/tensor_address_algebra.md)。
- 流式追加请继续阅读[队列后端](backends.md)概述。

---
template: home.html
title: Persisting - 参数、KV Cache 与轨迹的持久化存储
description: AI 分布式分层内存——为 Pulsing 扩展多维寻址、GPU/host/SSD 分层、基于 actor 运行时的分布式数据面。
hide: toc
---

<!-- 此内容被 home.html 模板隐藏，但会被搜索引擎索引 -->

# Persisting

**参数、KV Cache 与轨迹的持久化存储。**

为 [Pulsing](https://github.com/DeepLink-org/pulsing)（分布式 actor 运行时）扩展分布式分层内存：GPU ↔ host ↔ SSD + 多维 tensor 寻址 + 基于 Pulsing 的分布式能力。

## 核心思路

- **Pulsing** = 控制面（actor 发现、消息传递、生命周期）
- **Persisting** = 数据面（多维地址、分层内存、放置策略）

二者组合：actor 间通过 Pulsing 通信，通过 Persisting 共享 tensor 数据——数据在多层存储上分布，以 tensor 下标寻址，按需物化。

## 核心特性

- **Tensor Address Algebra (TAA)** — 面向 AI 数据（KV Cache、参数、轨迹）的多维寻址模型。
- **分层内存** — GPU ↔ host ↔ SSD，对应用透明。
- **Pulsing 分布式** — 通过 Pulsing 的 actor 运行时实现跨节点数据访问。
- **Tensor 式 API** — `kv["s1", 0, 2, 0:512].tensor()` — 下标切片，按需物化。

## 快速开始

```bash
pip install persisting
```

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

## 应用场景

| 场景 | 维度 | 主要访问模式 |
|-----|------|------------|
| **KV Cache Offloading** | (session, layer, head, time) | 点查 + 范围扫描 + 预取 |
| **参数服务** | (param_id, shard) | 批量点查 |
| **轨迹存储** | (run_id, time) | 顺序范围扫描 |

## 社区

- [GitHub 仓库](https://github.com/DeepLink-org/Persisting)
- [问题追踪](https://github.com/DeepLink-org/Persisting/issues)
- [讨论区](https://github.com/DeepLink-org/Persisting/discussions)

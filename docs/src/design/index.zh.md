# 设计文档

本节包含 Persisting（参数、KV Cache 与轨迹的持久化存储）的设计文档。

## 架构

```
┌────────────────────────────────────────────────────────┐
│  应用层                                                │
│  kv["s1", 0, 2, 0:512].tensor()                       │
├────────────────────────────────────────────────────────┤
│  访问模式                                              │
│  ┌──────────────┐ ┌──────────────┐ ┌────────────────┐ │
│  │ 多维查找     │ │  流式追加    │ │ 批量点查       │ │
│  │ (KV Cache)   │ │  (轨迹)     │ │ (PS)           │ │
│  └──────────────┘ └──────────────┘ └────────────────┘ │
├────────────────────────────────────────────────────────┤
│  Persisting 核心                                       │
│  ┌────────────┐  ┌────────────┐  ┌──────────────────┐ │
│  │    TTAS    │  │   分层     │  │   放置 / 路由    │ │
│  │ (分层     │  │  GPU/Host  │  │                  │ │
│  │  张量地址)│  │  /SSD      │  │                  │ │
│  └────────────┘  └────────────┘  └──────────────────┘ │
├────────────────────────────────────────────────────────┤
│  存储引擎: Lance                                       │
│  列式格式 · SSD 持久化 · 基线读取路径                   │
└────────────────────────────────────────────────────────┘
```

- **存储引擎（Lance）**：SSD 层文件格式，提供持久化兜底——最差情况从文件读，上层所有优化都是对此的加速。
- **Persisting 核心**：TTAS 寻址、分层内存（GPU ↔ host ↔ SSD）、放置与路由。
- **访问模式**：同一存储引擎上的不同访问模式——流式追加（轨迹）、多维查找（KV Cache）、批量点查（PS）。

## 核心设计

- [分层张量地址空间 (TTAS)](tensor_address_algebra.md) — 寻址模型（与 PGAS 对标）：多维地址、约束代数、lowering 到存储引擎原语。
- [架构设计](architecture.md) — 存储引擎集成、流式追加（Lance 队列后端）。
## 参考与可行性

- [整体架构与核心抽象](architecture_core_abstractions.zh.md) — Store / Namespace / Backend
- [统一存储可行性](unified_storage_feasibility.zh.md) — 参数 / KV Cache / 轨迹三者归一分析
- [LMCache KV Cache 参考](lmcache_kvcache_reference.zh.md) — LMCache 分析，供 KV 实现参考
- [Persisting vs TransferQueue 打分点评](persisting_vs_transferqueue_scoring.md) — Queue 链路与 TransferQueue 的维度打分与场景推荐
- [TransferQueue 与 Pulsing Queue 接口设计对比](transfer_queue_interface_comparison.md) — 接口逐项对照、迁移建议与性能差异分析
- [类似系统参考](similar_systems_reference.md) — 与分布式分层存储相关的系统（LMCache、UMap、CUDA UVM 等）对比与可借鉴点

## 历史 / 内部

### 评审与设计–实现对照

- [TTAS 设计 Review（历史）](tensor_address_algebra_review.md) — 寻址模型初版评审；当前以 [TTAS / tensor_address_algebra](tensor_address_algebra.md) 为准

### 讨论与实现步骤

- [分层存储实现步骤](tiered_storage_implementation_steps.md) — 实现步骤与单测清单（Step 1–11 等）

### 其他项目级文档

- [PERSISTING_REVIEW_AND_ROADMAP](../../PERSISTING_REVIEW_AND_ROADMAP.md) — 项目 review 与路线图
- [PERSISTING_VS_PULSING_REVIEW](../../PERSISTING_VS_PULSING_REVIEW.md) — Persisting 与 Pulsing 对比 review

## 设计原则

1. **Lance 是兜底** — Lance 存储引擎提供 SSD 层的持久化基线。GPU 缓存、host 内存池、跨节点共享都是对"从文件读"这条 baseline 的逐级加速。
2. **一个引擎，多种访问模式** — 同一个 Lance 存储引擎上的不同访问模式（流式追加 / 多维查找 / 批量点查），不是独立的子系统。
3. **TTAS 是内部实现** — 用户看到的是 `kv[key].tensor()`。TTAS 在底层驱动寻址、路由和批量优化。
4. **性能是产品** — Persisting 的价值用 KV Cache P99、GPU 利用率、内存效率来衡量。

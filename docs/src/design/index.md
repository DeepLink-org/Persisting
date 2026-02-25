# Design Documents

This section contains design documents for Persisting — persistent storage for parameters, KV Cache, and trajectories.

## Architecture

```
┌────────────────────────────────────────────────────────┐
│  Application                                           │
│  kv["s1", 0, 2, 0:512].tensor()                       │
├────────────────────────────────────────────────────────┤
│  Access Patterns                                       │
│  ┌──────────────┐ ┌──────────────┐ ┌────────────────┐ │
│  │ Multi-dim    │ │  Streaming   │ │ Batch point    │ │
│  │ lookup (KV)  │ │ append(traj) │ │ query (PS)     │ │
│  └──────────────┘ └──────────────┘ └────────────────┘ │
├────────────────────────────────────────────────────────┤
│  Persisting Core                                       │
│  ┌────────────┐  ┌────────────┐  ┌──────────────────┐ │
│  │    TTAS    │  │  Tiering   │  │   Placement /    │ │
│  │ (Tiered    │  │  GPU/Host  │  │   Route          │ │
│  │  Tensor AS)│  │  /SSD      │  │                  │ │
│  └────────────┘  └────────────┘  └──────────────────┘ │
├────────────────────────────────────────────────────────┤
│  Storage Engine: Lance                                 │
│  columnar format · SSD persist · baseline read path    │
└────────────────────────────────────────────────────────┘
```

- **Storage Engine (Lance)**: SSD 层文件格式，提供持久化兜底——最差情况从文件读，上层所有优化都是对此的加速。
- **Persisting Core**: TTAS（分层张量地址空间）寻址、分层内存（GPU ↔ host ↔ SSD）、放置与路由。
- **Access Patterns**: 同一存储引擎上的不同访问模式——streaming append（轨迹）、multi-dim lookup（KV Cache）、batch point query（PS）。

## Core Design

- [分层张量地址空间 (TTAS)](tensor_address_algebra.md) — The addressing model (counterpart to PGAS): multi-dimensional, tiered addresses, constraint algebra, lowering to storage engine primitives. TTAS vs PGAS 对照见该文档 0.2 节。
- [Architecture](architecture.md) — Storage engine integration, streaming append (Lance queue backend).
- [Distributed Tiered Storage](distributed_tiered_storage.md) — Four-tier storage (GPU / local CPU / remote CPU / SSD), Userfaultfd-driven paging, RDMA/NVLink/PCIe transport.

## Reference & Feasibility

- [Architecture & Core Abstractions](architecture_core_abstractions.zh.md) — Store / Namespace / Backend (中文)
- [Unified Storage Feasibility](unified_storage_feasibility.zh.md) — PS / KV Cache / Trajectory unification analysis (中文)
- [LMCache KV Cache Reference](lmcache_kvcache_reference.zh.md) — LMCache analysis for KV implementation reference (中文)
- [Persisting vs TransferQueue 打分点评](persisting_vs_transferqueue_scoring.md) — Queue 链路与 TransferQueue 的维度打分与场景推荐 (中文)
- [TransferQueue 与 Pulsing Queue 接口设计对比](transfer_queue_interface_comparison.md) — 接口逐项对照、迁移建议与性能差异分析 (中文)
- [类似系统参考](similar_systems_reference.md) — 与分布式分层存储相关的系统（LMCache、UMap、CUDA UVM 等）对比与可借鉴点

## Historical / Internal

### Reviews & design–implementation alignment

- [TTAS Design Review (historical)](tensor_address_algebra_review.md) — Review of addressing model v1; superseded by [TTAS / tensor_address_algebra](tensor_address_algebra.md)

### Discussion & implementation steps

- [Tiered Storage Implementation Steps](tiered_storage_implementation_steps.md) — 分层存储实现步骤与单测清单（Step 1–11 等）

### Other project-level docs

- [PERSISTING_REVIEW_AND_ROADMAP](../../PERSISTING_REVIEW_AND_ROADMAP.md) — 项目 review 与路线图
- [PERSISTING_VS_PULSING_REVIEW](../../PERSISTING_VS_PULSING_REVIEW.md) — Persisting 与 Pulsing 对比 review

## Design Principles

1. **Lance is the floor** — Lance 存储引擎提供 SSD 层的持久化兜底。GPU 缓存、host 内存池、跨节点共享都是对"从文件读"这条 baseline 的逐级加速。
2. **One engine, multiple access patterns** — 同一个 Lance 存储引擎上的不同访问模式（streaming append / multi-dim lookup / batch get），不是独立的子系统。
3. **TTAS is internal** — Users see `kv[key].tensor()`. TTAS (Tiered Tensor Address Space) powers addressing, routing, and batch optimization under the hood.
4. **Performance is the product** — The value of Persisting is measured by KV Cache P99, GPU utilization, and memory efficiency — not by the elegance of the address model.

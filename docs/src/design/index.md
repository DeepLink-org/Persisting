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
│  │    TAA     │  │  Tiering   │  │   Placement /    │ │
│  │ (address   │  │  GPU/Host  │  │   Route          │ │
│  │  algebra)  │  │  /SSD      │  │                  │ │
│  └────────────┘  └────────────┘  └──────────────────┘ │
├────────────────────────────────────────────────────────┤
│  Storage Engine: Lance                                 │
│  columnar format · SSD persist · baseline read path    │
└────────────────────────────────────────────────────────┘
```

- **Storage Engine (Lance)**: SSD 层文件格式，提供持久化兜底——最差情况从文件读，上层所有优化都是对此的加速。
- **Persisting Core**: TAA 寻址、分层内存（GPU ↔ host ↔ SSD）、放置与路由。
- **Access Patterns**: 同一存储引擎上的不同访问模式——streaming append（轨迹）、multi-dim lookup（KV Cache）、batch point query（PS）。

## Core Design

- [Tensor Address Algebra (TAA)](tensor_address_algebra.md) — The addressing model: multi-dimensional addresses, constraint algebra, lowering to storage engine primitives.
- [Architecture](architecture.md) — Storage engine integration, streaming append (Lance queue backend).
- [Distributed Tiered Storage](distributed_tiered_storage.md) — Four-tier storage (GPU / local CPU / remote CPU / SSD), Userfaultfd-driven paging, RDMA/NVLink/PCIe transport.
- [Write-Ahead Log](wal.md) — WAL design (planned).

## Reference & Feasibility

- [Architecture & Core Abstractions](architecture_core_abstractions.zh.md) — Store / Namespace / Backend (中文)
- [Unified Storage Feasibility](unified_storage_feasibility.zh.md) — PS / KV Cache / Trajectory unification analysis (中文)
- [LMCache KV Cache Reference](lmcache_kvcache_reference.zh.md) — LMCache analysis for KV implementation reference (中文)

## Historical / Internal

- [TAA Design Review (historical)](tensor_address_algebra_review.md) — Review of TAA v1; superseded by [TAA v2](tensor_address_algebra.md)
- [Project Review (internal)](project_review_internal.md) — Internal review, not a formal design document

## Design Principles

1. **Lance is the floor** — Lance 存储引擎提供 SSD 层的持久化兜底。GPU 缓存、host 内存池、跨节点共享都是对"从文件读"这条 baseline 的逐级加速。
2. **One engine, multiple access patterns** — 同一个 Lance 存储引擎上的不同访问模式（streaming append / multi-dim lookup / batch get），不是独立的子系统。
3. **TAA is internal** — Users see `kv[key].tensor()`. TAA powers addressing, routing, and batch optimization under the hood.
4. **Performance is the product** — The value of Persisting is measured by KV Cache P99, GPU utilization, and memory efficiency — not by the elegance of the algebra.

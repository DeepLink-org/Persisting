# 设计文档

Persisting 的设计文档，涵盖寻址模型、分层存储、队列持久化、CLI 命令设计。

---

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

- **存储引擎（Lance）**：SSD 层文件格式，提供持久化兜底。上层所有优化都是对「从文件读」这条 baseline 的逐级加速。
- **Persisting 核心**：TTAS 寻址、分层内存（GPU ↔ host ↔ SSD）、放置与路由。
- **访问模式**：同一存储引擎上的不同访问模式——流式追加（轨迹）、多维查找（KV Cache）、批量点查（PS）。

---

## 核心设计

- [分层张量地址空间 (TTAS)](tensor_address_algebra.md) — 寻址模型（与 PGAS 对标）：多维地址、约束代数、规范化与 lowering
- [架构设计](architecture.zh.md) — 队列持久化架构：LanceBackend、并发模型、与 Pulsing 的配合
- [分布式分层存储](distributed_tiered_storage.md) — 四级存储（GPU / CPU / Remote / SSD）、Block 模型、mmap + UFFD 虚拟地址映射

## CLI

- [CLI 整体架构](cli_architecture.zh.md) — 引擎发现、惰性加载、异步 Job ABI、wire 格式、版本常量
- [`persisting search` 命令设计](cli_search_command.zh.md) — 子命令树、参数、交互设计
- [`persisting trajectory` 命令设计](cli_trajectory_command.zh.md) — 子命令树、参数、交互设计

## 参考与分析

- [LMCache KV Cache 参考](lmcache_kvcache_reference.zh.md) — LMCache 分析，供 KV 实现参考
- [Persisting vs TransferQueue 打分](persisting_vs_transferqueue_scoring.md) — Queue 链路与 TransferQueue 的维度对比
- [TransferQueue 与 Pulsing Queue 接口对比](transfer_queue_interface_comparison.md) — 接口逐项对照与迁移建议
- [类似系统参考](similar_systems_reference.md) — LMCache、UMap、CUDA UVM 等系统对比

## 实现追踪

- [分层存储实现步骤](../dev/tiered_storage_implementation_steps.md) — Step 1–11 实现记录与单测清单

## 设计原则

1. **Lance 是兜底** — Lance 提供 SSD 层的持久化基线。GPU 缓存、host 内存池、跨节点共享都是对「从文件读」的逐级加速。
2. **一个引擎，多种访问模式** — 同一个 Lance 存储引擎上承载流式追加、多维查找、批量点查，不是独立子系统。
3. **TTAS 是内部实现** — 用户看到的是 `kv[key].tensor()`。TTAS 在底层驱动寻址、路由和批量优化。
4. **性能是产品** — Persisting 的价值用 KV Cache P99、GPU 利用率、内存效率来衡量。

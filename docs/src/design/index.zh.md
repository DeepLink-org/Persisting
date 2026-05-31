# 设计文档

Persisting 的设计文档：寻址与分层存储（演进中）、队列持久化、**轨迹捕获与存储**（已落地）、CLI 能力模型。

---

## 架构总览

```
┌─────────────────────────────────────────────────────────────────┐
│  应用层                                                         │
│  Tensor 式 KV 访问  ·  Agent 轨迹 capture  ·  文档检索          │
├─────────────────────────────────────────────────────────────────┤
│  访问模式                                                       │
│  ┌──────────────┐ ┌──────────────┐ ┌────────────┐ ┌──────────┐ │
│  │ 多维查找     │ │  流式追加    │ │ 批量点查   │ │ Agent    │ │
│  │ (KV Cache)   │ │  (轨迹)     │ │ (PS)       │ │ Search   │ │
│  └──────────────┘ └──────────────┘ └────────────┘ └──────────┘ │
├─────────────────────────────────────────────────────────────────┤
│  Persisting 核心 / 引擎                                         │
│  ┌────────────┐  ┌────────────┐  ┌──────────────────────────┐  │
│  │    TTAS    │  │   分层     │  │ 轨迹：Vortex canonical + MD 物化 │  │
│  │ (规划)     │  │  GPU/Host  │  │ Vortex 列存 │ 会话 Markdown│  │
│  │            │  │  /SSD      │  │                           │  │
│  └────────────┘  └────────────┘  └──────────────────────────┘  │
├─────────────────────────────────────────────────────────────────┤
│  存储引擎: Lance（列式 · SSD 持久化 · 多种能力的共同底座）       │
└─────────────────────────────────────────────────────────────────┘
```

- **Lance**：SSD 持久化基线；**Search 索引**、**队列**落盘建立在此之上。
- **轨迹（已实现）**：内嵌 LLM 代理捕获 → Vortex raw event log（`events.vortex`）；TLV Markdown 为物化人读视图。
- **TTAS 与分层内存（演进中）**：多维寻址、GPU ↔ host ↔ SSD 放置。
- **一种底座，多种访问模式**：流式追加、检索、点查、队列共享 Lance 生态。

---

## 核心设计

| 主题 | 文档 | 状态 |
|------|------|------|
| 轨迹存储模型 | [trajectory_storage.zh.md](trajectory_storage.zh.md) | 已实现 |
| 轨迹 Markdown 格式 | [trajectory_tlv_format.zh.md](trajectory_tlv_format.zh.md) | 已实现 |
| Capture 架构设计（对外） | [capture_design.zh.md](capture_design.zh.md) | 已实现 |
| 队列持久化架构 | [architecture.zh.md](architecture.zh.md) | 已实现 |
| TTAS 寻址模型 | [tensor_address_algebra.md](tensor_address_algebra.md) | 设计 |
| 分布式分层存储 | [distributed_tiered_storage.md](distributed_tiered_storage.md) | 规划 |

## CLI 能力

| 命令族 | 文档 | 职责 |
|--------|------|------|
| 整体架构 | [cli_architecture.zh.md](cli_architecture.zh.md) | 瘦 CLI + 动态引擎、异步任务、版本化协议 |
| **`traj`（轨迹）** | [cli_trajectory_command.zh.md](cli_trajectory_command.zh.md) | 统一入口：capture / proxy / import / stats / replay / materialize / … |
| Capture 子命令 | [cli_capture_command.zh.md](cli_capture_command.zh.md) | `traj capture`、`traj proxy`、`traj import` 设计说明 |
| Search | [cli_search_command.zh.md](cli_search_command.zh.md) | 导入、索引、检索 |

## 参考与分析

- [LMCache KV Cache 参考](lmcache_kvcache_reference.zh.md)
- [Persisting vs TransferQueue 打分](persisting_vs_transferqueue_scoring.md)
- [TransferQueue 与 Pulsing Queue 接口对比](transfer_queue_interface_comparison.md)
- [类似系统参考](similar_systems_reference.md)

## 设计原则

1. **Lance 是兜底** — 上层缓存与加速都建立在「从文件读」这一基线之上。
2. **一种底座，多种模式** — 轨迹、Search、KV、队列不是彼此孤立的子系统。
3. **轨迹两层视图** — Vortex canonical（全量，`events.vortex`）；Markdown 由 CaptureEngine live upsert / `traj import` / `traj materialize` 派生；`traj capture` 下主/子 md 分文件存放。
4. **Capture 自给自足** — 内嵌代理即可完整捕获 LLM 流量；IDE 日志 import 为补充。
5. **TTAS 对内** — 对外保持简洁的数据访问模型。
6. **性能是产品** — P99 延迟、GPU 利用率、capture 实时性是核心指标。

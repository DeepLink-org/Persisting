# 设计文档

Persisting 的架构、寻址模型与存储设计。

---

## 架构

| 文档 | 描述 |
|------|------|
| [Architecture](architecture.md) | 队列持久化与系统概览（英文） |
| [架构设计](architecture.zh.md) | 队列持久化、并发模型、Tensor Memory |
| [CLI 整体架构](cli.md) | 瘦 CLI + 动态引擎加载 |

## 分层存储

| 文档 | 描述 |
|------|------|
| [TTAS 寻址模型](tensor-address-space.md) | 分层张量地址空间——形式化寻址模型 |
| [分布式分层存储](distributed-tiered-storage.md) | Block 模型、虚拟地址映射、Pulsing 集成（英文） |
| [Block Store 内部设计](block-store.md) | Block Table、缺页处理、事件循环 |

## Capture 与轨迹

| 文档 | 描述 |
|------|------|
| [Capture 架构设计](capture.md) | LLM 代理、事件模型、双层存储 |
| [轨迹存储模型](trajectory.md) | Lance canonical + Markdown 物化 |
| [轨迹 Markdown 格式](trajectory-format.md) | TLV 块模型、frontmatter、live upsert |
| [Traj CLI](cli-traj.md) | `persisting traj` 命令参考 |
| [Capture CLI](cli-capture.md) | `traj capture` / `traj proxy` 子命令 |

## Compute

| 文档 | 描述 |
|------|------|
| [Compute 架构](compute.md) | Driver/Worker、调度、sink、checkpoint |

## Search

| 文档 | 描述 |
|------|------|
| [Search CLI](cli-search.md) | `persisting search` 命令设计 |

---

## 参考与分析

| 文档 | 描述 |
|------|------|
| [类似系统参考](references/similar-systems.md) | LMCache、vLLM、UMap、CUDA VMM |
| [vs TransferQueue](references/transfer-queue-comparison.md) | 打分与迁移分析 |
| [TransferQueue 接口对比](references/transfer-queue-interface.md) | API 对照表 |
| [LMCache KV Cache 参考](references/lmcache.md) | KV Cache 实现参考 |

---

## 实现追踪

| 文档 | 描述 |
|------|------|
| [分层存储实现步骤](../dev/tiered-storage-steps.md) | 分步实现与测试清单 |

---

## 设计原则

1. **Lance 是兜底** — 上层缓存与加速都建立在「从文件读」这一基线之上。
2. **一种底座，多种模式** — 轨迹、Search、KV、队列共享 Lance 生态。
3. **轨迹两层视图** — Lance canonical（`events.lance/`）+ Markdown 物化；`-f md` live upsert，`-f lance` 只写 Lance（md 经 materialize）。
4. **Capture 自给自足** — 内嵌代理即可完整捕获 LLM 流量。
5. **TTAS 对内** — 用户看到 `kv[key].tensor()`，而非原始代数。
6. **性能是产品** — P99 延迟、GPU 利用率、capture 实时性是核心指标。

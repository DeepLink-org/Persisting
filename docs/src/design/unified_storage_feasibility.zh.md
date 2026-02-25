# 参数、KV Cache、轨迹存储三者归一：可行性分析

本文档分析将 **模型参数（Parameters）**、**KV Cache**、**轨迹（Trajectories）** 三种存储需求归一为同一套抽象与实现的可行性，并给出推荐形态与落地路径。

---

## 1. 三种存储的语义对比

| 维度 | 参数 (Parameters) | KV Cache | 轨迹 (Trajectories) |
|------|------------------|----------|---------------------|
| **典型 Key** | 模型版本 + 分片/层，如 `model/v1/shard_0`、`layer_3` | 内容哈希 + 模型，如 CacheEngineKey 或 `kvcache/model/chunk_<hash>` | 会话/步/ID，如 `traj/session_1/step_5` 或 append 序 offset |
| **Value 形态** | 大块张量（权重），shape/dtype 固定或按层 | 固定格式张量块（MemoryObj），按 chunk | 结构化记录（prompt, response, reward, meta） |
| **写入模式** | 低频、大批量（checkpoint 全量/增量） | 中高频、按 key 写、可覆盖 | 追加为主、流式写入 |
| **读取模式** | 按前缀/范围批量加载（如按层、按 shard） | 按 key 随机读、按前缀最长匹配（lookup） | 按 offset 顺序读、按 key/范围查、采样 |
| **一致性** | 版本化（v1/v2），需要原子快照或版本指针 | 单 key 原子，可选 TTL | 追加序、可容忍最终一致 |
| **生命周期** | 版本保留与清理、checkpoint 轮转 | 驱逐、pin/unpin、TTL | 保留窗口、压缩、按策略删 |
| **性能侧重** | 大块顺序 I/O、带宽 | 延迟、随机读、前缀扫描 | 吞吐、追加、范围/采样 |

结论：三者都是「**键 + 值**」的存储，但 key 的生成方式、value 的物理形态和访问模式差异较大；归一的关键是找到**公共抽象**并允许**上层按类型做适配**。

---

## 2. 公共抽象：可行性的基础

三者都可以映射到同一底层模型：

- **Key**：统一为「命名空间 + 键」的字符串（或定长二进制），例如：
  - 参数：`params/<model>/<version>/<shard_or_layer>`
  - KV Cache：`kvcache/<model>/<chunk_hash>` 或 CacheEngineKey.to_string()
  - 轨迹：`traj/<session>/<step>` 或 `traj/by_offset/<topic>/<offset>`
- **Value**：统一为 **opaque 二进制 + 元数据**：
  - 元数据：`type`（tensor | record | blob）、可选 `shape`/`dtype`/`format`（tensor）、`schema`（record）、长度等。
  - 载荷：序列化后的 tensor 字节、或 record 的列式/行式字节、或原始 blob。
- **操作**：公共集为 **put(key, value, meta)**、**get(key)**、**exists(key)**、**delete(key)**、**list_prefix(prefix)** / **scan(prefix)**；轨迹的「追加」可建模为 put(生成 key) 或 append(namespace, value)。

因此，**在抽象层做「三者归一」是可行的**：用一个通用的 **BlobStore / Key-Value Store** 作为核心，三种存储都视为不同 **namespace**（或 prefix）下的键值数据，差异通过 **序列化格式与上层 API** 吸收。

---

## 3. 归一的主要挑战

| 挑战 | 说明 | 应对思路 |
|------|------|----------|
| **Value 格式不一** | 参数/KV 是 tensor；轨迹是 record（dict/列） | 核心层只存 bytes + meta；上层三种 Adapter 各自做序列化（tensor↔bytes、record↔bytes） |
| **访问模式差异大** | 参数大块顺序；KV 随机 + 前缀；轨迹追加 + 范围 | 统一引擎提供 get/put/prefix_scan；实现层可按 namespace 选不同布局（如参数大块文件、KV 索引表、轨迹追加表） |
| **生命周期策略不同** | 参数版本；KV 驱逐/TTL；轨迹保留窗口 | 在 Adapter 或 Policy 层实现，核心只提供 delete/list；或核心支持 TTL/version 等通用扩展 |
| **性能调优冲突** | 同一引擎若为「通用」设计，可能对某一类不够优 | 采用「统一 API + 可选 per-namespace 后端或格式」：例如 params 用大块 layout，kvcache 用索引+小块，traj 用追加表 |

综上：**语义归一可行，实现上可分层**——核心是统一 key-value 抽象与 namespace，上层用三种 **Adapter**（ParameterStore、KVCacheStore、TrajectoryStore）暴露各自语义，底层可共用同一存储引擎（如 Lance），也可按 namespace 选择不同存储布局或后端。

---

## 4. 推荐形态：统一核心 + 三类 Adapter

### 4.1 分层结构

```
┌─────────────────────────────────────────────────────────────────┐
│  Adapter 层（按使用场景的 API）                                    │
│  ParameterStore  │  KVCacheStore  │  TrajectoryStore             │
│  (版本/分片/加载)  │  (key/prefix/TTL) │  (append/采样/范围)         │
└────────────────────────────┬────────────────────────────────────┘
                             │ 均使用
┌────────────────────────────┴────────────────────────────────────┐
│  Unified Store 核心                                               │
│  • put(ns, key, payload: bytes, meta: dict)                     │
│  • get(ns, key) → (payload, meta)                               │
│  • exists(ns, key) / delete(ns, key)                            │
│  • list_prefix(ns, prefix) / scan(ns, prefix, limit)             │
│  • 可选：append(ns, payload, meta) → generated_key               │
└────────────────────────────┬────────────────────────────────────┘
                             │
┌────────────────────────────┴────────────────────────────────────┐
│  存储引擎层（可插拔）                                              │
│  LanceUnifiedBackend  │  或 按 ns 路由到 Queue/KV/Params 后端     │
└─────────────────────────────────────────────────────────────────┘
```

- **核心**：只认 `(namespace, key, payload, meta)`，不关心 payload 是 tensor 还是 record；list_prefix/scan 提供「按前缀枚举」与范围扫描。
- **Adapter**：
  - **ParameterStore**：key = params/...，value = 序列化后的 tensor；提供 load_checkpoint(version)、save_shard、按层/按 shard 加载。
  - **KVCacheStore**：key = kvcache/...（或 CacheEngineKey 串），value = MemoryObj 序列化；提供 contains、get、put、remove、按前缀最长匹配（可在此层实现）。
  - **TrajectoryStore**：key = traj/... 或由 append 生成；value = 序列化 record；提供 append、get_by_offset、get_range、sample（可结合 Sampler）。

这样，**参数、KV Cache、轨迹在「存储核心」中归一**，在「使用方式」上仍各有一套清晰 API，便于演进和单独调优。

### 4.2 用 Lance 作为统一引擎的可行做法

- **方案 A（单数据集多列）**：一个 Lance 表，列例如 `ns`（string）、`key`（string）、`payload`（binary）、`meta`（json）。按 `ns` 过滤即得到三种数据；索引可建在 `(ns, key)` 或仅 `key`（若 key 已含 ns 前缀）。适合数据量都不特别大的情况。
- **方案 B（按 namespace 分表/分目录）**：同一 `storage_path` 下子目录或子 dataset，如 `./data/params/`、`./data/kvcache/`、`./data/traj/`，每个下面各自 Lance 表结构：
  - params：key + payload（大块）+ meta（shape/dtype/version）
  - kvcache：key + payload + meta（format/chunk_hash）
  - traj：可与现有队列一致（列式 record），或 key + payload + meta  
  这样三种仍共用「Lance + 同一 path 根」，但物理布局按类型优化（例如参数大块、KV 索引友好、轨迹追加友好）。
- **方案 C（混合）**：核心 API 统一，底层根据 `ns` 路由到不同实现——例如 `params` 走大块/对象存储风格，`kvcache` 走索引+块，`traj` 走现有 Lance 队列表或追加表。对外仍是「一个 Unified Store」，对内可逐步优化每类。

推荐先按 **方案 B** 落地：**统一 API + 按 namespace 分目录/分表**，这样与现有「队列 Lance 后端」和「KV 存储」的路径约定（见 [lmcache_kvcache_reference](../design/lmcache_kvcache_reference.zh.md) 第 10 节）一致，且便于为三类数据选择不同 Lance 布局或压缩策略。

---

## 5. 与现有组件的衔接

- **现有队列（LanceBackend）**：可视为 TrajectoryStore 的一种，或作为「轨迹」namespace 的底层实现——即轨迹的 append/get 由现有 Queue 语义承载，Unified Store 的 `traj` 适配器调用现有队列后端或复用其表结构。
- **未来 KV 后端（如 LMCache 插件）**：KVCacheStore 适配器产出的 key/value 格式与 LMCache 的 CacheEngineKey/MemoryObj 对齐，底层可写 Unified Store 的 `kvcache` 表，或直接实现 LMCache StorageBackendInterface 时内部调用 Unified Store。
- **参数**：目前 Persisting 未实现；参数可作为 Unified Store 的 `params` namespace 首次落地，版本与分片由 ParameterStore 适配器管理，底层用 Lance 或大块文件。

这样，**三者归一** 不会推翻现有队列与 KV 设计，而是加上一层统一核心与命名空间约定，使参数、KV、轨迹共享同一套存储根、配置与可选的统一运维视图。

---

## 6. 实施顺序建议

1. **定义 Unified Store 核心接口**（put/get/exists/delete/list_prefix，及可选 append），与 `(ns, key, payload, meta)` 的数据模型；在文档中固定 namespace 约定（`params/`、`kvcache/`、`traj/`）。
2. **实现单一存储引擎**：例如 LanceUnifiedBackend，按 ns 分目录或分表，先支持所有 namespace 用同一表结构（方案 A），便于验证。
3. **实现 TrajectoryStore 适配器**：基于现有队列或新追加表，暴露 append、get_range、sample；与现有 Sampler 结合。
4. **实现 KVCacheStore 适配器**：key 用 CacheEngineKey 或等价字符串，value 用 MemoryObj 序列化；实现 contains/get/put/remove 及可选前缀扫描；可与 LMCache 后端共用此层。
5. **实现 ParameterStore 适配器**：版本与 shard 命名、save/load checkpoint、按层或按 shard 加载。
6. **按需拆分或优化**：若某 namespace 需要不同布局（如 params 大块、KV 强索引），再在引擎层按 ns 分支或路由到专门实现。

---

## 7. 结论

- **可行性**：**可行**。参数、KV Cache、轨迹在「键值 + 命名空间」层面可归一为同一套核心抽象（Unified Store），用统一的 key 空间与 bytes + meta 的 value 模型承载三类数据。
- **推荐做法**：**统一核心 API + 按 namespace 分目录/分表 + 三类 Adapter**；存储引擎可先由 Lance 统一实现，再按 namespace 做布局或后端上的分化。
- **收益**：一套配置与存储根、一致的生命周期与运维视角、代码复用（序列化、索引、策略可逐步共享），同时保留三种场景的独立 API 与调优空间。

若需要，可在下一步把「Unified Store 核心接口」写成具体 Python 协议或抽象类，并给出 `params/`、`kvcache/`、`traj/` 的 key 命名规范与 meta 字段约定，便于实现与跨组件对接。

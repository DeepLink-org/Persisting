# LMCache 分析：Persisting KV Cache 实现参考

本文档分析 [LMCache](https://github.com/LMCache/LMCache) 的架构与接口，作为 Persisting 实现 KV Cache 存储的参考。LMCache 面向 LLM 推理的 KV Cache 复用（降低 TTFT、提升吞吐），与 vLLM/SGLang 等集成，支持 CPU/磁盘/NIXL/P2P 等多级存储。

---

## 1. LMCache 核心定位

- **目标**：在数据中心内复用任意可复用文本的 KV Cache（不限于前缀），减少 GPU 计算与首 token 延迟。
- **集成**：vLLM、SGLang、Production Stack 等；存储侧有 Redis、Weka、PliOps、GDS、NIXL 等。
- **能力**：CPU offload、P2P 共享、Disaggregated prefill、多级存储（GPU → CPU → Disk → 远端）。

与 Persisting 的契合点：Persisting 的 Roadmap 包含「Tensor 级存储、基于前缀的 KV Cache 查找、多级缓存」，可借鉴 LMCache 的 Key 设计、存储后端抽象、内存对象与序列化。

---

## 2. 关键抽象概览

### 2.1 键：CacheEngineKey

**位置**：`lmcache/utils.py`（`CacheEngineKey` dataclass）

KV 条目的唯一标识，用于「按内容（token 序列）可复现」的缓存查找：

| 字段 | 含义 |
|------|------|
| `model_name` | 模型名，隔离不同模型 cache |
| `world_size` | 分布式时 TP 规模（可置 1 做 world-size 无关） |
| `worker_id` | 当前 worker id |
| `chunk_hash` | **内容哈希**：由 token 序列（按 chunk）经 `TokenDatabase` 计算得到 |
| `dtype` | KV 张量 dtype（如 float16） |
| `request_configs` | 可选请求级配置（含 `lmcache.tag.*` 等） |

- **序列化**：`to_string()` / `from_string()`，格式为 `model_name@world_size@worker_id@chunk_hash(hex)@dtype_str[@tag%value...]`，便于网络/磁盘索引。
- **前缀语义**：lookup 时按 chunk 顺序查 key；`batched_contains` 在第一个 miss 处停止，用于「最长前缀命中」。
- **扩展**：`LayerCacheEngineKey` 增加 `layer_id`，用于按层存储/加载。

**对 Persisting 的启示**：KV Cache 的 key 需要包含「模型 + 内容哈希 + 可选 layer/chunk 信息」，并支持可序列化字符串，便于跨进程/节点与存储后端索引。

### 2.2 值：MemoryObj 与 MemoryFormat

**位置**：`lmcache/v1/memory_management.py`

- **MemoryObj**：对「一块 KV 数据」的抽象，持有 `MemoryObjMetadata`（shape、dtype、address、phy_size、ref_count、pin_count、fmt 等），子类提供具体存储（如 GPU/CPU tensor、buffer）。
- **MemoryFormat**：物理布局枚举，如 `KV_2LTD`（`[2, num_layers, num_tokens, hidden_dim]`）、`KV_T2D`、`BINARY`（压缩）、`KV_MLA_FMT` 等，用于分配与序列化时一致解释。

**对 Persisting 的启示**：Persisting 若做 KV 存储，需要定义「逻辑 key → 物理块」的映射，以及块格式（shape/dtype/format）；可先支持 1–2 种常用格式（如 2LTD），再扩展。

### 2.3 TokenDatabase：从 Token 到 Key

**位置**：`lmcache/v1/token_database.py`

- 将输入 **token 序列** 分 chunk，对每个 chunk 计算 **chunk_hash**（可带前缀哈希链），再组装为 `CacheEngineKey`。
- `process_tokens()` 返回 `(start, end, key)` 的迭代，供上层按 chunk 做 lookup/put。
- 支持 `chunk_size`、`save_unfull_chunk`、`save_only_first_rank`（MLA 时 world_size 置 1）等策略。

**对 Persisting 的启示**：若 Persisting 只做「存储层」，key 的生成可由调用方（或 Pulsing 侧适配层）完成；Persisting 只需约定 key 的字符串/二进制格式及「前缀可比较」的约定（若要做前缀匹配）。

---

## 3. 存储后端接口（StorageBackendInterface）

**位置**：`lmcache/v1/storage_backend/abstract_backend.py`

所有存储后端的统一抽象（CPU、Disk、Remote、P2P、NIXL、GDS 等）都实现该接口，核心方法如下。

### 3.1 读写与存在性

| 方法 | 说明 |
|------|------|
| `contains(key, pin=False)` | 是否存在该 key；可选 pin 防驱逐 |
| `exists_in_put_tasks(key)` | 是否在「正在写入」集合中（避免读到未完成写） |
| `batched_submit_put_task(keys, objs, ...)` | 批量异步写入 MemoryObj；可选 `on_complete_callback` 按 key 回调 |
| `get_blocking(key)` | 同步取回 `MemoryObj \| None` |
| `get_non_blocking(key)` | 异步取回 `Future` |
| `batched_get_blocking(keys)` | 批量同步 get（默认逐 key 调用） |
| `batched_async_contains(lookup_id, keys, pin)` | 批量 contains，返回命中数量 |
| `batched_get_non_blocking(lookup_id, keys)` | 批量异步 get，用于 prefetch 路径 |

### 3.2 生命周期与策略

| 方法 | 说明 |
|------|------|
| `pin(key)` / `unpin(key)` | 钉住/放开，避免被驱逐 |
| `remove(key, force)` | 删除条目 |
| `batched_remove(keys)` | 批量删除 |
| `get_allocator_backend()` | 返回本后端用于 get 时分配目标缓冲的 AllocatorBackend |

### 3.3 AllocatorBackendInterface（扩展）

在 StorageBackend 基础上增加「在本后端分配内存」的能力，用于 CPU/本地缓存层：

- `allocate(shapes, dtypes, fmt, eviction, busy_loop)` / `batched_allocate(...)`
- `initialize_allocator(config, metadata)` / `get_memory_allocator()`
- `calculate_chunk_budget()`：可分配块数/容量预算

**对 Persisting 的启示**：Persisting 的 KV 后端可先实现「纯存储」版（只做 put/get/contains/remove），不实现 Allocator；接口上可对齐 `contains`、`get`、`put`（或 batched）、`remove`、可选 `pin/unpin`，便于后续与 LMCache 或 vLLM 侧逻辑对接。

---

## 4. StorageManager：多后端路由与调度

**位置**：`lmcache/v1/storage_backend/storage_manager.py`

- 持有多个 **StorageBackend** 实例（如 LocalCPUBackend、LocalDiskBackend、RemoteBackend），按配置或 `search_range` 选择后端。
- **put**：可写多副本（如先写 CPU 再异步写 Disk/Remote）；通过 `allocate_and_copy_objects` 在目标后端分配并拷贝。
- **get**：按后端优先级 lookup，命中则从该后端 `get_blocking`/`get_non_blocking`；支持 **async_lookup_and_prefetch**（按 key 列表批量 prefetch，并上报 scheduler）。
- **前缀语义**：`batched_contains` 按 key 顺序在多个后端查，在第一个 miss 处停止，得到「最长前缀命中」；`get_block_mapping` 按 prefix 将 chunk 映射到不同后端。

**对 Persisting 的启示**：Persisting 若支持多级（如「内存 + Lance/磁盘」），可引入一层「KV 存储管理器」，负责后端选择、批量 put/get 和简单的前缀命中逻辑；与现有 Queue 的 StorageManager 可分离，仅共享「后端注册/发现」思路。

---

## 5. 具体后端实现要点

### 5.1 LocalCPUBackend（内存）

- 使用 `AllocatorBackendInterface`，在 CPU 上分配并保存 `MemoryObj`，dict 存 `key -> MemoryObj`。
- 支持 eviction、pin、LRU 等策略的底层。

### 5.2 LocalDiskBackend（本地磁盘）

- 使用 **cache_policy**（如 LRU）维护内存中的「key → 磁盘路径/元数据」映射；实际张量落盘。
- 序列化：通过 naive_serde / cachegen 等将 MemoryObj 编码后写入文件或块设备。
- 与 Persisting 最相关：Persisting 的 KV 持久化可参考「key 索引 + 块/文件存储」的模式，底层可用 Lance 列存或自定义块格式。

### 5.3 RustRawBlockBackend（块设备 / 高性能）

- 固定 slot、固定大小块，通过 header（magic + chunk_hash + payload_len）定位；支持 LRU、pin、异步写。
- **Header 格式**：`magic(8) + chunk_hash(8) + payload_len(8) + padding`，便于 Persisting 做「小元数据 + 大 payload」的块存储。

### 5.4 RemoteBackend（远端服务）

- 通过 RPC/连接将 put/get 转发到远端；`get` 时在本地用 `local_cpu_backend` 分配缓冲区再拉数据。
- Persisting 若做「远端 KV 服务」，可只实现类似 Remote 的客户端，服务端由 Persisting 或 Pulsing 提供。

### 5.5 P2P Backend

- 只读：`contains` 通过 controller 查 peer，`get` 从 peer 拉数据；不实现 put。
- 说明「存储层」与「发现/路由层」可分离：Persisting 可只做存储，发现与路由由 Pulsing 或上层做。

---

## 6. 配置与分层（Config / Tiers）

**位置**：`lmcache/v1/config.py`

- **local_cpu** / **max_local_cpu_size**：本地 CPU 缓存开关与容量。
- **local_disk** / **max_local_disk_size**：本地磁盘路径与容量。
- **remote_url** / **remote_serde**：远端存储 URL 与序列化方式。
- **chunk_size**：与 TokenDatabase 一致，影响 key 粒度。
- **cache_policy**：本地磁盘的驱逐策略（如 LRU）。

Persisting 的 KV 配置可类似：每层（内存/磁盘/远端）的开关、容量、路径、序列化格式；与现有 `backend_options` 风格统一。

---

## 7. 序列化与压缩

- **naive_serde**：简单张量序列化，用于远程或磁盘。
- **cachegen_encoder/decoder**：带量化（按层 bins）的 KV 压缩，减少带宽与存储。
- **RustRawBlockBackend**：固定块 + header，无压缩或可插压缩。

Persisting 若先做「无损」KV 存储，可只做类似 naive 的 tensor 序列化；后续再加可选压缩（如按层量化），接口上可与 LMCache 的 serde 名字或格式兼容以便对接。

---

## 8. 对 Persisting 的实现建议（简要）

1. **Key 设计**  
   - 定义 KV key 的字段（model、content_hash、layer/chunk、dtype 等）及字符串/二进制格式，支持 `from_string`/`to_string`，便于跨进程与存储索引。  
   - 若需「前缀匹配」，约定 key 的排序或前缀规则（如按 chunk 顺序、字典序）。

2. **存储后端协议**  
   - 定义最小 KV 后端接口：`contains(key)`、`get(key)`、`put(key, value)`、`remove(key)`，value 为「tensor 或 buffer + 元数据」。  
   - 可选：`pin`/`unpin`、`batched_get`/`batched_put`，与 LMCache 接口对齐便于适配。

3. **值类型**  
   - 先支持「单块」KV（一个 key 对应一块连续 tensor），元数据含 shape、dtype、format；后续再考虑多块或分层（layer-wise）与 LMCache LayerCacheEngineKey 对齐。

4. **持久化格式**  
   - 参考 LocalDiskBackend + RustRawBlock：索引（key → 文件/块偏移）+ 块存储（header + payload）；或用 Lance 存「key 列 + 二进制 payload 列」做列式存储。  
   - 若用 Lance：key 用字符串列，value 用 binary/large_binary，便于按 key 过滤与扫描。

5. **多级与路由**  
   - 先实现单后端（如「内存 + Lance」一体或纯 Lance）；再引入 KVStorageManager 做多后端路由、容量与驱逐策略，与现有 Queue StorageManager 解耦。

6. **与 Pulsing 的集成**  
   - KV 读写可封装为 Pulsing Actor（如 KVCacheStorageActor），内部使用 Persisting 的 KV 后端；key 生成与 token 逻辑可放在调用方或单独服务，Persisting 只认「最终 key + value」。

7. **测试与兼容**  
   - 若有 vLLM/LMCache 集成需求，可写一层薄适配：将 LMCache 的 `CacheEngineKey`/`MemoryObj` 映射为 Persisting 的 key/value，调用 Persisting 的 get/put，便于对比测试与逐步迁移。

---

## 9. 建议：Persisting 先作为 LMCache 后端构建能力

**结论：是。** 先把 Persisting 实现为 LMCache 的一个 **Storage Plugin 后端**，再在 Pulsing 生态里提供独立 KV 能力，是更稳妥的路径。

### 9.1 为什么先做 LMCache 后端

1. **接口即规范**：直接实现 `StorageBackendInterface` / `StoragePluginInterface`，key（CacheEngineKey）、value（MemoryObj）、contains/put/get/remove 等语义一次性对齐业界用法，避免自己发明一套再改。
2. **真实场景验证**：接入 LMCache 后可在 vLLM/SGLang 真实推理链路里跑，持久化、序列化、多级路由、前缀语义都能被真实负载检验。
3. **生态复用**：用户若已在用 LMCache，只需在配置里加 Persisting 插件即可试用；Persisting 的 Lance/磁盘实现也能被 LMCache 社区看到和反馈。
4. **能力沉淀**：在实现 LMCache 后端时沉淀的「key 索引 + 块/列存、MemoryObj 序列化、与 local_cpu_backend 协作」等，可直接复用到 Persisting 独立 KV API 或 Pulsing 侧 KVCacheStorageActor。

### 9.2 LMCache 插件接入方式

LMCache 通过配置动态加载插件，无需改 LMCache 主仓：

- 在 `config.storage_plugins` 里加入插件名（如 `PersistingBackend`）。
- 在 `config.extra_config` 里为该插件指定：
  - `storage_plugin.PersistingBackend.module_path`：例如 `persisting.lmcache_backend`
  - `storage_plugin.PersistingBackend.class_name`：例如 `PersistingKVBackend`
- 插件类需实现 **StoragePluginInterface**，构造函数签名为：
  `(config, dst_device, metadata, local_cpu_backend, loop)`，由 LMCache 在启动时注入。

这样用户只需安装 `persisting[lmcache]`（或可选依赖 lmcache），在 vLLM/LMCache 的 YAML 里加上上述配置即可启用 Persisting 作为一层存储。

### 9.3 Persisting 侧需实现的最小集合

| 能力 | 说明 |
|------|------|
| **类** | 实现 `StoragePluginInterface`（或仅 `StorageBackendInterface`），建议类名如 `PersistingKVBackend`，放在 `persisting.lmcache_backend` 模块。 |
| **contains(key, pin)** | 查本地索引（如 Lance 表或内存 dict）是否存在该 key；pin 可先 no-op 或简单标记。 |
| **exists_in_put_tasks(key)** | 维护「正在写入」集合，put 未完成前返回 True。 |
| **batched_submit_put_task(keys, objs, on_complete_callback)** | 将 MemoryObj 序列化后写入 Persisting 存储（如 Lance 或块文件），写完后按 key 调用 on_complete_callback。 |
| **get_blocking(key)** | 用 `local_cpu_backend.get_allocator_backend().allocate(...)` 分配目标 MemoryObj，从 Persisting 读出并反序列化到该对象，返回；若不存在返回 None。 |
| **get_allocator_backend()** | 返回 LMCache 注入的 `local_cpu_backend`，供 StorageManager 在 get 时分配目标缓冲。 |
| **pin / unpin** | 可先 no-op 或简单集合标记，不做驱逐时忽略。 |
| **remove(key)** | 从索引与存储中删除该 key。 |
| **batched_contains / batched_get_non_blocking** | 按接口实现，可先基于单 key 循环，再优化为批量。 |
| **close()** | 释放资源、关闭存储连接。 |

序列化：MemoryObj 的 shape/dtype/format 与 LMCache 的 naive_serde 或现有格式对齐即可；Persisting 只需「key 的字符串 + 二进制 payload」落盘（如 Lance 一列 key、一列 bytes）。

### 9.4 实现顺序建议

1. **Phase 1**：在 Persisting 仓库内新增 `persisting/lmcache_backend.py`，实现上述最小接口，存储层用现有 Lance 或单文件块存储（key → 文件偏移 + 长度），MemoryObj 用 LMCache 已有 serde 或简单 tensor 序列化。
2. **Phase 2**：在 LMCache 配置中启用该插件，与 local_cpu / local_disk 等一起跑 vLLM 用例，验证正确性与性能。
3. **Phase 3**：根据需求加能力（pin/unpin、batched 优化、压缩、多级），并把这些能力同步到「Persisting 独立 KV API」或 Pulsing 的 KVCacheStorageActor，形成「一套实现、两种入口（LMCache 插件 + Pulsing/Persisting 原生）」。

这样 Persisting 先作为 LMCache 的一个后端，用 LMCache 的接口和生态把 KV 存储的基础能力做实，再在此基础上扩展独立 API 和 Pulsing 集成，会更稳、也更易被现有 LMCache/vLLM 用户接受。

---

## 10. Lance 队列后端与 KV Cache 存储的互动

当前 Persisting 的 **Lance 存储后端** 是为 **队列** 设计的：`record = dict`、按 offset 追加、按 offset/indices 读取。**KV Cache 存储** 则是按 key 查找、value 为张量/二进制块。二者语义不同，但可以在同一套 Lance 基础设施下共存并互动。

### 10.1 语义对比

| 维度 | 队列 LanceBackend（现有） | KV Cache 存储（规划） |
|------|---------------------------|------------------------|
| **数据模型** | 记录流：`dict`，按写入顺序追加 | 键值：CacheEngineKey（或字符串）→ MemoryObj / 二进制块 |
| **写入** | `put(record)` → 缓冲 → flush 合并进一张表 | `put(key, value)` → 按 key 写入/覆盖 |
| **读取** | `get(offset, limit)` 或 `get_by_indices(indices)` | `get(key)`、`contains(key)` |
| **Lance 形态** | 单表按 chunk 追加，列 = record 字段（推断类型） | 键值表：至少两列 `key`（string）+ `payload`（binary），或按 key 分文件/块 |
| **典型路径** | `storage_path` = 每 topic/bucket 一个目录，内建 `data.lance` | 同一根目录下子空间，如 `storage_path/kv/<model>/` 或单独 `data_kv.lance` |

因此：**队列后端** 和 **KV 后端** 是两套 API、两种 schema，但可以共用「Lance 作为底层存储引擎」和「同一存储根路径」。

### 10.2 可以怎样「互动」

1. **共用存储根路径与 Lance 运行时**  
   使用同一 `storage_path`（或同一「Persisting 存储实例」），用子目录或不同 dataset 区分：
   - 队列：`storage_path/queue/<topic>/bucket_<id>/data.lance`（与现有一致）
   - KV Cache：`storage_path/kv/<model>/data.lance` 或按 key 哈希分片到多个 Lance 表  
   这样同一进程、同一配置下，既写队列又写 KV cache，数据都落在同一块盘/命名空间下，便于备份、配额和监控。

2. **共用 Lance 能力，不共用表结构**  
   - 队列表：列 = 业务字段（id, value, name…），行 = 记录。  
   - KV 表：列 = `key`（string）、`payload`（binary）、可选 `meta`（json），行 = 一条 cache 条目。  
   两者都调用 `lance.write_dataset` / `lance.dataset().to_table()` 等，可抽象出一层 **LanceStore** 工具（打开 dataset、按需 schema 读写），队列后端和 KV 后端分别用不同 schema 调用，实现**代码级复用**而不混用同一张表。

3. **数据流上的互动（可选）**  
   - 例如：队列里写入「待预热 / 待淘汰」的 cache key 或 prompt hash，另一个消费者从队列取 key，再对 KV 后端做 prefetch 或 remove。  
   - 此时队列和 KV 在「逻辑」上互动（通过 key 或事件），存储上仍可共用同一 `storage_path` 下的不同 Lance 数据集，无需把队列和 KV 塞进同一张表。

4. **统一配置与未来统一入口**  
   - 配置里一个 `storage_path`，Queue 的 `storage_path` 和 KV 的 `storage_path` 可指向同一根（如 `./data`），队列用 `./data/queue/...`，KV 用 `./data/kv/...`。  
   - 后续若做「Persisting 存储管理器」，可在一个进程内同时挂载队列后端和 KV 后端，共享该根路径与 Lance 依赖，甚至共享统计/健康检查。

### 10.3 建议实现方式

- **保持两个后端类**：`LanceBackend`（队列）与 KV 用的后端（如 `PersistingKVBackend` / LMCache 插件）**不合并**，各自实现自己的协议（Pulsing StorageBackend vs LMCache StorageBackendInterface），避免语义混在一起。
- **约定路径布局**：在文档和实现中约定，若使用同一根路径，则：
  - 队列：`<storage_path>/queue/<topic>/...` 或保持现有 `<storage_path>/data.lance`（按 topic 传不同 path）；
  - KV：`<storage_path>/kv/<model_or_ns>/...` 或单独 dataset 名。
- **可选共享层**：抽一层「Lance 读写工具」或「LanceStore」（按 path + schema 打开/创建 dataset、读/写/删），队列后端和 KV 后端都通过该层访问 Lance，减少重复 I/O 与格式逻辑；两者仍使用不同 schema 和不同 API。

这样，**Persisting 的 Lance 存储后端** 与 **KV Cache 存储** 可以：共用同一存储根路径与 Lance 技术栈、在代码上复用 Lance 读写能力、在数据流上通过 key/事件互动，而**不必**共用同一张表或同一套读写 API，既清晰又可扩展。

---

## 11. LMCache 当前支持的后端一览

根据 `lmcache/v1/storage_backend/__init__.py` 的 `CreateStorageBackends` 与 `connector/__init__.py` 的 `CreateConnector` 整理。

### 11.1 内置存储后端（按配置创建）

| 后端 | 配置条件 | 说明 |
|------|----------|------|
| **LocalCPUBackend** | `config.local_cpu` 且 `max_local_cpu_size > 0` | 本地 CPU 内存缓存；其他后端常依赖其做 get 时的目标缓冲 |
| **PDBackend** | `config.enable_pd` | P/D  disaggregation（预填与解码分离）专用 |
| **P2PBackend** | `config.enable_p2p` | P2P KV 共享，只读（从 peer 拉取），不写 |
| **NixlStorageBackend** | `extra_config.enable_nixl_storage` | [NIXL](https://github.com/ai-dynamo/nixl) 存储 |
| **LocalDiskBackend** | `config.local_disk` 且 `max_local_disk_size > 0` | 本地磁盘，配合 cache_policy（如 LRU） |
| **GdsBackend** | `config.gds_path` 非空 | NVIDIA GPUDirect Storage |
| **RemoteBackend** | `config.remote_url` 非空 | 远端存储，根据 URL scheme 选 Connector（见下表） |

### 11.2 远端 URL 与 Connector（RemoteBackend 使用）

| URL 方案 | 说明 |
|----------|------|
| `redis://`, `rediss://`, `redis-sentinel://` | Redis / Valkey |
| `lm://host:port` | LMCache 自有服务端 |
| `infinistore://host:port[?device=...]` | InfiniStore |
| `mooncakestore://host:port[?device=...]` | MooncakeStore |
| `fs://[host:port]/path` | 文件系统 |
| `s3://...` | AWS S3 / S3 Express |
| `audit://host:port[?verify=...]` | 审计服务 |
| `blackhole://` | 黑洞（测试用） |
| `mock://[capacity]/?...` | 模拟（延迟/吞吐可配） |
| `external://...` | 外部 connector 插件 |

### 11.3 可插拔插件（Storage Plugin）

通过配置动态加载，无需改 LMCache 主仓：

- **config**：`storage_plugins = ["MyBackend"]`，且 `extra_config.storage_plugin.MyBackend.module_path` / `class_name` 指定模块与类名。
- **接口**：插件类实现 `StoragePluginInterface`，构造 `(config, dst_device, metadata, local_cpu_backend, loop)`。
- **仓库内示例**：`RustRawBlockBackend`（块设备、固定 slot、LRU）。

**Persisting** 若实现为 LMCache 后端，即以此方式接入：`storage_plugins = ["PersistingBackend"]`，并配置 `persisting.lmcache_backend` / `PersistingKVBackend`。

### 11.4 包装层

- **AuditBackend**：当 `extra_config.audit_backend_enabled` 为真时，为除 LocalCPUBackend 外的所有后端包一层审计（日志与性能统计）。

---

## 12. 参考文件清单（LMCache 仓库）

| 用途 | 路径 |
|------|------|
| Key 定义与序列化 | `lmcache/utils.py`（CacheEngineKey, to_string, from_string） |
| 存储后端抽象 | `lmcache/v1/storage_backend/abstract_backend.py` |
| 多后端管理 | `lmcache/v1/storage_backend/storage_manager.py` |
| Token → Key | `lmcache/v1/token_database.py` |
| 内存对象与格式 | `lmcache/v1/memory_management.py`（MemoryObj, MemoryFormat） |
| 元数据 | `lmcache/v1/metadata.py`（LMCacheMetadata） |
| 配置 | `lmcache/v1/config.py` |
| 本地磁盘后端 | `lmcache/v1/storage_backend/local_disk_backend.py` |
| 块设备后端 | `lmcache/v1/storage_backend/plugins/rust_raw_block_backend.py` |
| 远程后端 | `lmcache/v1/storage_backend/remote_backend.py` |

以上内容可作为 Persisting 实现 KV Cache 存储时的设计与接口参考；**LMCache 当前支持的后端**见第 11 节；**优先以「Persisting 作为 LMCache 后端」落地**（第 9 节）；**Lance 队列与 KV 存储可共用路径与 Lance 能力**（第 10 节）。

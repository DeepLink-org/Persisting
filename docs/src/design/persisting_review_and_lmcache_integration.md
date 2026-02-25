# Persisting 实现整体 Review 与 LMCache 结合可行性

本文档对 Persisting 当前实现做整体 review，并评估与 LMCache 的结合方式与可行路径。

---

## 1. Persisting 实现整体 Review

### 1.1 模块结构概览

| 模块 | 职责 | 状态 |
|------|------|------|
| **TAA（Rust + Python）** | 寻址：Dimension / Region / TensorView，region_to_blocks / block_to_region | 已实现，Rust 提供 persisting-core (_core)，Python 封装 TensorView |
| **Block / BlockStore** | Block 粒度 L1+L3、get/put/prefetch/wait、write-through | 已实现，单测完整 |
| **Store 协议** | get(region)/put(region)，Handler / TensorNamespace | 已实现，open(backend="tiered") 使用 BlockStore |
| **Rust 底层** | block_read/block_write、mmap_reserve + MmapRegion.copy_in/out | 已实现（Unix），供 L3/缺页填块 |
| **BlockMappedBacking** | 降级 NumpyBacking；use_mmap=True 时 MmapRegion，连续索引 | 已实现 |
| **预取异步化** | prefetch 后台线程、wait 等待、get 未预取时同步拉取 | 已实现 |
| **RemoteBacking** | L3 按块委托 get_block/put_block，可 stub 或 RPC | 已实现 |
| **BlockStoreActor** | Pulsing @remote，get_region/put_region，ActorStore 代理 | 已实现，需 Pulsing 运行时 |
| **Queue / Sampler** | 流式队列、Lance 后端、采样器 | 已有，与分层存储独立 |
| **DistributedStore** | 按 partition_key 路由到本机/远程 BlockStore | 未实现 |
| **UFFD/缺页** | 用户态缺页填页、numpy 连续视图 | 未实现（设计已预留） |
| **RDMA 数据面** | 零拷贝跨节点块传输 | 暂缓 |

### 1.2 数据流与 API 一致性

- **用户 API**：`open(...) → kv[key] → h.tensor() / h.put(data)` 与 README/llms.binding 一致；`prefetch(key)` / `wait(key)` 在 tiered 下转发到 BlockStore。
- **索引语义**：Region 对应「单点 + 一段」时，get/put 的 shape 为 order_dim 长度的一维（如 64、128），与 Numpy 高级索引一致；多块时由 BlockStore 按块拼成连续读。
- **线程安全**：BlockStore 内 _blocks_in_l1、预取 pending 用 Lock 保护；Backing 实现（Numpy/Mmap/Remote）未强制要求线程安全，单写单读或 GIL 下使用。

### 1.3 测试与可维护性

- **单测**：Block/BlockStore/BlockMappedBacking/RemoteBacking/PrefetchAsync/BlockStoreActor 均有对应用例；步骤文档（tiered_storage_implementation_steps.md）与实现一一对应。
- **依赖**：persisting 依赖 pulsing、tensordict；Rust 仅 libc（mmap）、pyo3；Lance 为可选。
- **缺口**：DistributedStore 与多节点 E2E 测试缺失；UFFD/真实缺页路径未实现；与 vLLM 的集成未在本仓内。

### 1.4 小结：当前强项与短板

- **强项**：TAA 寻址、Block 粒度分层、Store 抽象、Rust 块 I/O 与 mmap 预留、预取异步、服务化（Actor + RemoteBacking）、文档与单测齐全。
- **短板**：无分布式路由层、无 UFFD 填页、无 LMCache/vLLM 直连；L3 持久化目前以 NumpyBacking/MmapBacking 为主，与 Lance 队列共用存储根路径的约定尚未在 tiered 路径中体现。

---

## 2. LMCache 结合可能性

### 2.1 LMCache 侧关键抽象（简要）

- **Key**：`CacheEngineKey`（model_name, world_size, worker_id, chunk_hash, dtype, request_configs），序列化为字符串，支持「按 chunk 顺序、最长前缀命中」。
- **Value**：`MemoryObj`（shape、dtype、format、address 等），由 StorageManager 在 get 时用 `local_cpu_backend` 分配目标缓冲再拉数据。
- **存储接口**：`StorageBackendInterface` — contains(key, pin)、exists_in_put_tasks(key)、batched_submit_put_task(keys, objs, ...)、get_blocking(key)、get_non_blocking(key)、pin/unpin、remove、batched_contains、batched_get_non_blocking 等；插件通过 `StoragePluginInterface` 注册（config + dst_device + metadata + local_cpu_backend + loop）。

详见 Persisting 已有文档：`docs/src/design/lmcache_kvcache_reference.zh.md`。

### 2.2 结合方向概览

| 方向 | 含义 | 可行性 |
|------|------|--------|
| **A. Persisting 作为 LMCache 后端** | 实现 LMCache 的 StorageBackendInterface/StoragePluginInterface，用 BlockStore 或 Store 做底层存储 | 高：接口清晰，key→region、MemoryObj→ndarray 需一层适配 |
| **B. LMCache 作为 Persisting 的 L3** | BlockStore 的 L3 使用「能按 key 读写的后端」，LMCache 的 LocalCPUBackend/LocalDiskBackend 语义上可视为 key-value 存储 | 中：需把 CacheEngineKey 映射为 Persisting 的 BlockId/region 或 norm_key，并统一 value 格式 |
| **C. 共享存储根与格式** | 同一 storage_path 下，队列用 Lance 表，KV 用 key+payload 表或块文件，Persisting 与 LMCache 共用路径与 Lance 能力 | 高：文档 10.1–10.3 已描述，实现上约定路径与 schema 即可 |
| **D. 控制面/数据面分离** | Pulsing 做发现与路由，Persisting BlockStore 做单节点存储；LMCache 的 RemoteBackend 可指向「Persisting 暴露的 RPC/Actor」 | 中：需在 LMCache 侧增加一种 connector（如 persisting:// 或使用现有 external 插件），调用 Persisting Actor 或 HTTP 接口 |

### 2.3 推荐路径：先做 LMCache 后端插件

与 `lmcache_kvcache_reference.zh.md` 第 9 节一致，建议**先把 Persisting 实现为 LMCache 的一个 Storage Plugin 后端**：

1. **接口即契约**：实现 `StorageBackendInterface`（或插件要求的 `StoragePluginInterface`），直接对齐 contains/put/get/remove、key=CacheEngineKey、value=MemoryObj，避免两套语义长期分叉。
2. **真实场景**：接入 LMCache 后可在 vLLM/SGLang 真实推理链中验证持久化、序列化、多级与前缀语义。
3. **复用现有能力**：Persisting 已有 BlockStore、RemoteBacking、BlockStoreActor、region 序列化；适配层只需做「CacheEngineKey ↔ (partition_key, block_id) 或 norm_key」「MemoryObj ↔ ndarray/bytes」。
4. **双入口**：同一套 BlockStore/Store 实现，既可作为「Persisting 原生 open(tiered)」的底层，也可作为「LMCache 配置中的 Persisting 后端」被 vLLM 使用。

### 2.4 适配层设计要点

- **Key 映射**：  
  - LMCache：`CacheEngineKey` → 字符串（如 to_string）。  
  - Persisting：可用「单 Block」语义：partition_key = (model_name, worker_id, layer_id?) 或由 key 哈希得到；block_id 可由 chunk_hash 或 chunk 序数推导；或简化为「一个 CacheEngineKey 对应一个 Region」（单块），则 norm_key = (…, (start, stop)) 由 key 唯一确定。  
  - 建议：为 Persisting 后端定义「CacheEngineKey → region_ser 或 BlockId 或稳定 norm_key」的规则，保证 contains/get/put 一致。

- **Value 映射**：  
  - LMCache：MemoryObj（含 shape、dtype、format、底层 buffer）。  
  - Persisting：Store 读写的为 ndarray；Backing 按 indices 读写。  
  - 适配：get 时用 LMCache 注入的 local_cpu_backend 分配 MemoryObj，从 Persisting 读出 ndarray 后拷贝或 wrap 进 MemoryObj；put 时从 MemoryObj 取出 tensor/bytes 写入 Persisting。序列化格式可与 LMCache naive_serde 或现有格式对齐。

- **最小实现集合**（参考文档 9.3）：  
  - contains(key, pin)、exists_in_put_tasks(key)、batched_submit_put_task、get_blocking(key)、get_allocator_backend() 返回 local_cpu_backend、pin/unpin（可 no-op）、remove(key)、close()；batched_contains / batched_get_non_blocking 可先单 key 循环再优化。

- **存储根路径**：  
  - 与 Lance 队列约定同一根路径时，KV 使用子目录如 `<storage_path>/kv/<model>/` 或独立 dataset，避免与队列表 schema 混用。

### 2.5 与现有 Persisting 组件的对应关系

| LMCache 概念 | Persisting 对应 |
|--------------|-----------------|
| StorageBackendInterface | 新模块 `persisting.lmcache_backend.PersistingKVBackend` 实现该接口，内部使用 BlockStore 或 Store |
| key (CacheEngineKey) | 映射为 Region 或 BlockId 或稳定 norm_key，用于 Store.get/put 或 RemoteBacking get_block/put_block |
| value (MemoryObj) | 与 ndarray 互转；持久化用 block_write / 或 Lance key+payload |
| local_cpu_backend | 由 LMCache 注入，Persisting 后端 get 时用其分配目标 MemoryObj |
| 多级 (CPU + Disk + Remote) | Persisting 侧 BlockStore L1+L3、RemoteBacking、BlockStoreActor 已具备；LMCache StorageManager 负责选后端与 put/get 路由 |
| 前缀/最长匹配 | LMCache 在 StorageManager 层做 batched_contains 与 prefix 语义；Persisting 后端只需按 key 单条 contains/get/put |

---

## 3. 建议的下一步（优先级）

1. **实现 Persisting LMCache 后端**  
   在 Persisting 仓内新增 `persisting/lmcache_backend.py`（或 `persisting/integrations/lmcache_backend.py`），实现 StoragePluginInterface，底层用 BlockStore + 单文件或 Lance key+payload；定义 CacheEngineKey → region/norm_key 的稳定规则与 MemoryObj ↔ ndarray 的序列化。

2. **LMCache 配置与 E2E**  
   在 LMCache 配置中启用 Persisting 插件（storage_plugins + extra_config），与 local_cpu/local_disk 一起跑 vLLM 用例，验证正确性与性能。

3. **路径与 Lance 约定**  
   在文档与实现中约定：同一 storage_path 下队列用 `queue/...`，KV 用 `kv/...` 或独立 Lance 表，并可选抽象 LanceStore 供队列与 KV 共用。

4. **DistributedStore 与 Pulsing**  
   在「单节点 Persisting 后端」稳定后，再做 DistributedStore（partition_key 路由）和「LMCache RemoteBackend 指向 Persisting Actor 或 HTTP」的 connector，实现跨节点 KV。

5. **UFFD/缺页**  
   在 BlockMappedBacking 与 MmapRegion 就绪的前提下，按设计文档接入 UFFD（或 macOS 降级），实现透明缺页填块与 numpy 连续视图。

---

## 4. 总结

- **Persisting 当前实现**：TAA、Block、BlockStore、Store、Rust 块 I/O 与 mmap、预取异步、RemoteBacking、BlockStoreActor 已就绪；缺分布式路由、UFFD 与 LMCache/vLLM 直连。
- **与 LMCache 结合**：最稳妥路径是「Persisting 作为 LMCache Storage Plugin 后端」；通过 CacheEngineKey ↔ region/norm_key、MemoryObj ↔ ndarray 的适配层，复用现有 BlockStore/Store/RemoteBacking/Actor 能力，并在 LMCache 配置与 vLLM 场景下验证；同时约定与 Lance 队列共用存储根路径与 schema 分离，便于后续统一存储管理与多级扩展。

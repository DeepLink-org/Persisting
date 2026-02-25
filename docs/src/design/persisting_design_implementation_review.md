# Persisting 设计与实现整体 Review

本文档仅针对 Persisting 自身：对照设计文档梳理当前实现状态、接口一致性、测试覆盖与缺口，不涉及 LMCache 等外部集成。

**更新**：更完整的项目级审阅（含 Pin/Unpin 与 GPU 中心显存管理设计讨论现状、UFFD/缺页已实现状态）见 [persisting_project_review.md](persisting_project_review.md)。

---

## 1. 设计文档与职责边界

### 1.1 设计文档索引

| 文档 | 内容 |
|------|------|
| [tensor_address_algebra.md](tensor_address_algebra.md) | TAA 寻址模型：Dimension / Address / Constraint / Region、KeyOrder、规范化与 lowering |
| [distributed_tiered_storage.md](distributed_tiered_storage.md) | 综合设计：Block、mmap+UFFD、BlockStore、DistributedStore、四级存储、预取与缺页 |
| [tiered_storage_implementation_steps.md](tiered_storage_implementation_steps.md) | 实现步骤与单测清单（Step 1–11 已完成；UFFD/缺页已实现 Linux+macOS；Step 12 暂缓） |
| [pin_unpin_mechanism_discussion.md](pin_unpin_mechanism_discussion.md) | Pin/Unpin 与显存管理设计讨论（两条时间线、设备侧决策、GPU 中心方案；当前方案不足，未实现） |
| [architecture.md](architecture.md) | 队列持久化：Queue → LanceBackend / PersistingBackend，与 Pulsing 控制面关系 |

### 1.2 两条主线

- **队列主线**：Queue / LanceBackend / Sampler / KVInterface — 流式写入、列存、采样；与「分层 tensor 存储」独立。
- **Tensor 存储主线**：open() → TensorNamespace → Handler → Store；TAA 驱动 Region；BlockStore 做 L1+L3 分层；设计上还有 DistributedStore、UFFD、RDMA。

以下 review 以 **Tensor 存储主线** 为主，队列仅指出与存储的边界。

---

## 2. 设计 → 实现对照

### 2.1 TAA（寻址模型）

| 设计 | 实现 | 说明 |
|------|------|------|
| Dimension / Point / Range / SetC / Region | `persisting.core`（Rust persisting-core (_core) + Python 封装） | TensorView[key] → Region，project_prefix、select、shift_range 等 |
| KeyOrder、order_dim 在最后 | block.py 中 region_to_blocks 要求 prefix_dims 全 Point，order_dim 上 Range/Point | 与设计一致 |
| region_to_blocks / block_to_region | `persisting/store/block.py` | BlockId(partition_key, block_id)，与设计文档公式一致 |
| NF-PointQuery / NF-RangeQuery | 未单独成模块 | 当前通过 TensorView 下标直接得到 Region，由 BlockStore 使用，未显式做 NF 拆解 |

**结论**：TAA 核心（维度、约束、Region、Block 列表）已实现并用于 BlockStore；规范化层若需可后续在 Handler/Store 之上加一层。

### 2.2 Block 与 BlockStore

| 设计 | 实现 | 说明 |
|------|------|------|
| Block = (namespace, partition_key, block_id) | BlockId(partition_key, block_id)，namespace 在 Store/Namespace 层 | 等价 |
| L1 + L3，write-through | BlockStore：_l1 / _l3 为 Backing，put 同时写 L1 与 L3 | 一致 |
| get 缺块从 L3 拉入 L1 | _ensure_block_in_l1：按块从 L3 read 再 write 到 L1，_blocks_in_l1 标记 | 一致 |
| prefetch / wait | prefetch 提交到 ThreadPoolExecutor，wait 等 _prefetch_pending 的 Event | 异步预取已实现 |
| block_table（tier/state/位置） | 未实现完整 BlockEntry | 当前用 _blocks_in_l1 布尔集合，无 tier/ssd_fd/remote_node 等；L3 由 Backing 抽象承载 |

**结论**：Block 粒度、L1/L3、预取语义已实现；「每 Block 的 tier/位置表」在设计文档中有，实现上简化为「在 L1 与否」+ Backing 类型（Numpy/Mmap/Remote），满足当前单机与 RemoteBacking 需求。若后续做 UFFD/多 tier，再引入 BlockEntry 不迟。

### 2.3 Store 协议与 open()

| 设计 | 实现 | 说明 |
|------|------|------|
| Store：get(region)/put(region, data) | local_tensor.Store 协议 + LocalTensorStore/BlockStore 实现 | 一致 |
| Handler：tensor()/put(data) | Handler 调用 store.get/put | 与 llms.binding 一致 |
| TensorNamespace：kv[key] → Handler，prefetch/wait | 实现；prefetch/wait 通过 hasattr(store,"prefetch") 转发 | 一致 |
| open(backend="local" \| "tiered") | __init__.py：local → LocalTensorStore，tiered → BlockStore | 一致；partition_dims 未在 open 中使用，留给 DistributedStore |

### 2.4 Backing 与 BlockMappedBacking

| 设计 | 实现 | 说明 |
|------|------|------|
| Backing：read(indices)/write(indices, data) | NumpyBacking、MmapBacking、SafetensorsBacking、BlockMappedBacking、RemoteBacking | 多种实现 |
| BlockMappedBacking 降级 | block_table is None 时用 NumpyBacking | 与步骤文档一致 |
| use_mmap=True 时 MmapRegion | 使用 core.mmap_reserve、copy_in/copy_out，连续索引 | 仅 Unix，非连续索引 NotImplementedError |
| _indices_to_contiguous_byte_range | 用于 Mmap 路径的字节区间计算 | 已实现 |

### 2.5 Rust 底层

| 设计 | 实现 | 说明 |
|------|------|------|
| 块级文件 I/O | block_io.rs：block_read / block_write | 已导出，供 L3 或未来缺页填块 |
| mmap 预留 + 填块/读块 | mmap_region.rs：mmap_reserve、MmapRegion.copy_in/out | #[cfg(unix)]，Drop 时 munmap |
| UFFD / 缺页 | 设计文档有详细描述 | **已实现**：Linux uffd.rs、macOS page_fault_darwin.rs；Python 统一 start_uffd_handler；e2e 仅 Linux。与 BlockStore 预取统一事件循环（Rust TieredLoop 填页）尚未打通。 |

### 2.6 服务化与分布式

| 设计 | 实现 | 说明 |
|------|------|------|
| RemoteBacking | get_block/put_block 委托，_norm_indices 做键 | 单测用内存 dict stub，可换 RPC/Actor |
| BlockStore 作为 Pulsing Actor | block_store_actor.py：BlockStoreActor、ActorStore、region_serialize/deserialize | 已实现；catalog 以 dict 序列化传递，避免 Dimension 跨进程 |
| DistributedStore | partition_key → 本机/远程、route 到 BlockStore | **未实现**；设计文档中有架构图与 trace |

### 2.7 队列与存储的边界

| 组件 | 说明 |
|------|------|
| Queue / LanceBackend | 流式队列、列存落盘；与 tensor 的 open/kv[key] 无直接共用代码 |
| 存储根路径 | 设计文档 10.x 提到与 Lance 共用 storage_path、KV 用子目录；当前 tiered 的 L3 多为内存或测试用路径，未统一约定 |

---

## 3. 接口与行为一致性

- **用户 API**：open() → kv[key] → h.tensor() / h.put(data)、kv.prefetch(key)/kv.wait(key) 与 README、llms.binding 一致。
- **索引语义**：Region 对应「单点 + 一段」时，get/put 的 shape 为 order_dim 长度的一维（如 64、128），与 Numpy 高级索引一致；多块时 BlockStore 按块从 L3 拉取再拼成连续读。
- **线程安全**：BlockStore 内 _blocks_in_l1、_prefetch_pending 用 Lock；Backing 未强制要求线程安全，单写单读或 GIL 下使用合理。

---

## 4. 测试覆盖

| 区域 | 测试文件 | 要点 |
|------|----------|------|
| Block | test_block_store.py | BlockId、region_to_blocks、block_to_region（含空范围、越界、prefix 非 Point） |
| BlockStore | test_block_store.py | get/put、L3 拉取、prefetch/wait、write-through、BlockMappedBacking、RemoteBacking、prefetch 异步 |
| BlockStoreActor | test_block_store_actor.py | region 序列化 roundtrip、put_then_get、ActorStore get_async/put_async（依赖 Pulsing 时 skip） |
| Rust | test_block_io.py | block_read/block_write、MmapRegion copy_in/out |
| Store/Handler | test_local_tensor_store.py | LocalTensorStore、Handler、下标 → Region |
| TAA | test_taa_subscript_planning.py | TensorView 下标规划 |
| 队列 | test_queue_* / test_sampler | 与 tensor 存储分离 |

步骤文档（tiered_storage_implementation_steps.md）与上述单测一一对应；全量 `pytest tests/` 可作回归。

**缺口**：DistributedStore 无实现故无单测；UFFD 有契约单测与 Linux e2e（test_page_fault_handler），缺与 BlockStore 打通的统一预取/填页测试；多节点 E2E 无；与 Lance 共用存储根的集成测试无。

---

## 5. 实现质量与可维护性

- **依赖**：persisting 依赖 pulsing、tensordict；Rust 仅 libc（mmap）、pyo3；Lance 可选。BlockStoreActor 在无 Pulsing 时可跳过相关用例。
- **代码结构**：store（block、block_store、local_tensor、block_store_actor）职责清晰；TAA 在 core.py + Rust；队列在 queue/、sampler/、dataloader/。
- **文档**：设计文档与实现步骤对应良好，便于后续接 Step 12（RDMA）或 UFFD、DistributedStore。

---

## 6. 总结：已实现 vs 未实现

**已实现且与设计一致**  
- TAA 寻址（Dimension/Region/Block 列表）、BlockId、region_to_blocks/block_to_region  
- BlockStore：L1+L3、write-through、prefetch 异步、wait  
- Store 协议、Handler、TensorNamespace、open(local|tiered)  
- Backing：Numpy、Mmap、BlockMappedBacking（含 use_mmap）、RemoteBacking  
- Rust：block_read/block_write、mmap_reserve、MmapRegion；UFFD（Linux）、缺页（macOS Mach）  
- BlockStoreActor、ActorStore、region 序列化  

**已设计未实现**  
- DistributedStore（partition_key 路由、本机/远程 BlockStore）  
- UFFD/缺页与 BlockStore 打通（Rust TieredLoop 填页与预取统一事件循环）  
- 完整 block_table（BlockEntry：tier/state/位置）  
- RDMA 数据面（Step 12 暂缓）  
- 与 Lance 队列共用 storage_path 的约定与统一 L3 路径  

**设计讨论中、未落地**  
- Pin/Unpin 与显存生命周期自动管理（llms.binding §2.7、[pin_unpin_mechanism_discussion.md](pin_unpin_mechanism_discussion.md)）；完全 GPU 中心显存管理（GDA-KI/IBGDA、persistent kernel 等）为讨论方向，当前方案仍不足。  

**建议优先级**  
1. 若需多节点：先做 DistributedStore（路由 + ActorStore 作为远程端点）。  
2. 若需透明缺页与预取统一：在 Rust TieredLoop 中接入 UFFD/Mach 缺页与 BlockStore 的 L3 填页，预取与缺页走同一事件循环。  
3. 若需统一存储根：在文档与 open()/BlockStore 中约定 L3 根路径与 Lance 子目录（如 `kv/` vs `queue/`）。

---

## 7. 点评与打分

评分说明：1–5 分，5 为优秀、4 良好、3 合格、2 不足、1 缺失/差。可带 0.5。

### 7.1 设计

| 维度 | 得分 | 点评 |
|------|------|------|
| **概念清晰度** | 4.5 | TAA（Dimension/Region/Block）、Block 粒度、L1/L3、预取/UFFD 角色分工明确；与 PGAS 对比、KeyOrder 约束、NF 形态在文档中有清晰界定，易于实现者对齐。 |
| **文档完整度** | 4 | 有 TAA 形式化、分层存储综合设计、实现步骤清单、端到端 trace；缺一份「单页架构总览」把队列与 tensor 两条线画在一起。 |
| **可扩展性** | 4 | Backing 抽象、Store 协议、region 序列化便于加新后端与分布式；BlockEntry 未实现但设计已预留；UFFD/RDMA 以「后续步骤」形式存在，未锁死实现路径。 |
| **设计小计** | **4.2** | 设计层面成熟度高，边界清楚，文档与实现步骤可追溯。 |

### 7.2 实现

| 维度 | 得分 | 点评 |
|------|------|------|
| **与设计一致性** | 4.5 | 已实现部分（TAA、BlockStore、Store/Handler、Backing、Rust 块 I/O 与 mmap）与设计文档和步骤清单高度一致；未实现部分（DistributedStore、UFFD、RDMA）在文档中有明确描述，无「设计写了但实现跑偏」的情况。 |
| **代码结构** | 4 | store/block、block_store、local_tensor、block_store_actor 职责清晰；TAA 在 core + Rust；队列与 tensor 存储解耦。少量可改进：BlockStore 内 L1/L3 与预取逻辑同文件略长，可按需拆出 prefetch 模块。 |
| **接口稳定性** | 4 | 对外 open/kv[key]/h.tensor()/h.put()、prefetch/wait 稳定；Store 协议、Backing 协议清晰；region 序列化格式已用于 Actor，变更需兼容。 |
| **实现小计** | **4.2** | 实现质量好，与设计咬合紧，结构利于后续接 DistributedStore 与 UFFD。 |

### 7.3 测试与可维护性

| 维度 | 得分 | 点评 |
|------|------|------|
| **测试覆盖** | 4 | Block/BlockStore/Backing/Actor/Rust 有对应单测，步骤与用例可一一对应；缺分布式、UFFD、多节点 E2E 与存储根约定相关测试，属「未实现功能」的自然缺口。 |
| **可维护性** | 4 | 步骤文档与单测对应；依赖清晰（pulsing、tensordict、Rust 最小集）；新成员可按 tiered_storage_implementation_steps 接下一阶段。 |
| **测试与可维护性小计** | **4.0** | 单测到位，文档可追溯，维护成本可控。 |

### 7.4 总评与总分

| 项目 | 得分 |
|------|------|
| 设计 | 4.2 |
| 实现 | 4.2 |
| 测试与可维护性 | 4.0 |
| **总分（均权）** | **4.1 / 5** |

**总评（一句话）**：设计与实现整体达到「良好偏上」水平：概念清晰、文档与步骤可追溯、已实现部分与设计一致且单测齐全；扣分主要来自分布式与缺页等未实现项及存储根/队列统一约定的缺失，属阶段性缺口而非设计或实现质量缺陷。

# Persisting 项目设计与实现 — 整体 Review

本文档对 Persisting 项目的**设计**与**实现**做一次整体审阅：目标与范围、设计文档索引、已实现/未实现/设计讨论中的内容、缺口与建议。可与 [persisting_design_implementation_review.md](persisting_design_implementation_review.md) 对照阅读，本文档在此基础上补充近期变更与 pin/显存管理方向。

---

## 1. 项目目标与范围

**Persisting** 提供 **AI 负载的分布式分层存储**：参数、KV Cache、轨迹等，按**多维 tensor 下标**寻址，数据分布在 GPU / host / SSD / 远程节点，**按需物化**。

| 表面 | 用途 |
|------|------|
| **Tensor 内存** | `open` → `kv[key]` → `h.tensor()` / `h.put(data)`；支持 `backend="local"` 或 `"tiered"`，tiered 下 prefetch/wait |
| **队列** | 流式追加与消费：Queue、Writer/Reader、get_meta/get_data、KVInterface、Sampler |

**控制面**：Pulsing（节点发现、消息、生命周期）。**数据面**：Persisting（TAA 寻址、分层、放置）。

---

## 2. 设计文档索引

| 文档 | 内容 |
|------|------|
| [tensor_address_algebra.md](tensor_address_algebra.md) | TAA：Dimension/Address/Constraint/Region、KeyOrder、规范化与 lowering |
| [distributed_tiered_storage.md](distributed_tiered_storage.md) | 综合设计：Block、mmap+UFFD、BlockStore、DistributedStore、四级存储、预取与缺页、事件循环（Rust 侧） |
| [tiered_storage_implementation_steps.md](tiered_storage_implementation_steps.md) | 实现步骤与单测清单（Step 1–11 已完成；UFFD/缺页已实现 Linux+macOS；Step 12 RDMA 暂缓） |
| [pin_unpin_mechanism_discussion.md](pin_unpin_mechanism_discussion.md) | Pin/Unpin 与显存管理**设计讨论**：生命周期自动管理、两条时间线、设备侧决策、GDA-KI/IBGDA、persistent kernel、GPU 中心显存管理；**当前方案仍不足**，文档为讨论与迭代用 |
| [architecture.md](architecture.md) | 队列持久化、LanceBackend、与 Pulsing 控制面关系 |
| [llms.binding.md](../../../llms.binding.md) | **契约**：Tensor 内存 API（稳定）、Queue、TAA、Pin/Unpin 方向（设计方向，未完全实现） |

---

## 3. 设计 → 实现对照

### 3.1 TAA（寻址模型）

| 设计 | 实现 | 状态 |
|------|------|------|
| Dimension / Point / Range / SetC / Region | `persisting.core`（Rust persisting-core + Python 封装） | ✅ |
| TensorView[key] → Region，project_prefix、select、shift_range | core.py TensorView；Rust 侧 Region/Constraint | ✅ |
| region_to_blocks / block_to_region | `persisting/store/block.py`，BlockId(partition_key, block_id) | ✅ |
| NF-PointQuery / NF-RangeQuery | 未单独模块；由 TensorView 与 BlockStore 直接使用 | 可选后续 |

### 3.2 Block 与 BlockStore

| 设计 | 实现 | 状态 |
|------|------|------|
| Block = (partition_key, block_id)，L1 + L3 write-through | BlockStore：_l1/_l3 为 Backing，put 写 L1 与 L3 | ✅ |
| get 缺块从 L3 拉入 L1，_blocks_in_l1 标记 | _ensure_block_in_l1，按块从 L3 read 再 write 到 L1 | ✅ |
| prefetch / wait | prefetch 提交 ThreadPoolExecutor，wait 等 _prefetch_pending 的 Event | ✅ |
| block_table（tier/state/位置） | 简化为「在 L1 与否」+ Backing 类型；无完整 BlockEntry | 简化实现，满足当前单机与 RemoteBacking |

### 3.3 Store 协议与 open()

| 设计 | 实现 | 状态 |
|------|------|------|
| Store：get(region)/put(region, data) | local_tensor.Store 协议，LocalTensorStore/BlockStore 实现 | ✅ |
| Handler：tensor()/put(data)；TensorNamespace：kv[key]、prefetch(key)/wait(key) | 一致；prefetch/wait 通过 hasattr(store,"prefetch") 转发 | ✅ |
| open(backend="local" \| "tiered") | __init__.py；tiered 必填 order_dim | ✅ |

### 3.4 Backing 与 Rust 底层

| 设计 | 实现 | 状态 |
|------|------|------|
| NumpyBacking、MmapBacking、BlockMappedBacking、RemoteBacking、SafetensorsBacking | local_tensor.py | ✅ |
| BlockMappedBacking(use_mmap=True) + MmapRegion | 连续索引 path，copy_in/out；仅 Unix | ✅ |
| block_read / block_write | crates/persisting-core/block_io.rs，Python 导出 | ✅ |
| mmap_reserve、MmapRegion | mmap_region.rs，#[cfg(unix)] | ✅ |
| UFFD 缺页（Linux） | uffd.rs；Python 统一 start_uffd_handler，e2e 仅 Linux | ✅ |
| 缺页（macOS） | page_fault_darwin.rs，Mach exception port，标准模式；e2e 在 macOS 上 skip | ✅ |
| TieredLoop（Rust 预取队列） | tiered_loop.rs，start/submit_prefetch/stop；填页占位，未与 BlockStore 打通 | 骨架已有 |

### 3.5 服务化与分布式

| 设计 | 实现 | 状态 |
|------|------|------|
| RemoteBacking（get_block/put_block） | 已实现；单测用内存 dict stub | ✅ |
| BlockStore 作为 Pulsing Actor | block_store_actor.py：BlockStoreActor、ActorStore、region 序列化 | ✅ |
| **DistributedStore**（partition_key 路由、本机/远程 BlockStore） | **未实现** | ❌ |
| RDMA 数据面（Step 12） | 暂缓 | ❌ |

### 3.6 队列主线

| 设计 | 实现 | 状态 |
|------|------|------|
| Queue、Writer、Reader、LanceBackend、PersistingBackend | queue/queue.py、backend.py | ✅ |
| get_meta、get_data、KVInterface、Sampler（Sequential、RankAware、GRPOGroupN） | queue/、sampler/ | ✅ |
| TensorDict 序列化、zerocopy 描述符 | queue/tensor_serde.py | ✅ |

### 3.7 Pin/Unpin 与显存管理（设计讨论，未实现）

| 契约/讨论 | 实现 | 状态 |
|-----------|------|------|
| persisting.pin(ref, after=stream)、生命周期自动管理、两条时间线、设备侧决策 | **未实现**；llms.binding §2.7 注明为设计方向 | 设计讨论中 |
| GPU 自维护 ref 表、GDA-KI/IBGDA、persistent kernel、完全 GPU 中心显存管理 | 见 pin_unpin_mechanism_discussion.md，**未实现** | 设计讨论中 |

---

## 4. 实现结构概览

```
persisting/
├── __init__.py          # open(), 导出
├── core.py              # TensorView（Python 侧）；Dimension/Region 等由 _core 提供
├── store/
│   ├── block.py         # BlockId, region_to_blocks, block_to_region
│   ├── block_store.py   # BlockStore, L1/L3, prefetch/wait
│   ├── local_tensor.py  # Store 协议, Backing 们, Handler, TensorNamespace, TensorLayout
│   ├── block_store_actor.py  # BlockStoreActor, ActorStore, region 序列化
│   └── tiered_loop.py   # Python 侧 TieredEventLoop（已废弃，Rust 为主）
├── queue/               # Queue, LanceBackend, metadata, tensor_serde, status_tracker, kv_interface
├── sampler/             # BaseSampler, Sequential, RankAware, GRPOGroupN
└── dataloader/          # StreamingDataset, StreamingDataLoader

crates/persisting-core/
├── lib.rs               # Dimension, Region, TensorView 等 TAA；py 导出
├── block_io.rs          # block_read, block_write
├── mmap_region.rs       # mmap_reserve, MmapRegion (#[cfg(unix)])
├── uffd.rs              # Linux userfaultfd (#[cfg(target_os = "linux")])
├── page_fault_darwin.rs # macOS Mach exception (#[cfg(target_os = "macos")])
└── tiered_loop.rs       # TieredLoop 预取队列骨架
```

**测试**：`tests/test_block_store.py`（Block/BlockStore/Backing/OpenTiered/Prefetch/RemoteBacking/TieredLoop）、`test_block_io.py`、`test_local_tensor_store.py`、`test_taa_subscript_planning.py`、`test_block_store_actor.py`、`test_page_fault_handler.py`（UFFD 契约 + Linux e2e）、队列与 sampler 等。

---

## 5. 缺口与建议

### 5.1 已实现且与设计一致

- TAA 寻址（Dimension/Region/Block 列表）、BlockId、region_to_blocks/block_to_region  
- BlockStore：L1+L3、write-through、prefetch 异步、wait  
- Store 协议、Handler、TensorNamespace、open(local|tiered)  
- Backing：Numpy、Mmap、BlockMappedBacking（含 use_mmap）、RemoteBacking、Safetensors  
- Rust：block_read/block_write、mmap_reserve、MmapRegion；UFFD（Linux）、缺页（macOS Mach）  
- BlockStoreActor、ActorStore、region 序列化  
- 队列：Queue、LanceBackend、Writer/Reader、Sampler、metadata、tensor_serde  

### 5.2 已设计未实现

- **DistributedStore**：partition_key 路由、本机/远程 BlockStore 选择  
- **UFFD/缺页与 BlockStore 打通**：设计上预取与缺页应由同一 Rust 事件循环处理；当前 BlockStore 预取仍用 Python ThreadPoolExecutor，Rust TieredLoop 未接入填页与完成通知  
- **完整 block_table**：每 Block 的 tier/state/位置（gpu_ptr、remote_node、ssd_fd 等）；当前简化为「在 L1 与否」  
- **RDMA 数据面**（Step 12）：暂缓  
- **L3 与 Lance 共用 storage_path**：未统一约定目录布局  

### 5.3 设计讨论中、未落地

- **Pin/Unpin**：契约 §2.7 与 [pin_unpin_mechanism_discussion.md](pin_unpin_mechanism_discussion.md) 描述方向（生命周期自动管理、两条时间线、设备侧决策、`persisting.pin(ref, after=stream)`）；实现尚未满足设备侧决策与两条时间线对齐。  
- **完全 GPU 中心显存管理**：GPU 自维护 ref 表、GDA-KI/IBGDA 做 system↔GPU copy、persistent kernel 调度、参数迟绑定、指针擦除等，为讨论中的目标架构，依赖硬件/栈（GDA-KI、CDP 等）与后续实现选型。  

### 5.4 建议优先级

1. **多节点**：实现 DistributedStore（路由 + 现有 ActorStore 作远程端点），补单测与 E2E。  
2. **透明缺页与预取统一**：在 Rust TieredLoop 中接入 UFFD/Mach 缺页与 BlockStore 的 L3 填页逻辑，预取与缺页走同一事件循环；Python 侧 prefetch/wait 可改为提交到 Rust 并等待完成。  
3. **存储根与 Lance**：在文档与 open()/BlockStore 中约定 L3 根路径与 Queue 子目录（如 `kv/` vs `queue/`），便于集成与运维。  
4. **Pin/Unpin 与显存管理**：在「当前方案不足」前提下，先明确降级路径（host 侧 enqueue 前/stream 回调 trigger）；若目标栈支持 GDA-KI/IBGDA，再迭代设备侧决策与 GPU 中心方案，并保持 [pin_unpin_mechanism_discussion.md](pin_unpin_mechanism_discussion.md) 与契约同步更新。  

---

## 6. 总结

| 维度 | 状态 |
|------|------|
| **设计** | 概念清晰（TAA、Block、分层、预取/缺页、事件循环在 Rust）；文档完整且与实现步骤可追溯；pin/显存管理为开放设计讨论，已标明「当前方案仍不足」。 |
| **实现** | 单机 tensor 存储（open、kv[key]、tensor/put、prefetch/wait）、BlockStore L1+L3、多种 Backing、Rust 块 I/O 与 mmap、UFFD（Linux）/缺页（macOS）、BlockStoreActor 已实现并与设计一致；DistributedStore、UFFD 与 BlockStore 打通、完整 block_table、RDMA 未实现；Pin/Unpin 与 GPU 中心显存管理未实现，属设计讨论方向。 |
| **测试** | Block/BlockStore/Backing/Actor/Rust/缺页契约 有对应单测；缺分布式、多节点 E2E、存储根与 Lance 集成测试。 |

**总评**：Persisting 在「单机分层 tensor 存储 + 队列」上设计与实现对齐良好，可维护、可扩展；分布式与透明缺页统一调度为下一阶段重点；Pin/Unpin 与 GPU 中心显存管理为重要设计方向，文档已系统讨论（两条时间线、设备侧决策、GDA-KI/IBGDA、persistent kernel 等），待能力与优先级确定后再落地实现。

# 分层存储实现步骤与测试清单

与 [distributed_tiered_storage.md](distributed_tiered_storage.md) 对应，按「小步实现、每步有严格单元测试」推进。

---

## 已完成的步骤

### Step 1：Block 模块（Region ↔ BlockId）

- **实现**：`persisting/store/block.py`
  - `BlockId(partition_key, block_id)`，不可变、可哈希
  - `region_to_blocks(region, dims, order_dim, block_tokens, shape)` → `list[BlockId]`
  - `block_to_region(block_id, dims, order_dim, block_tokens, shape)` → Region
- **单测**：`tests/test_block_store.py`
  - `TestBlockId`：eq/hash、放入 set 去重
  - `TestRegionToBlocks`：多块/单块/Point/空范围(裁剪后 lo≥hi)/越界裁剪/prefix 非 Point 报错/dims 与 shape 长度不一致报错
  - `TestBlockToRegion`：roundtrip、block_id 越界、partition_key 长度与 prefix_dims 不一致

### Step 2：BlockStore（L1 + L3，write-through）

- **实现**：`persisting/store/block_store.py`
  - 单节点 L1（默认 NumpyBacking）+ L3（默认 NumpyBacking）
  - `get(region)`：缺块从 L3 拷入 L1 再读
  - `put(region, value)`：写 L1 并写回 L3，标记 Block 在 L1
  - `prefetch(region)` / `wait(region)`：同步将覆盖 region 的块从 L3 拉到 L1
- **单测**：`tests/test_block_store.py`
  - `TestBlockStore::test_get_triggers_fetch_from_l3`
  - `TestBlockStore::test_put_then_get`
  - `TestBlockStore::test_prefetch_and_wait`
  - `TestBlockStore::test_put_writes_through_to_l3`（两 BlockStore 共享同一 L3，put 后另一 store get 可读）

### Step 3：Store 协议与 open(tiered)

- **实现**：`persisting/store/local_tensor.py`（Store 协议）、`persisting/__init__.py`（open backend="tiered"）
  - Handler/TensorNamespace 接受任意 Store；Namespace 提供 `prefetch(key)` / `wait(key)` 当 store 支持时转发
- **单测**：`TestOpenTiered::test_open_tiered_returns_namespace`（open(backend="tiered") 返回 namespace，h.tensor()/h.put() 可用）

**验收**：`uv run pytest tests/test_block_store.py -v` 全部通过；全量 `pytest tests/` 无新增失败。

### Step 4：Rust 层块级文件 I/O（底层接口）

- **实现**：`crates/persisting-core/src/block_io.rs`
  - `block_read(path, offset, len)` → `bytes`：从文件指定偏移读恰好 `len` 字节（不足则返回实际长度）
  - `block_write(path, offset, data)`：向文件指定偏移写 `data`，不存在则创建并 sync
  - 供 L3 块读写、后续 mmap+UFFD 缺页填块使用
- **Python 导出**：`persisting.core.block_read` / `persisting.core.block_write`
- **单测**：`tests/test_block_io.py` — `TestBlockReadWrite`：roundtrip、短文件返回可用字节、指定偏移读写、create 新文件

后续底层接口（如 mmap 预留、UFFD 注册/填页）可继续在 Rust 层实现并暴露给 Python。

### Step 5：BlockMappedBacking 接口与降级实现

- **实现**：`persisting/store/local_tensor.py` — `BlockMappedBacking`
  - Backing 协议：shape、dtype、read(indices)、write(indices, data)
  - 降级：`block_table is None` 时内部使用 NumpyBacking，与普通 Backing 行为一致
  - `block_table` 非 None 时显式 `NotImplementedError`，预留后续 mmap+UFFD
- **导出**：`persisting.store.BlockMappedBacking`
- **单测**：`tests/test_block_store.py`
  - `TestBlockMappedBacking`：block_table 未实现报错、降级 read/write 与 Numpy 一致
  - `TestBlockStoreWithBlockMappedBacking`：L1=BlockMappedBacking 时 get 从 L3 拉取、put 再 get、put 写回 L3 后另一 store 可读

### Step 6：Rust mmap 预留与填块/读块 API

- **实现**：`crates/persisting-core/src/mmap_region.rs`（`#[cfg(unix)]`）
  - `mmap_reserve(length)` → `MmapRegion`：PROT_NONE 预留虚拟区间，Drop 时 munmap
  - `MmapRegion.copy_in(offset, data)`：mprotect 后写入
  - `MmapRegion.copy_out(offset, length)` → bytes：mprotect 后读出
  - 依赖 `libc`，仅 Unix 编译
- **Python 导出**：`persisting.core.mmap_reserve`、`persisting.core.MmapRegion`（非 Unix 时为 None）
- **单测**：`tests/test_block_io.py` — `TestMmapRegion`：reserve + copy_in/out、指定偏移、length=0 报错、越界 copy_out 报错

后续可在此区间注册 UFFD、缺页时用 block_read 填块再 copy_in，或暴露为 buffer 供 numpy 视图。

### Step 7：BlockMappedBacking 接真实 MmapRegion

- **实现**：`persisting/store/local_tensor.py`
  - `BlockMappedBacking(..., use_mmap=False)`：保持降级 NumpyBacking
  - `BlockMappedBacking(..., use_mmap=True)` 且 `mmap_reserve` 可用：用 `MmapRegion(total_bytes)`，read/write 经 `_indices_to_contiguous_byte_range` 转为字节区间后 `copy_out`/`copy_in`；仅支持连续索引，非连续时 `NotImplementedError`
  - 辅助 `_indices_to_contiguous_byte_range(shape, dtype, indices)` → (start_byte, nbytes, result_shape) 或 (None, None, result_shape)
- **单测**：`tests/test_block_store.py`
  - `TestBlockMappedBacking::test_use_mmap_read_write_same_as_numpy`（Unix 下）
  - `TestBlockStoreWithBlockMappedBacking::test_store_with_mmap_l1_put_then_get`（L1=use_mmap=True 时 put/get 与 NumpyBacking 一致）

### Step 8：预取异步化

- **实现**：`persisting/store/block_store.py`
  - `prefetch(region)`：将覆盖 region 的块提交到单线程 `ThreadPoolExecutor`，在后台从 L3 拉入 L1，并登记 `_prefetch_pending[key] = (frozenset(blocks), Event)`
  - `wait(region)`：若该 region 所需块尚未全在 L1，则查找包含这些块的已登记预取并 `event.wait()`；若无匹配则同步 `_ensure_block_in_l1`
  - `get(region)`：行为不变，缺块则同步拉取（未预取时仍可正确读）
  - `_ensure_block_in_l1` / `put` 对 `_blocks_in_l1` 的访问用 `threading.Lock` 保护
- **单测**：`tests/test_block_store.py` — `TestPrefetchAsync`：`test_wait_waits_for_background_prefetch`、`test_get_without_prefetch_sync_fetches`

### Step 9：BlockStore 服务化（RemoteBacking）

- **实现**：`persisting/store/local_tensor.py`
  - `_norm_indices(indices)`：将索引规范为可哈希键（slice→(start,stop)），供 stub/RPC 键用
  - **RemoteBacking**：Backing 实现，构造时传入 `get_block`、可选 `put_block`（签名为 (norm_key) → array 与 (norm_key, array) → None）。read/write 时用 `_norm_indices(indices)` 得到键，委托给 get_block/put_block。L3 在远程时可将 get_block/put_block 实现为 RPC 或 Pulsing Actor 调用
- **导出**：`persisting.store.RemoteBacking`
- **单测**：`tests/test_block_store.py` — `TestBlockStoreWithRemoteBacking`：L3=RemoteBacking(内存 dict stub) 时 get 从 stub 拉块、put 写回 stub 后另一 BlockStore 共享同一 L3 可读

### Step 11：BlockStore 作为 Pulsing Actor

- **实现**：`persisting/store/block_store_actor.py`
  - **region_serialize(region, dims)** → `list[(dim_name, "point"|"range"|"all", value)]`；**region_deserialize(region_ser, dims)** → Region（用 TensorView(dims)[key] 重建）
  - **BlockStoreActor**（`get_block_store_actor_class()` 返回的 @remote 类）：`__init__(dims_ser, shape, order_dim_name, block_tokens, catalog_ser)` 内建 BlockStore；暴露 **get_region(region_ser)**、**put_region(region_ser, data)**、**prefetch_region**、**wait_region**
  - **ActorStore(actor_proxy, dims)**：实现 Store，get/put 将 region 序列化后转发到 actor；提供 **get_async/put_async** 供 async 上下文使用
- **导出**：`persisting.store.get_block_store_actor_class`、`ActorStore`、`region_serialize`、`region_deserialize`
- **单测**：`tests/test_block_store_actor.py` — region 序列化/反序列化 roundtrip；`test_block_store_actor_put_then_get`、`test_actor_store_get_async_put_async`（需 Pulsing 运行时，未安装时跳过）

---

### 主事件循环（Rust 侧，避免 GIL 死锁）

- **设计约束**：主事件循环必须在 Rust 侧（设计 6.2.2）；Python 侧循环在 UFFD 场景下可能导致 GIL 死锁。
- **事件管理与派发**：见设计文档 **6.5.1**（事件类型、优先级、派发表、完成通知、背压与错误）。实现时需统一事件枚举、缺页优先、同 block 去重/合并、有界队列与完成通道。
- **实现**：`crates/persisting-core/src/tiered_loop.rs`
  - **TieredLoop**：单线程消费预取队列（mpsc），`start()` / `submit_prefetch(blocks)` / `stop()`；当前 fill 为占位，后续接入 block_read + copy_in，全在 Rust 内。
  - Python 暴露：`persisting.core.TieredLoop`。
- **单测**：`tests/test_block_store.py` — `TestRustTieredLoop`：start、submit_prefetch、stop；未 start 时 submit 报错。
- **说明**：Python 侧 `persisting/store/tiered_loop.py` 已废弃，不再导出；BlockStore 预取仍用 ThreadPoolExecutor，待 Rust 循环接入填页与完成通知后再可选走 TieredLoop。

---

## 后续步骤（设计文档 Phase 2 起）

| 步骤 | 内容 | 建议单测要点 |
|------|------|--------------|
| UFFD 缺页处理 | **已实现**（Linux）：`crates/persisting-core/src/uffd.rs` — userfaultfd 创建、UFFDIO_REGISTER、读缺页事件、从 L3 文件按 block 读页、UFFDIO_COPY 填页。**macOS 标准模式**（已实现）：`crates/persisting-core/src/page_fault_darwin.rs` — Mach exception port、EXC_MASK_BAD_ACCESS、handler 线程 `mach_msg` 收异常、fault_addr=code[1]（64 位）、fetch_page + mprotect + memcpy、回复 KERN_SUCCESS；**非** SIGSEGV 降级。Python 统一暴露 `start_uffd_handler(base, len, block_size, path)`（Linux 返回 uffd fd，macOS 返回 shutdown pipe fd，关闭即停止 handler）；MmapRegion 暴露 `base_address`。与 TieredLoop 填页逻辑可后续统一（6.5.1）。 | **单测**：`tests/test_page_fault_handler.py` — **TestStartUffdHandlerContract**：返回 fd≥0、关闭 fd 不抛错、block_size 页对齐；**TestPageFaultFillsFromFile**（e2e）：仅 Linux 运行，mmap_reserve + 写 L3 文件 + start_uffd_handler + ctypes 读基址/第二页，校验与文件一致；macOS 上 e2e 暂 skip，契约测试全覆盖。 |
| Step 12 | RDMA 数据面（暂缓） | 与现有 Block 语义兼容的零拷贝读块 |

每完成一步，在本文档中补充「实现文件」与「单测类/用例名」，并确保 `pytest tests/` 全绿。

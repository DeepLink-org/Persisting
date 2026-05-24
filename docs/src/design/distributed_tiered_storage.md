# Persisting 分布式分层存储 — 设计文档

本文档涵盖 Block 模型、虚拟地址映射（mmap + UFFD）、分布式路由（Pulsing）与传输选路。

寻址模型（TTAS）的完整形式化定义见 [分层张量地址空间 (TTAS)](tensor_address_algebra.md)。BlockStore 内部实现细节（Block Table、事件循环、缺页处理）见 [BlockStore 内部设计](blockstore_internals.zh.md)。实现步骤与单测清单见 [分层存储实现步骤](../dev/tiered_storage_implementation_steps.md)。

---

## 1. TTAS 寻址模型（概要）

TTAS（Tiered Tensor Address Space）是 Persisting 数据面的寻址模型：为 KV Cache、PS、轨迹等场景提供统一多维寻址（点查、单维范围、滑窗预取），并通过确定性规范化驱动分区裁剪、批量合并与预取生成。

- **Region**：各维度约束的合取，如 `SESSION=Point("s1"), LAYER=Point(0), HEAD=Point(2), TIME=Range(0,512)`
- **partition_key**：从 prefix_dims 抽取，用于路由
- **order_dim**：排序维度，用于 range_scan 和 block 化

TTAS 不是独立软件层，而是贯穿各层的寻址模型：TensorNamespace 用它做用户下标 → Region，DistributedStore 用它做路由，BlockStore 用它做 Region → Block 列表。

完整定义（Dimension、Address、Constraint、规范化、lowering）见 [TTAS](tensor_address_algebra.md)。

---

## 2. Block：存储与传输的基本单元

### 2.1 为什么需要 Block

系统中存在两种视角：
- **对上（用户/numpy/torch）**：数据是一个连续的多维数组，用 `arr[i, j, lo:hi]` 索引
- **对下（存储/传输/缓存）**：数据分散在不同层级（GPU/CPU/SSD）、不同节点，需要独立管理生命周期

**Block 是桥接这两种视角的基本单元**——底层按 Block 存储（不连续），上层通过 mmap + UFFD 将不连续的 Block 重新映射为连续的虚拟地址空间。

### 2.2 Block 定义

```
Block = (namespace, partition_key, block_id) → 一段页对齐的连续 tensor 数据
```

- **partition_key**：TTAS prefix_dims 各维的 Point 值。如 KV Cache 场景下 `("s1", 0, 2)` = (session, layer, head)。
- **block_id**：order_dim 上的区间编号。如 time 维的 `[0, 64)`、`[64, 128)` 分别为 block 0、block 1。
- **Block 大小**：固定，页对齐。例如 KV Cache 中 64 tokens × 128 head_dim × fp16 = 16KB = 4 个 4KB 页。
- **每个 Block** 独立跟踪所在层级（GPU / CPU / SSD / Remote）和状态（present / evicted / migrating）。

### 2.3 TTAS Region → Block 列表

给定一个 Region：
- prefix_dims 均为 Point → 确定 partition_key
- order_dim 为 Range(lo, hi) → 计算覆盖的 block_id 范围：`[lo // block_tokens, (hi - 1) // block_tokens]`

```python
def region_to_blocks(region, schema, block_tokens):
    partition_key = project_prefix(region, schema.prefix_dims)
    lo, hi = region[schema.order_dim].lo, region[schema.order_dim].hi
    return [
        Block(schema.namespace, partition_key, bid)
        for bid in range(lo // block_tokens, ceil(hi / block_tokens))
    ]
```

### 2.4 具体例子：KV Cache

```
namespace = "kvcache/v1"
dims = (SESSION, LAYER, HEAD, TIME)
order_dim = TIME
partition_dims = (SESSION,)
block_tokens = 64

用户请求：kv["s1", 0, 2, 0:512].tensor()
  ↓ TTAS 解析
Region: SESSION=Point("s1"), LAYER=Point(0), HEAD=Point(2), TIME=Range(0,512)
  ↓ region_to_blocks
partition_key = ("s1", 0, 2)
blocks = [Block("kvcache/v1", ("s1",0,2), 0),   # time [0,64)
          Block("kvcache/v1", ("s1",0,2), 1),   # time [64,128)
          ...
          Block("kvcache/v1", ("s1",0,2), 7)]   # time [448,512)
```

---

## 3. 虚拟地址映射：Block → 连续空间

### 3.1 核心思路

**底层按 Block 存储（不连续、可能分散在不同文件/节点/层级），上层通过 mmap + UFFD 重新映射为连续虚拟地址空间。** 对 numpy/torch 来说，看到的是一个普通的连续数组。

这本质上是在用户态实现的**面向 tensor 的虚拟内存管理器**。

### 3.2 CPU 侧机制

**第一步：预留虚拟地址空间**

```c
void *base = mmap(NULL, total_bytes,
                  PROT_NONE,
                  MAP_ANONYMOUS | MAP_PRIVATE | MAP_NORESERVE,
                  -1, 0);
```

只消耗页表条目，不分配物理内存。可预留 TB 级空间。

**第二步：已有的本地 Block，用 MAP_FIXED 映射到对应位置**

```c
// block 5 在本地 SSD 文件中
mmap(base + 5 * block_size, block_size,
     PROT_READ | PROT_WRITE,
     MAP_FIXED | MAP_SHARED,
     ssd_fd, block_5_file_offset);
```

**第三步：未到位的 Block，注册 UFFD 按需填充**

```c
uffdio_register(uffd, base, total_bytes, UFFDIO_REGISTER_MODE_MISSING);

// handler 线程
while (read(uffd, &msg, sizeof(msg)) > 0) {
    uint64_t fault_addr = msg.arg.pagefault.address;
    int block_id = (fault_addr - base) / block_size;     // O(1) 反向映射
    BlockLocation loc = block_table[block_id];

    switch (loc.tier) {
        case REMOTE: rdma_read(loc.node, loc.mr, loc.offset, buf, block_size); break;
        case SSD:    pread(loc.fd, buf, block_size, loc.file_offset);          break;
    }

    uffdio_copy(uffd, fault_addr & ~(page_size-1), buf, page_size);
}
```

**最终：numpy 看到连续数组**

```python
arr = np.frombuffer(mmap_region, dtype=np.float16).reshape(shape)
data = arr[session_idx, layer_idx, head_idx, 0:512]
# block 在 CPU 内存 → 直接读
# block 在 SSD     → 缺页 → handler 从文件读入 → 自动继续
# block 在远程     → 缺页 → handler RDMA 拉取 → 自动继续
```

### 3.3 GPU 侧机制

GPU 侧用 **CUDA Virtual Memory API**（`cuMemCreate` + `cuMemMap`），思路完全对应：

```c
cuMemAddressReserve(&gpu_base, total_size, 0, 0, 0);          // 预留 GPU VA
cuMemCreate(&block_handle, block_size, &prop, 0);              // 创建物理分配
cuMemMap(gpu_base + bid * block_size, block_size, 0, handle, 0); // 映射到对应位置
cuMemSetAccess(gpu_base + bid * block_size, block_size, &access, 1);
```

### 3.4 对 Backing 协议的影响

Backing 协议不需要改变。`BlockMappedBacking` 只是 Backing 的一个实现——`read(indices)` 和 `NumpyBacking.read(indices)` 写法完全一样，Block 存储、分层、UFFD 全部对上层透明。

---

## 4. 架构总览

### 4.1 三层组件

| 组件 | 职责 |
|------|------|
| **TensorNamespace / Handler** | 用户接口。`open() → kv[key] → h.tensor() / h.put(data)`。使用 TTAS 做下标 → Region。 |
| **DistributedStore** | 分布式路由。使用 TTAS 的 partition_dims 做路由决策，本地请求发给本机 BlockStore，远程请求经 Pulsing 解析端点后发给远程 BlockStore。 |
| **BlockStore**（每节点一个） | 单节点分层存储 + 对外服务。管理本机 L0 GPU / L1 CPU / L3 SSD 的 Block（L2 Remote 的 Block 由远程节点管理，本机仅跟踪），miss 时 fetch。通过 mmap + UFFD 对上层暴露连续虚拟空间。内部设计详见 [BlockStore 内部设计](blockstore_internals.zh.md)。 |

TTAS 是贯穿各层的寻址模型，不是独立的一层。

### 4.2 四级存储层

| 层级 | 介质 | 延迟 | 典型实现 |
|------|------|------|----------|
| **L0 GPU** | GPU 显存 (HBM) | ~1μs | CUDA Virtual Memory API 映射 Block |
| **L1 CPU** | 主机内存 (DRAM) | ~100ns (命中) / ~10μs–1ms (缺页) | mmap + UFFD 映射 Block，缺页时从 L2/L3 拉取 |
| **L2 Remote** | 远程节点内存 | ~2μs (RDMA) / ~1ms (RPC) | 远程 BlockStore，经 Pulsing 发现 |
| **L3 SSD** | 本地 NVMe | ~10μs | 文件 |

### 4.3 整体架构图

```
  Node A                                                    Node B
  ┌─────────────────────────────────────────┐              ┌──────────────────────────────────────────┐
  │  kv["s1", 0, 2, 0:512].tensor()        │              │  kv["s1", 0, 2, 0:512].tensor()         │
  │       │ TTAS: Region → Block 列表        │              │       │                                  │
  │       ▼                                 │              │       ▼                                  │
  │  DistributedStore                       │              │  DistributedStore                        │
  │    partition_key → home=A? 本地          │              │    partition_key → home=A? 远程           │
  │       │                                 │   RDMA/RPC   │       │                                  │
  │       ▼                                 │◄────────────►│       ▼                                  │
  │  BlockStore                             │              │  BlockStore                              │
  │  ┌──────────────────────────────────┐   │              │  ┌──────────────────────────────────┐    │
  │  │  连续虚拟地址空间 (mmap+UFFD)     │   │              │  │  连续虚拟地址空间 (mmap+UFFD)     │    │
  │  │  ┌─────┬─────┬─────┬─────┐      │   │              │  │  ...                             │    │
  │  │  │blk 0│blk 1│blk 2│ ... │      │   │              │  │                                  │    │
  │  │  └──┬──┴──┬──┴──┬──┴─────┘      │   │              │  └──────────────────────────────────┘    │
  │  │     │     │     │               │   │              │  ┌────────┐ ┌────────┐ ┌────────┐        │
  │  │     ▼     ▼     ▼               │   │              │  │ L0 GPU │ │ L1 CPU │ │ L3 SSD │        │
  │  │  MAP_FIXED  UFFD缺页  UFFD缺页    │   │              │  └────────┘ └────────┘ └────────┘        │
  │  └──────────────────────────────────┘   │              └──────────────────────────────────────────┘
  │  ┌────────┐ ┌────────┐ ┌────────┐       │
  │  │ L0 GPU │ │ L1 CPU │ │ L3 SSD │       │
  │  └────────┘ └────────┘ └────────┘       │
  └─────────────────────────────────────────┘
```

- **控制面**：Pulsing 做节点发现与端点解析；UFFD 捕获 CPU 缺页；GPU 事件通过 eventfd 接入。统一由 epoll 事件循环处理。
- **数据面**：Block 传输由对应引擎执行（memcpy / PCIe DMA / NVLink / RDMA / io_uring）。

---

## 5. 端到端 Trace：KV Cache 读取

以 `kv["s1", 0, 2, 0:512].tensor()` 为例：

```
1. TensorNamespace.__getitem__("s1", 0, 2, 0:512)
   → Region{SESSION=Point("s1"), LAYER=Point(0), HEAD=Point(2), TIME=Range(0,512)}

2. Handler.tensor() → DistributedStore.get(region)

3. DistributedStore.get(region)
   → partition_key = ("s1",)
   → home_node = route("kvcache/v1", ("s1",))
   → 本地? 调本机 BlockStore : 远程 RPC/RDMA

4. BlockStore.get(region)
   → blocks = region_to_blocks(region)  [block 0..7]
   → TensorLayout.region_to_index(region) → numpy 索引
   → self._arr[indices].copy()   [访问连续虚拟空间]

5. 虚拟地址访问（底层透明）
   → block 0 已在 L1 CPU → 直接读
   → block 3 不在本机 → 缺页 → handler RDMA 拉取 → UFFDIO_COPY → 恢复
   → block 5 不在内存（在本机 SSD） → 缺页 → handler SSD pread → 填页 → 恢复

6. numpy ndarray 返回给用户
```

步骤 4 的 `self._arr[indices].copy()` 与 `NumpyBacking.read()` 写法完全相同。Block 存储、分层、UFFD 全部对用户代码透明。

---

## 6. 传输路径

| 路径 | 场景 | Block 传输方向 |
|------|------|----------------|
| **PCIe** | 本机 GPU ↔ CPU | L0↔L1 |
| **NVLink** | 本机多 GPU | L0↔L0 |
| **RDMA** | 跨节点 | L1↔L2 |
| **GPUDirect RDMA** | 跨节点 GPU 直通 | L0↔L2 |
| **io_uring / pread** | 本机 SSD | L1↔L3 |
| **RPC / TCP** | 跨节点降级 | 无 RDMA 时的数据面降级 |

传输接口统一为：

```python
class Transport(Protocol):
    def fetch_block(self, src: BlockLocation, dst: BlockLocation,
                    size: int, callback: Callable) -> None: ...
```

路由逻辑：给定 src 和 dst 的 location type，选对应 transport 实现。同机优先 PCIe/NVLink，跨节点优先 RDMA/GDR。

---

## 7. 与 Pulsing 的配合

### 7.1 Placement

- Pulsing 的集群视图 + TTAS 的 partition_dims → "某 partition_key 的 home 节点"
- DistributedStore 据此决定：访问本机 BlockStore 还是远程

### 7.2 演进路径

| 阶段 | Pulsing 的角色 |
|------|----------------|
| **Phase 3–4** | BlockStore 作为 Pulsing Actor（`@remote`），远程访问 = actor 消息 |
| **Phase 5** | Pulsing 只做控制面（发现 BlockStore 端点），数据面切到 RDMA |

### 7.3 一致性

- v1：单写多读或会话内一致
- 后续：可在 TTAS 上加版本或在 BlockStore 上加租约

---

## 8. 小结

- **Block 是基本单元**：存储、传输、缓存、驱逐全部以 Block 为粒度。Block 有 TTAS 地址，大小页对齐。
- **Block 是物理现实，连续空间是承诺**：底层按 Block 存储（不连续），上层通过 mmap + UFFD（CPU）或 CUDA Virtual Memory API（GPU）将不连续 Block 映射为连续虚拟空间。对 numpy/torch 透明。
- **跨平台缺页机制**：Linux 用 userfaultfd，macOS 用 Mach exception handler，开发阶段可用 mprotect+SIGSEGV 降级。BlockStore 内部抽象为 `PageFaultHandler` 接口。
- **Backing 协议不变**：`BlockMappedBacking.read(indices)` 和 `NumpyBacking.read(indices)` 写法完全一样。
- **TTAS 贯穿各层**：用户接口（Region）、分布式路由（partition_key）、Block 寻址（region_to_blocks）、数组索引（region_to_index）都使用 TTAS。
- **三层组件**：TensorNamespace（用户接口）→ DistributedStore（路由）→ BlockStore（单节点分层存储）。
- **UFFD 是优化不是必须**：Phase 1 用显式 miss/fetch，Phase 2 加 UFFD 做透明化。
- **控制面与数据面分离**：Pulsing 做控制面（发现、路由），Block 传输走 RDMA/PCIe/NVLink 做数据面。

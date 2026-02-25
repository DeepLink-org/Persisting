# Persisting 分布式分层存储 — 综合设计文档

**文档用途**：供技术团队 review 与讨论的统一设计稿，涵盖寻址模型（TAA）、Block 存储、虚拟地址映射（mmap + UFFD）、分布式路由（Pulsing）与传输选路。

---

## 1. TAA 设计概要（寻址模型）

TAA（Tensor Address Algebra）是 Persisting 数据面的**寻址模型**：只做地址语义，无 I/O、无存储。详细形式化见 [Tensor Address Algebra](tensor_address_algebra.md)。

### 1.1 角色与边界

- **解决什么**：为 KV Cache、PS/Optimizer state、轨迹等场景提供统一多维寻址（点查、单维范围、滑窗预取），并通过确定性规范化与 lowering 优化分区裁剪、批量合并与预取生成。
- **不做什么**：不做通用查询优化器/执行引擎、不做物理连续字节假设、不做分层 placement 在线决策。
- **用户可见**：`kv["s1", 0, 2, 0:512].tensor()`。TAA 在底层驱动寻址与路由，用户无需直接操作。

### 1.2 数据模型

| 概念 | 含义 |
|------|------|
| **Dimension** | 维度：`name` + `kind`（`int` / `str` / `bytes`） |
| **Address** | 点地址：对所有维度各给一个值，无 wildcard |
| **Constraint** | 单维约束：**Point**(v)、**Range**(lo, hi)（仅 int）、**SetC**({v1,v2,...}) |
| **Region** | 各维度约束的合取（AND），表示地址集合 |

### 1.3 Key Schema

每个命名空间声明：

- **维度全集** `Dims = (d0, d1, ..., dn)`
- **排序维度** `order_dim`（可选；无则不支持 range_scan），必须位于 KeyOrder 最后一位
- **分区维度** `partition_dims ⊆ prefix_dims`，用于跨节点路由

### 1.4 核心操作

- **project_prefix(addr_or_region, dims)**：抽取分区键，用于路由
- **select(r, d, c)**：增加维度约束，构造查询
- **shift_range(r, order_dim, delta)**：range 平移，用于预取窗口

### 1.5 规范化与 Normal Form

- **NF-PointQuery**：所有维度均为 Point → `mget([key])`
- **NF-RangeQuery**：order_dim 为 Range，其余为 Point → `range_scan(prefix, lo, hi)`
- 非 NF 的 Region 须在上层拆解后再提交

### 1.6 TAA 不是一个"层"

TAA 是贯穿各层的寻址模型，不是独立的软件层：
- **TensorNamespace / Handler** 使用 TAA 做用户下标 → Region
- **DistributedStore** 使用 TAA 做 partition_key 路由
- **BlockStore** 使用 TAA 做 Region → Block 列表
- **TensorLayout** 使用 TAA 做 Region → 数组索引

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

- **partition_key**：TAA prefix_dims 各维的 Point 值。如 KV Cache 场景下 `("s1", 0, 2)` = (session, layer, head)。
- **block_id**：order_dim 上的区间编号。如 time 维的 `[0, 64)`、`[64, 128)` 分别为 block 0、block 1。
- **Block 大小**：固定，页对齐。例如 KV Cache 中 64 tokens × 128 head_dim × fp16 = 16KB = 4 个 4KB 页。
- **每个 Block** 独立跟踪所在层级（GPU / CPU / SSD / Remote）和状态（present / evicted / migrating）。

### 2.3 TAA Region → Block 列表

给定一个 Region：
- prefix_dims 均为 Point → 确定 partition_key
- order_dim 为 Range(lo, hi) → 计算覆盖的 block_id 范围：`[lo // block_tokens, (hi - 1) // block_tokens]`
- 结果：该 Region 覆盖的 Block 列表（每个 Block 有确定的 TAA 地址）

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
  ↓ TAA 解析
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

文件中的 block 数据被"贴"到连续虚拟空间的正确位置。

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
# 用户线程完全无感
```

### 3.3 GPU 侧机制

GPU 侧用 **CUDA Virtual Memory API**（`cuMemCreate` + `cuMemMap`），思路完全对应：

```c
cuMemAddressReserve(&gpu_base, total_size, 0, 0, 0);          // 预留 GPU VA
cuMemCreate(&block_handle, block_size, &prop, 0);              // 创建物理分配
cuMemMap(gpu_base + bid * block_size, block_size, 0, handle, 0); // 映射到对应位置
cuMemSetAccess(gpu_base + bid * block_size, block_size, &access, 1);
```

GPU 和 CPU 两侧都可以做"不连续 Block → 连续虚拟空间"的映射。

### 3.4 对 Backing 协议的影响

**Backing 协议不需要改变。** `BlockMappedBacking` 只是 Backing 的一个实现：

```python
class BlockMappedBacking:
    """将不连续 Block 映射为连续虚拟地址空间。"""

    def __init__(self, shape, dtype, block_size, block_table):
        total = int(np.prod(shape)) * np.dtype(dtype).itemsize
        self._region = mmap_reserve(total)
        self._uffd = setup_userfaultfd(self._region, total)
        self._block_size = block_size
        self._block_table = block_table

        for bid, loc in block_table.items():
            if loc.tier == LOCAL_SSD:
                mmap_fixed(self._region, bid * block_size, loc.fd, loc.offset)

        self._arr = np.frombuffer(self._region, dtype=dtype).reshape(shape)
        start_fault_handler(self._uffd, self._block_table, self._block_size)

    def read(self, indices):
        return self._arr[indices].copy()    # 和 NumpyBacking 完全一样

    def write(self, indices, data):
        self._arr[indices] = data           # 和 NumpyBacking 完全一样
```

`TensorLayout.region_to_index` 产生的 numpy 索引直接作用于连续虚拟视图。上层代码无需感知 Block 的存在。

---

## 4. 架构总览

### 4.1 三层组件

| 组件 | 职责 |
|------|------|
| **TensorNamespace / Handler** | 用户接口。`open() → kv[key] → h.tensor() / h.put(data)`。使用 TAA 做下标 → Region。 |
| **DistributedStore** | 分布式路由。使用 TAA 的 partition_dims 做路由决策，本地请求发给本机 BlockStore，远程请求经 Pulsing 解析端点后发给远程 BlockStore。 |
| **BlockStore**（每节点一个） | 单节点分层存储 + 对外服务。管理本机 L0/L1/L3 的 Block，跟踪每 Block 所在层级，miss 时 fetch。通过 mmap + UFFD 对上层暴露连续虚拟空间。可被远程节点通过 RPC/RDMA 访问。 |

TAA 是贯穿各层的寻址模型，不是独立的一层。

### 4.2 四级存储层

| 层级 | 介质 | 延迟 | 典型实现 |
|------|------|------|----------|
| **L0 GPU** | GPU 显存 (HBM) | ~1μs | CUDA Virtual Memory API 映射 Block |
| **L1 CPU** | 主机内存 (DRAM) | ~100ns (命中) / ~10μs–1ms (缺页) | mmap + UFFD 映射 Block，缺页时从 L2/L3 拉取 |
| **L2 Remote** | 远程节点内存 | ~2μs (RDMA) / ~1ms (RPC) | 远程 BlockStore，经 Pulsing 发现 |
| **L3 SSD** | 本地 NVMe | ~10μs | MmapBacking / SafetensorsBacking / 文件 |

### 4.3 整体架构图

```
  Node A                                                    Node B
  ┌─────────────────────────────────────────┐              ┌──────────────────────────────────────────┐
  │  kv["s1", 0, 2, 0:512].tensor()        │              │  kv["s1", 0, 2, 0:512].tensor()         │
  │       │ TAA: Region → Block 列表         │              │       │                                  │
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
  │  │  (本地SSD) (远程B)   (远程C)     │   │              └──────────────────────────────────────────┘
  │  └──────────────────────────────────┘   │
  │  ┌────────┐ ┌────────┐ ┌────────┐       │
  │  │ L0 GPU │ │ L1 CPU │ │ L3 SSD │       │
  │  └────────┘ └────────┘ └────────┘       │
  └─────────────────────────────────────────┘
```

- **控制面**：Pulsing 做节点发现与端点解析；UFFD 捕获 CPU 缺页；GPU 事件通过 eventfd 接入。统一由 epoll 事件循环处理。
- **数据面**：Block 传输由对应引擎执行（memcpy / PCIe DMA / NVLink / RDMA / io_uring）。

---

## 5. 端到端 Trace：KV Cache 读取

以 `kv["s1", 0, 2, 0:512].tensor()` 为例，完整调用链：

```
1. TensorNamespace.__getitem__("s1", 0, 2, 0:512)
   → TensorView 解析为 Region{SESSION=Point("s1"), LAYER=Point(0), HEAD=Point(2), TIME=Range(0,512)}
   → 返回 Handler(region)

2. Handler.tensor()
   → DistributedStore.get(region)

3. DistributedStore.get(region)
   → partition_key = project_prefix(region, (SESSION,)) = ("s1",)
   → home_node = route("kvcache/v1", ("s1",))  [Pulsing 解析]
   → home_node == 本机? 调本机 BlockStore : RPC/RDMA 调远程 BlockStore

4. BlockStore.get(region)
   → blocks = region_to_blocks(region)  [block 0..7, 每个 64 tokens]
   → TensorLayout.region_to_index(region) → numpy 索引
   → self._arr[indices].copy()   [访问连续虚拟空间]

5. 虚拟地址访问（底层透明）
   → block 0 已在 L1 CPU (MAP_FIXED 映射自 SSD 文件) → 直接读，无开销
   → block 3 不在本机 → 触发缺页（Linux: UFFD / macOS: Mach exception / 降级: SIGSEGV）：
       a. handler 收到 fault_addr
       b. block_id = (fault_addr - base) / block_size = 3
       c. block_table[3] = {tier: REMOTE, node: "B", mr: ..., offset: ...}
       d. RDMA read 从 Node B 拉取 block 3 数据
       e. 填入 fault 地址（Linux: UFFDIO_COPY / macOS: memcpy+mprotect+reply）
       f. 用户线程自动恢复，读取完成
   → block 5 不在本机 → 触发缺页：
       handler 从本地 SSD pread → 填页 → 用户线程恢复

6. numpy ndarray 返回给用户
```

关键点：步骤 4 的 `self._arr[indices].copy()` 与 `NumpyBacking.read()` **写法完全相同**。Block 存储、分层、UFFD 全部对用户代码透明。

---

## 6. BlockStore 内部设计

### 6.1 Block Table

BlockStore 维护 `block_table: dict[BlockId, BlockEntry]`：

```python
@dataclass
class BlockEntry:
    tier: Literal["gpu", "cpu", "remote", "ssd"]
    state: Literal["present", "evicted", "migrating"]
    # tier-specific location
    gpu_ptr: int | None         # L0: CUDA device pointer
    cpu_mapped: bool            # L1: 是否已 MAP_FIXED 映射
    remote_node: str | None     # L2: 远程节点 ID
    remote_mr: Any | None       # L2: RDMA MR 信息
    ssd_fd: int | None          # L3: 文件 fd
    ssd_offset: int | None      # L3: 文件偏移
```

### 6.2 缺页处理：跨平台机制

缺页填充是 BlockStore 的**内部实现细节**，不是独立的系统组件。核心逻辑相同（fault_addr → block_id → 从下层拉取 → 填页 → 恢复用户线程），但底层机制因平台而异：

| | Linux userfaultfd | macOS Mach Exception | 降级：mprotect + SIGSEGV |
|---|---|---|---|
| **原理** | 内核将缺页投递到 fd，handler 线程 read fd 后用 UFFDIO_COPY 原子填页 | 缺页触发 EXC_BAD_ACCESS，内核通过 Mach port 投递到 handler 线程，handler 填数据后回复消息 | mprotect 设 PROT_NONE，访问触发 SIGSEGV/SIGBUS，signal handler 内填数据后改 mprotect |
| **handler 位置** | 独立线程（read fd） | 独立线程（Mach port receive） | 信号上下文（同线程，栈受限） |
| **填页方式** | `UFFDIO_COPY`（内核原子建立映射） | `memcpy` + `mprotect`（两步） | 先 `mprotect(RW)` 再 `memcpy`（两步） |
| **并发安全** | 内核保证（同一页的多个 fault 合并） | 需自行处理 | 需自行处理 |
| **适合场景** | 生产部署（Linux） | 生产部署（macOS） | 开发原型（跨平台） |

**Linux（userfaultfd）**：

```c
// handler 线程
while (read(uffd, &msg, sizeof(msg)) > 0) {
    uint64_t fault_addr = msg.arg.pagefault.address;
    int block_id = (fault_addr - base) / block_size;
    fetch_block(block_id, buffer);
    uffdio_copy(uffd, fault_addr & ~(page_size-1), buffer, page_size);
    // 内核自动唤醒用户线程
}
```

**macOS（Mach Exception Handler）**：

```c
// 注册
mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &exc_port);
task_set_exception_ports(mach_task_self(), EXC_MASK_BAD_ACCESS,
                         exc_port, EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES, 0);

// handler 线程
while (1) {
    mach_msg_receive(exc_port, ...);
    uint64_t fault_addr = msg.code[1];
    int block_id = (fault_addr - base) / block_size;
    void *page = (void *)(fault_addr & ~(page_size-1));
    fetch_block(block_id, page);
    mprotect(page, page_size, PROT_READ | PROT_WRITE);
    mach_msg_send(reply);  // KERN_SUCCESS，内核恢复用户线程
}
```

**降级（mprotect + SIGSEGV，跨平台）**：

```c
// 初始化：mmap(PROT_NONE) + sigaction(SIGSEGV/SIGBUS)
void fault_handler(int sig, siginfo_t *info, void *ctx) {
    void *fault_addr = info->si_addr;
    int block_id = ((uintptr_t)fault_addr - (uintptr_t)base) / block_size;
    void *page = (void *)((uintptr_t)fault_addr & ~(page_size-1));
    mprotect(page, page_size, PROT_READ | PROT_WRITE);
    fetch_block(block_id, page);
    // signal handler 返回后内核自动重试原始访问
}
```

BlockStore 内部抽象为平台无关的接口：

```python
class PageFaultHandler(Protocol):
    """用户态缺页处理，底层按平台选实现。"""
    def register(self, base: int, size: int) -> None: ...
    def on_fault(self, fault_addr: int) -> None: ...

class LinuxUffdHandler(PageFaultHandler): ...       # userfaultfd + UFFDIO_COPY
class DarwinMachHandler(PageFaultHandler): ...      # Mach exception port
class FallbackSignalHandler(PageFaultHandler): ...  # mprotect + SIGSEGV
```

**对上层的效果相同**：用户线程的内存访问自动挂起、填页后自动恢复。`BlockMappedBacking.read(indices)` 在所有平台上都是 `self._arr[indices].copy()`，无需感知底层差异。

开发阶段可在 macOS 上用 Mach Exception 或 mprotect+SIGSEGV 做原型验证，生产部署用 Linux userfaultfd。

#### 6.2.1 调度逻辑归属：以 UFFD handler 为核心

**核心调度逻辑应实现在 userfaultfd 的处理逻辑（或与之共享的同一事件循环）里**，避免「缺页路径」与「预取路径」两套决策分叉。

#### 6.2.2 事件循环必须在 Rust 侧（GIL 死锁约束）

**主事件循环必须在 Rust 侧实现，不得放在 Python 侧。** 原因：UFFD 缺页时，触缺页的线程往往在 numpy/torch 等 C 扩展中仍持有 GIL 并被内核挂起；若填页逻辑在 Python 中执行（handler 线程需调 Python 做选层/拉取/填页），handler 会争用 GIL，导致死锁或长时间阻塞。因此：

- **循环与填页热路径均在 Rust**：epoll/kqueue、预取队列消费、UFFD 读 fd 与 UFFDIO_COPY、block_read/填页，全部在 Rust 内完成，不回调 Python。
- **Python 仅做提交与等待**：`prefetch(region)` 通过 FFI 将 block 列表送入 Rust 侧队列；`wait(region)` 等待 Rust 侧完成事件（如条件变量/信号量）；填页所需配置（L3 路径、block 布局、mmap 基址）在启动循环时一次性从 Python 传入 Rust。

- **调度集中**：选层（从 L3 SSD 拉 vs 从 L2 远程拉）、填页顺序（如缺页触发的 block 优先）、批量化（多 fault 合并为一次 I/O）、驱逐与预取决策，均应由 **UFFD handler 所在线程/事件循环** 统一执行。`fetch_block(block_id)` 不是简单「查表拉取」，而是经过同一套调度逻辑：查 block_table、选源、排队、拉取、填页、更新状态。
- **预取与缺页同路径**：`prefetch(region)` 不单独维护一套「预取线程 + 拉取逻辑」，而是向**同一调度器**提交「待填块」任务（与缺页触发的任务同队列或同优先级策略）。这样 handler 是唯一决策点，预取等价于「提前触发的填块请求」。
- **事件循环**：6.5 节中的 epoll/kqueue 事件循环应同时监听 UFFD fd（缺页）与预取队列/定时器；handler 处理一次 fault 时，可顺带根据策略触发相邻 block 的预取或批量拉取，逻辑仍集中在同一处。

### 6.3 预取与同步保证

**预取是「提前向 UFFD 侧调度器提交填块任务」**，与真实缺页走同一套 handler 逻辑；理想情况下预取先完成，访问时不触发缺页。

1. **预取**：`prefetch(region)` 将覆盖的 Block 列表提交给与 UFFD handler 共享的调度器，由同一事件循环执行拉取与填页（RDMA read / SSD pread → UFFDIO_COPY 或 MAP_FIXED），并更新 block_table。
2. **UFFD 保底**：若未预取或预取未完成，计算流访问时触发缺页，仍由同一 handler 按相同调度逻辑拉取并填页，保证正确性。
3. **显式等待**：`wait(region)` 阻塞直到该 region 对应块在调度器侧已完成填页，避免在 `get()` 时不可控地阻塞在缺页上。

```python
# 1. 发起预取（提交给 handler 侧调度器）
store.prefetch(region)

# 2. 显式等待数据就绪（可选）
store.wait(region)

# 3. 访问数据（此时应已在 L1，不触发缺页）
data = store.get(region)
```

### 6.4 驱逐与预取实现要点
- **驱逐**：由事件循环内的定时/周期任务或内存压力回调决策；对选中 Block 执行 `MADV_DONTNEED`、更新 block_table 为 evicted；下次访问触发缺页，由 handler 重新拉取。驱逐逻辑可与 handler 同处一事件循环，便于统一调度。
- **预取实现**：`prefetch(region)` → 将 Block 列表入队到 **handler 所用调度器** → handler 所在事件循环按策略拉取并执行 UFFDIO_COPY（或 MAP_FIXED）填页 → 标记 block 为 present。不另起「预取专用」线程或重复一套选层/拉取代码。

### 6.5 事件循环

**调度与预取均由同一事件循环处理**（见 6.2.1）。管理线程用 **epoll**（Linux）或 **kqueue**（macOS）统一监听：

**实现状态**：**主事件循环必须在 Rust 侧实现**（见 6.2.2，避免 GIL 死锁）。Rust 侧骨架（预取队列 + 循环线程）已/将实现在 `persisting-core`；Python 侧仅通过 FFI 提交预取与等待完成。UFFD fd 后续在 Rust 循环中注册，填页（block_read + copy_in）全在 Rust 内完成。

| 事件源 | 事件含义 | 处理 | 平台 |
|--------|---------|------|------|
| UFFD fd | CPU 缺页 | 进入调度逻辑：查 block_table、选层、拉取、UFFDIO_COPY、更新状态 | Linux |
| 预取队列 | prefetch(region) 提交的待填块 | 与缺页同调度逻辑：选层、拉取、填页 | 通用 |
| Mach port | CPU 缺页 (EXC_BAD_ACCESS) | 同上，填页后 mach_msg_send 回复 | macOS |
| GPU eventfd | GPU 传输完成 | 更新 block_table（"migrating"→"present"） | 通用 |
| RDMA 完成通道 | 远程拉取完成 | 填页 + 更新 block_table | Linux (生产) |
| io_uring | SSD 读写完成 | 同上 | Linux |
| 定时器 fd / kqueue timer | 周期任务 | 冷热扫描、驱逐决策 | 通用 |

macOS 上 Mach port 可通过 `dispatch_source` 或 `kevent(EVFILT_MACHPORT)` 接入 kqueue 事件循环，与其他事件源统一。

### 6.5.1 事件管理与派发设计

为保证大量事件进入循环后仍能正确派发并触发逻辑，需对事件的**类型、优先级、顺序、完成通知与背压**做统一约定。以下为 Rust 侧循环实现时的设计约束。

#### 事件类型与来源

| 事件类型 | 来源 | 载荷 | 触发逻辑 |
|----------|------|------|----------|
| **FillRequest** | 预取队列（Python submit_prefetch） | block 列表 | 选层 → 拉取 → 填页 → 可选完成通知 |
| **PageFault** | UFFD fd / Mach port | fault_addr → block_id | 同上，且需在填页后唤醒触缺页线程（UFFDIO_COPY 等由内核完成） |
| **IoComplete** | io_uring / RDMA 完成通道 / GPU eventfd | (op_id, 成功/失败) | 填页到目标 VA 或更新 block_table，可选唤醒等待者 |
| **Timer** | 定时器 fd / kqueue timer | 周期或单次 | 驱逐扫描、预取窗口推进等 |

同一「填块」动作可能由 FillRequest 或 PageFault 触发，后端逻辑应复用同一套选层 + 拉取 + 填页（见 6.2.1）。

#### 事件表示与优先级

- **统一事件枚举**：循环内部将上述来源收敛为一种或多种内部事件类型（如 `Fill { blocks }`、`Fault { block_id, fault_ctx }`、`IoComplete { .. }`、`Timer { kind }`），便于单一路径派发。
- **优先级**：建议 **PageFault 优先于 FillRequest**，避免触缺页的计算线程长时间挂起；IoComplete 与 Timer 可按需插入（如 IoComplete 及时处理以释放缓冲）。实现上可用多队列（高/普通）或单队列 + 优先级标签，epoll_wait 返回后按优先级顺序消费。
- **顺序保证**：同一 block 的多次请求（如先 prefetch 再 fault）应能**去重或合并**（如同一 block 只填一次，后续请求发现已 present 则直接完成），避免重复 I/O。

#### 派发与 handler 绑定

- **派发表**：循环内维护「事件类型 → 处理函数」的映射；处理函数为 Rust 内纯函数（选层、block_read、copy_in、UFFDIO_COPY、更新 block_table），不回调 Python，避免 GIL。
- **FillRequest / PageFault** → 同一 `fill_blocks` 入口：输入 block_id(s)，输出为「成功/失败」；失败时可选重试或上报（通过完成通道回 Python）。
- **IoComplete** → 根据 op_id 找到对应请求，将结果写入目标 VA 或更新状态，并触发该请求上的完成通知（若有）。
- **Timer** → 调用驱逐/预取策略函数，不阻塞其他事件。

#### 完成通知与等待

- **预取完成**：Python 侧 `wait(region)` 需阻塞直到该 region 对应 block 均填页完成。Rust 侧可在每次 fill 完成后，将「已完成的 block 集合」通过**完成通道**（如另一 mpsc）或**条件变量 + 共享状态**通知 Python；或由 Python 轮询「block 是否在 L1」（需 Rust 暴露查询接口）。设计上建议：Rust 循环在完成一批 fill 后，向完成队列写入 (request_id 或 block set)；Python 侧 wait() 在提交预取时登记 request_id 与 Event，由一专用线程或 async 侧消费完成队列并 set() 对应 Event。
- **缺页完成**：由内核/UFFD 机制保证，填页后触缺页线程自动恢复，无需应用层再通知。

#### 批量化与合并

- **同 block 合并**：若队列中多次出现同一 block_id，可合并为一次 fill，减少 I/O。
- **相邻 block 批量读**：若连续多个 block 来自同一 L3 文件，可合并为一次 read(offset, len) 再按 block 切分填页，降低 syscall 与缓存未命中。
- **批量大小上限**：单次派发处理的 block 数可设上限，避免单次回调过长阻塞其他事件源（如 UFFD）。

#### 背压与错误

- **队列深度**：预取队列与完成队列建议有界；超过时 Python submit_prefetch 可阻塞或返回「队列满」，避免无界堆积。
- **错误处理**：fill 失败（如 L3 read 错误、UFFDIO_COPY 失败）应在循环内记录并标记该 block 为失败；可选重试或通过完成通道向 Python 返回错误，由上层决定是否重试或降级。
- **循环健康**：长时间无 epoll_wait 返回或某 fd 始终可读可能表示异常；可加简单超时或看门狗（与实现细节绑定，此处仅作提醒）。

#### 小结

- **事件类型**：FillRequest、PageFault、IoComplete、Timer 四类，统一枚举与派发表。
- **优先级**：缺页优先，预取与 I/O 完成次之，定时器再次；同 block 去重/合并。
- **派发**：单线程循环内按优先级取事件 → 查表调用对应 Rust handler → 不回调 Python 热路径。
- **完成**：预取通过完成通道/共享状态通知 Python wait()；缺页由内核唤醒。
- **背压与错误**：有界队列、失败标记与可选重试、完成通道可带错误码。

### 6.6 实现注意

- **防递归缺页**：handler 线程的栈和堆不得落在 UFFD 管辖区间内。建议 `mlockall` 或从非托管区分配。
- **DMA 缓冲区**：RDMA、GPU 拷贝的中间缓冲须为固定内存（pinned / `cudaHostAlloc`）。
- **一致性**：GPU 与 CPU 同时访问同一 Block 时需协调（如先迁回再访问）。

---

## 7. 传输路径

| 路径 | 场景 | Block 传输方向 |
|------|------|----------------|
| **PCIe** | 本机 GPU ↔ CPU | L0↔L1：Block 在 GPU 和 CPU 层之间迁移 |
| **NVLink** | 本机多 GPU | L0↔L0：Block 在不同 GPU 之间迁移 |
| **RDMA** | 跨节点 | L1↔L2：从远程 BlockStore 拉取 Block |
| **GPUDirect RDMA** | 跨节点 GPU 直通 | L0↔L2：远程内存直接写入本机 GPU，跳过 CPU |
| **io_uring / pread** | 本机 SSD | L1↔L3：Block 从 SSD 读入 CPU |
| **RPC / TCP** | 跨节点降级 | 无 RDMA 时的数据面降级路径 |

传输接口统一为：

```python
class Transport(Protocol):
    def fetch_block(self, src: BlockLocation, dst: BlockLocation,
                    size: int, callback: Callable) -> None: ...
```

路由逻辑：给定 src 和 dst 的 location type，选对应 transport 实现。同机优先 PCIe/NVLink，跨节点优先 RDMA/GDR。

---

## 8. 与 Pulsing 的配合

### 8.1 Placement

- Pulsing 的集群视图 + TAA 的 partition_dims → "某 partition_key 的 home 节点"
- DistributedStore 据此决定：访问本机 BlockStore 还是远程

### 8.2 演进路径

| 阶段 | Pulsing 的角色 |
|------|----------------|
| **Phase 3–4** | BlockStore 作为 Pulsing Actor（`@remote`），远程访问 = actor 消息。简单但数据面走 Pulsing HTTP/2，吞吐有限。 |
| **Phase 5** | Pulsing 只做控制面（发现 BlockStore 端点），数据面切到 RDMA。Pulsing 解析出 (node_ip, rdma_port)，Block 传输走 RDMA。 |

### 8.3 一致性

- v1：单写多读或会话内一致
- 后续：可在 TAA 上加版本或在 BlockStore 上加租约

---

## 9. 实现分阶段

| 阶段 | 内容 | 验收 |
|------|------|------|
| **Phase 1** | **BlockStore 单机版**：Block 定义 + block_table + 两层（L1 NumpyBacking + L3 MmapBacking），显式 miss/fetch，无 UFFD。Handler 面向 BlockStore 接口。 | KV Cache 场景下 `tensor()` / `put()` 端到端跑通 |
| **Phase 2** | **BlockMappedBacking**：L1 用 mmap + 用户态缺页处理管理虚拟区间，Block 映射为连续空间。缺页时从 L3 按页读入。Linux 用 userfaultfd，macOS 用 Mach exception handler，开发阶段可用 mprotect+SIGSEGV 降级。 | 缺页填充正确；Backing 协议不变 |
| **Phase 3** | **BlockStore 服务化**：BlockStore 作为 Pulsing Actor 暴露 get_block/put_block；本地调用直接走，远程走 actor 消息。 | 两节点跨节点读取跑通 |
| **Phase 4** | **预取**：`tensor()` 调用时用 `shift_range` 生成预取 Block 列表，后台填充。 | 顺序读场景缺页率显著下降 |
| **Phase 5** | **RDMA 数据面**：Block 传输切到 RDMA（或 GDR），Pulsing 退为控制面。 | 跨节点 Block 拉取延迟 < 10μs (RDMA) |

---

## 10. 小结

- **Block 是基本单元**：存储、传输、缓存、驱逐全部以 Block 为粒度。Block 有 TAA 地址，大小页对齐。
- **Block 是物理现实，连续空间是承诺**：底层按 Block 存储（不连续），上层通过 mmap + 用户态缺页处理（CPU）或 CUDA Virtual Memory API（GPU）将不连续 Block 映射为连续虚拟空间。对 numpy/torch 透明。
- **跨平台缺页机制**：Linux 用 userfaultfd（UFFDIO_COPY 原子填页），macOS 用 Mach exception handler（EXC_BAD_ACCESS + Mach port），开发阶段可用 mprotect+SIGSEGV 降级。BlockStore 内部抽象为 `PageFaultHandler` 接口，按平台选实现。
- **Backing 协议不变**：`BlockMappedBacking.read(indices)` 和 `NumpyBacking.read(indices)` 写法完全一样——都是 `self._arr[indices].copy()`。缺页处理对用户线程透明，所有平台效果相同。
- **TAA 贯穿各层**：用户接口（Region）、分布式路由（partition_key）、Block 寻址（region_to_blocks）、数组索引（region_to_index）都使用 TAA，但 TAA 本身不是一个"层"。
- **三层组件**：TensorNamespace（用户接口）→ DistributedStore（路由）→ BlockStore（单节点分层存储 + 服务化），清晰分工。
- **UFFD 是优化不是必须**：Phase 1 用显式 miss/fetch，Phase 2 加 UFFD 做透明化。
- **控制面与数据面分离**：Pulsing 做控制面（发现、路由），Block 传输走 RDMA/PCIe/NVLink 做数据面。

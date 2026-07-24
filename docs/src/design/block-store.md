# BlockStore 内部设计

本文档描述 BlockStore 单节点内部的实现细节：Block Table、跨平台缺页处理、事件循环、预取与驱逐调度。

BlockStore 的架构定位与对外接口见 [分布式分层存储](distributed_tiered_storage.md)。

---

## 1. Block Table

BlockStore 维护 `block_table: dict[BlockId, BlockEntry]`：

```python
@dataclass
class BlockEntry:
    tier: Literal["gpu", "cpu", "remote", "ssd"]
    state: Literal["present", "evicted", "migrating", "loading"]
    # present: 数据已到位；evicted: 已被驱逐需重拉；migrating: 正在迁移到不同层级；loading: 正在从下层或远程读取
    # tier-specific location
    gpu_ptr: int | None         # L0: CUDA device pointer
    cpu_mapped: bool            # L1: 是否已 MAP_FIXED 映射
    remote_node: str | None     # L2: 远程节点 ID
    remote_mr: Any | None       # L2: RDMA MR 信息
    ssd_fd: int | None          # L3: 文件 fd
    ssd_offset: int | None      # L3: 文件偏移
```

---

## 2. 缺页处理：跨平台机制

缺页填充是 BlockStore 的内部实现细节。核心逻辑相同（fault_addr → block_id → 从下层拉取 → 填页 → 恢复用户线程），但底层机制因平台而异：

| | Linux userfaultfd | macOS Mach Exception | 降级：mprotect + SIGSEGV |
|---|---|---|---|
| **原理** | 内核将缺页投递到 fd，handler 线程 read fd 后用 UFFDIO_COPY 原子填页 | 缺页触发 EXC_BAD_ACCESS，内核通过 Mach port 投递到 handler 线程，handler 填数据后回复消息 | mprotect 设 PROT_NONE，访问触发 SIGSEGV/SIGBUS，signal handler 内填数据后改 mprotect |
| **handler 位置** | 独立线程（read fd） | 独立线程（Mach port receive） | 信号上下文（同线程，栈受限） |
| **填页方式** | `UFFDIO_COPY`（内核原子建立映射） | `memcpy` + `mprotect`（两步） | 先 `mprotect(RW)` 再 `memcpy`（两步） |
| **并发安全** | 内核保证（同一页的多个 fault 合并） | 需自行处理 | 需自行处理 |
| **适合场景** | 生产部署（Linux） | 生产部署（macOS） | 开发原型（跨平台） |

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

对上层的效果相同：用户线程的内存访问自动挂起、填页后自动恢复。`BlockMappedBacking.read(indices)` 在所有平台上都是 `self._arr[indices].copy()`，无需感知底层差异。

开发阶段可在 macOS 上用 Mach Exception 或 mprotect+SIGSEGV 做原型验证，生产部署用 Linux userfaultfd。

### 2.1 调度逻辑归属

核心调度逻辑应实现在缺页 handler 所在的事件循环里，避免「缺页路径」与「预取路径」两套决策分叉。

### 2.2 事件循环必须在 Rust 侧（GIL 死锁约束）

主事件循环必须在 Rust 侧实现。原因：UFFD 缺页时，触缺页的线程往往在 numpy/torch 等 C 扩展中仍持有 GIL 并被内核挂起；若填页逻辑在 Python 中执行，handler 会争用 GIL，导致死锁。

- **循环与填页热路径均在 Rust**：epoll/kqueue、预取队列消费、UFFD 读 fd 与 UFFDIO_COPY、block_read/填页，全部在 Rust 内完成，不回调 Python。
- **Python 仅做提交与等待**：`prefetch(region)` 通过 FFI 将 block 列表送入 Rust 侧队列；`wait(region)` 等待 Rust 侧完成事件。填页所需配置在启动循环时一次性从 Python 传入 Rust。

---

## 3. 预取与同步

预取是「提前向调度器提交填块任务」，与真实缺页走同一套 handler 逻辑：

1. **预取**：`prefetch(region)` 将 Block 列表提交给调度器，由事件循环执行拉取与填页。
2. **UFFD 保底**：若未预取或预取未完成，计算流访问时触发缺页，仍由同一 handler 按相同逻辑拉取并填页。
3. **显式等待**：`wait(region)` 阻塞直到该 region 对应块已完成填页。

---

## 4. 驱逐

由事件循环内的定时/周期任务或内存压力回调决策；对选中 Block 执行 `MADV_DONTNEED`、更新 block_table 为 evicted；下次访问触发缺页，由 handler 重新拉取。

---

## 5. 事件循环

管理线程用 **epoll**（Linux）或 **kqueue**（macOS）统一监听：

| 事件源 | 事件含义 | 处理 | 平台 |
|--------|---------|------|------|
| UFFD fd | CPU 缺页 | 查 block_table、选层、拉取、UFFDIO_COPY、更新状态 | Linux |
| 预取队列 | prefetch(region) 提交的待填块 | 与缺页同调度逻辑 | 通用 |
| Mach port | CPU 缺页 (EXC_BAD_ACCESS) | 同上，填页后 mach_msg_send 回复 | macOS |
| GPU eventfd | GPU 传输完成 | 更新 block_table（"migrating"→"present"） | 通用 |
| RDMA 完成通道 | 远程拉取完成 | 填页 + 更新 block_table | Linux |
| io_uring | SSD 读写完成 | 同上 | Linux |
| 定时器 | 周期任务 | 冷热扫描、驱逐决策 | 通用 |

### 5.1 事件管理与派发

#### 事件类型与来源

| 事件类型 | 来源 | 载荷 | 触发逻辑 |
|----------|------|------|----------|
| **FillRequest** | 预取队列 | block 列表 | 选层 → 拉取 → 填页 → 可选完成通知 |
| **PageFault** | UFFD fd / Mach port | fault_addr → block_id | 同上，填页后唤醒触缺页线程 |
| **IoComplete** | io_uring / RDMA 完成通道 / GPU eventfd | (op_id, 成功/失败) | 填页到目标 VA 或更新 block_table |
| **Timer** | 定时器 | 周期或单次 | 驱逐扫描、预取窗口推进 |

#### 优先级与顺序

- **PageFault 优先于 FillRequest**，避免触缺页的计算线程长时间挂起。
- 同一 block 的多次请求应**去重或合并**，避免重复 I/O。

#### 派发

- 循环内维护「事件类型 → 处理函数」映射，均为 Rust 内纯函数，不回调 Python。
- FillRequest / PageFault → 同一 `fill_blocks` 入口。

#### 完成通知

- 预取完成通过完成通道或共享状态通知 Python `wait()`。
- 缺页完成由内核/UFFD 机制保证，触缺页线程自动恢复。

#### 批量化

- 同 block 合并为一次 fill。
- 相邻 block 可合并为一次 read(offset, len)，按 block 切分填页。
- 单次派发 block 数设上限，避免阻塞其他事件源。

#### 背压与错误

- 预取队列与完成队列有界；超过时 Python submit 阻塞或返回错误。
- fill 失败标记该 block 并可选重试或向 Python 返回错误。

---

## 6. 实现注意

- **防递归缺页**：handler 线程的栈和堆不得落在 UFFD 管辖区间内，建议 `mlockall` 或从非托管区分配。
- **DMA 缓冲区**：RDMA、GPU 拷贝的中间缓冲须为固定内存（pinned / `cudaHostAlloc`）。
- **一致性**：GPU 与 CPU 同时访问同一 Block 时需协调（如先迁回再访问）。
